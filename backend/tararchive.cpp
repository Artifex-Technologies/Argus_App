#include "tararchive.h"

#include <QDir>
#include <QFileInfo>
#include <QDirIterator>

#include <QIODevice>
#include <QDateTime>

#include <QFutureWatcher>
#include <QtConcurrent/QtConcurrentRun>

#define BLOCK_SIZE 512
#define CHUNK_SIZE (4 * 1024 * 1024)

struct TarHeader
{
    char name[100];
    char mode[8];
    char owner[8];
    char group[8];
    char size[12];
    char mtime[12];
    char checksum[8];
    char typeflag;
    char linkname[100];
    char unused[255];
};

static_assert(sizeof(TarHeader) == BLOCK_SIZE, "Check TarHeader alignment");

static bool isMemZeros(char *p, size_t len)
{
    while(len--) {
        if(*(p++)) {
            return false;
        }
    }

    return true;
}

static uint32_t calculateChecksum(const TarHeader *header)
{
    uint32_t ret = 256;
    const auto *p = (unsigned char*)header;

    for(size_t i = 0; i < offsetof(TarHeader, checksum); ++i) {
        ret += p[i];
    }

    for(size_t i = offsetof(TarHeader, typeflag); i < sizeof(TarHeader); ++i) {
        ret += p[i];
    }

    return ret;
}

TarArchive::TarArchive(QIODevice *inputFile, QObject *parent):
    QObject(parent),
    m_tarFile(inputFile),
    m_root(new FileNode("", FileNode::Type::Directory))
{
    if(!m_tarFile->open(QIODevice::ReadOnly)) {
        setError(BackendError::DiskError, m_tarFile->errorString());
    } else {
        readTarFile();
    }
}

TarArchive::TarArchive(const QDir &inputDir, QIODevice *outputFile, QObject *parent):
    QObject(parent),
    m_tarFile(outputFile),
    m_root(new FileNode("", FileNode::Type::Directory))
{
    if(!m_tarFile->open(QIODevice::WriteOnly)) {
        setError(BackendError::DiskError, m_tarFile->errorString());
        return;
    }

    auto *watcher = new QFutureWatcher<void>(this);

    connect(watcher, &QFutureWatcherBase::finished, this, [=]() {
        watcher->deleteLater();
        emit ready();
    });

#if QT_VERSION < 0x060000
    watcher->setFuture(QtConcurrent::run(this, &TarArchive::assembleTarFile, inputDir));
#else
    watcher->setFuture(QtConcurrent::run(&TarArchive::assembleTarFile, this, inputDir));
#endif
}

FileNode *TarArchive::root() const
{
    return m_root.get();
}

FileNode *TarArchive::file(const QString &fullName)
{
    return m_root->find(fullName);
}

QByteArray TarArchive::fileData(const QString &fullName)
{
    if(!m_tarFile) {
        setError(BackendError::UnknownError, QStringLiteral("Archive file not set"));
        return QByteArray();

    } else if(!m_tarFile->isOpen()) {
        setError(BackendError::UnknownError, QStringLiteral("Archive file is not open"));
        return QByteArray();
    }

    auto *node = file(fullName);
    if(!node) {
        setError(BackendError::UnknownError, QStringLiteral("File not found"));
        return QByteArray();
    }

    if(!node->userData().canConvert<FileInfo>()) {
        setError(BackendError::DataError, QStringLiteral("No valid FileData found in the node."));
        return QByteArray();
    }

    const auto data = node->userData().value<FileInfo>();
    const auto success = m_tarFile->seek(data.offset);

    if(success) {
        return m_tarFile->read(data.size);

    } else {
        setError(BackendError::DiskError, m_tarFile->errorString());
        return QByteArray();
    }
}

// An octal header field: optional leading spaces, at least one digit, then only NULs or spaces
// up to the end of the field. A binary (base-256) field, or garbage, is refused (strtol used to
// read whatever it found, and read past a field with no NUL in it).
static bool parseOctal(const char *field, size_t len, qint64 &value)
{
    size_t i = 0;
    while(i < len && field[i] == ' ') {
        ++i;
    }

    const auto start = i;
    quint64 v = 0;

    for(; i < len && field[i] >= '0' && field[i] <= '7'; ++i) {
        if(v > (quint64(1) << 60)) {
            return false;
        }
        v = v * 8 + quint64(field[i] - '0');
    }

    if(i == start) {
        return false;
    }

    for(; i < len; ++i) {
        if(field[i] != 0 && field[i] != ' ') {
            return false;
        }
    }

    value = (qint64)v;
    return true;
}

// True when a name from an archive is safe to use below a folder: relative, every part not
// empty, not "." or "..", without '\\' or a control character. The same rule as the Rust port's
// flipper_rpc::restore.
static bool isSafeArchivePath(const QString &path)
{
    if(path.isEmpty()) {
        return false;
    }

    for(const auto &part : path.split(QLatin1Char('/'))) {
        if(part.isEmpty() || part == QStringLiteral(".") || part == QStringLiteral("..")) {
            return false;
        }

        for(const auto &c : part) {
            if(c == QLatin1Char('\\') || c.category() == QChar::Other_Control) {
                return false;
            }
        }
    }

    return true;
}

void TarArchive::readTarFile()
{
    // Every entry is checked before it is indexed, and the whole archive is refused at the first
    // problem (docs/RUST_PORT.md, "fixed in both"): a name with ".." parts, an absolute name, a
    // link or a cut archive used to be indexed, and an unpacked "int/../../x" landed outside the
    // target folder. Entries whose folder was missing were silently dropped.
    TarHeader header;
    int emptyCounter = 0;
    const auto archiveSize = m_tarFile->size();

    while(m_tarFile->pos() < archiveSize) {
        const auto n = m_tarFile->read((char*)&header, sizeof(TarHeader));

        if(n != sizeof(TarHeader)) {
            setError(BackendError::DataError, QStringLiteral("Archive file is truncated"));
            return;

        } else if(isMemZeros((char*)&header, sizeof(TarHeader))) {
            // The end is two zero blocks in a row. Archives from older Argus/qFlipper builds have a
            // single stray zero block after every file whose size is a multiple of 512; skip those.
            if(++emptyCounter == 2) {
               break;
            } else {
                continue;
            }
        }

        emptyCounter = 0;

        qint64 storedChecksum, fileSize;
        if(!parseOctal(header.checksum, sizeof(header.checksum), storedChecksum) || storedChecksum != (qint64)calculateChecksum(&header)) {
            setError(BackendError::DataError, QStringLiteral("Archive header checksum does not match"));
            return;
        }

        const auto nameLength = qstrnlen(header.name, sizeof(header.name));
        if(nameLength == sizeof(header.name)) {
            setError(BackendError::DataError, QStringLiteral("Archive entry name is not terminated"));
            return;
        }

        // The ustar prefix (bytes 345..499) would make the real name longer than the one read here.
        if(!isMemZeros(header.unused + 88, 155)) {
            setError(BackendError::DataError, QStringLiteral("Archive entry names with a prefix are not supported"));
            return;
        }

        if(!parseOctal(header.size, sizeof(header.size), fileSize)) {
            setError(BackendError::DataError, QStringLiteral("Archive entry size is not a number"));
            return;
        }

        const QByteArray rawName(header.name, (int)nameLength);
        const auto fileName = QString::fromUtf8(rawName);

        if(fileName.toUtf8() != rawName) {
            setError(BackendError::DataError, QStringLiteral("Archive entry name is not UTF-8"));
            return;
        }

        const auto isDirectory = header.typeflag == '5';

        if(header.typeflag != '0' && !isDirectory) {
            setError(BackendError::DataError, QStringLiteral("Only regular files and directories are supported"));
            return;
        } else if(isDirectory && (!fileName.endsWith(QLatin1Char('/')) || fileSize != 0)) {
            setError(BackendError::DataError, QStringLiteral("Malformed directory entry: %1").arg(fileName));
            return;
        } else if(!isDirectory && fileName.endsWith(QLatin1Char('/'))) {
            setError(BackendError::DataError, QStringLiteral("Malformed file entry: %1").arg(fileName));
            return;
        }

        const auto path = isDirectory ? fileName.chopped(1) : fileName;

        if(!isSafeArchivePath(path)) {
            setError(BackendError::DataError, QStringLiteral("Unsafe name in the archive: %1").arg(fileName.toHtmlEscaped()));
            return;
        } else if(m_root->find(path)) {
            setError(BackendError::DataError, QStringLiteral("Name is in the archive twice: %1").arg(fileName));
            return;
        }

        // Blocks are always padded to BLOCK_SIZE
        const auto padding = fileSize % BLOCK_SIZE ? BLOCK_SIZE - (fileSize % BLOCK_SIZE) : 0;
        const auto dataOffset = m_tarFile->pos();

        if(fileSize > archiveSize - dataOffset || fileSize + padding > archiveSize - dataOffset) {
            setError(BackendError::DataError, QStringLiteral("Archive file is truncated"));
            return;
        }

        bool added;

        if(isDirectory) {
            added = m_root->addDirectory(path);
        } else {
            FileInfo data;
            data.offset = dataOffset;
            data.size = fileSize;
            added = m_root->addFile(path, QVariant::fromValue(data));
        }

        if(!added) {
            setError(BackendError::DataError, QStringLiteral("Archive entry comes before its folder: %1").arg(fileName));
            return;
        }

        if(!m_tarFile->seek(dataOffset + fileSize + padding)) {
            setError(BackendError::DataError, QStringLiteral("Archive file is truncated"));
            return;
        }
    }
}

void TarArchive::assembleTarFile(const QDir &inputDir)
{
    TarHeader header = {};
    QDirIterator it(inputDir, QDirIterator::Subdirectories);

    while(it.hasNext()) {
        const QFileInfo fileInfo(it.next());
        const auto relativeFilePath = inputDir.relativeFilePath(fileInfo.absoluteFilePath()).toLocal8Bit();

        // The name field holds 99 bytes and a NUL (a folder's name ends in '/'); refuse a longer
        // name rather than store a truncated one.
        if(relativeFilePath.size() + (fileInfo.isDir() ? 1 : 0) > (int)sizeof(header.name) - 1) {
            setError(BackendError::DataError, QStringLiteral("Name too long for the archive: %1").arg(QString::fromLocal8Bit(relativeFilePath)));
            break;
        }

        header = {};

        if(fileInfo.isFile()) {
            snprintf(header.name, sizeof(header.name), "%s", relativeFilePath.data());
            snprintf(header.mode, sizeof(header.mode), "%07o", 0664);
            snprintf(header.size, sizeof(header.size), "%011o", (unsigned int)fileInfo.size());
            header.typeflag = '0';

        } else if(fileInfo.isDir()) {
            snprintf(header.name, sizeof(header.name), "%s/", relativeFilePath.data());
            snprintf(header.mode, sizeof(header.mode), "%07o", 0755);
            snprintf(header.size, sizeof(header.size), "%011o", 0);
            header.typeflag = '5';
        }

        snprintf(header.owner, sizeof(header.owner), "%07o", 1000);
        snprintf(header.group, sizeof(header.group), "%07o", 1000);

        snprintf(header.mtime, sizeof(header.mtime), "%011o", (unsigned int)fileInfo.metadataChangeTime().toSecsSinceEpoch());
        snprintf(header.checksum, sizeof(header.checksum), "%06o", calculateChecksum(&header));

        if(m_tarFile->write((const char*)&header, sizeof(TarHeader)) != sizeof(TarHeader)) {
            setError(BackendError::DiskError, m_tarFile->errorString());
            break;
        }

        if(fileInfo.isFile() && fileInfo.size() > 0) {
            QFile file(fileInfo.absoluteFilePath());

            if(!file.open(QIODevice::ReadOnly)) {
                setError(BackendError::DiskError, file.errorString());
                break;
            }

            qint64 bytesProcessed = 0;

            while(file.bytesAvailable() > 0) {
                const auto chunk = file.read(CHUNK_SIZE);
                if(chunk.isEmpty()) {
                    setError(BackendError::DiskError, QStringLiteral("Failed to read from file: %1").arg(file.errorString()));
                    break;
                }

                if(m_tarFile->write(chunk) != chunk.size()) {
                    setError(BackendError::DiskError, QStringLiteral("Failed to write to file: %1").arg(m_tarFile->errorString()));
                    break;
                }

                bytesProcessed += chunk.size();
            }

            if(bytesProcessed != fileInfo.size()) {
                break;
            }

            // Data is padded to a whole block; a file that already fills its blocks gets no padding
            // (older builds wrote a whole zero block there, which standard tar reads as the end).
            const auto padding = (BLOCK_SIZE - (fileInfo.size() % BLOCK_SIZE)) % BLOCK_SIZE;
            if(padding) {
                m_tarFile->write(QByteArray(padding, 0));
            }
        }
    }

    // End of archive: two zero blocks.
    if(!isError() && m_tarFile->write(QByteArray(2 * BLOCK_SIZE, 0)) != 2 * BLOCK_SIZE) {
        setError(BackendError::DiskError, m_tarFile->errorString());
    }

    m_tarFile->close();
}
