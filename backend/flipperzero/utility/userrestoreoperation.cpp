#include "userrestoreoperation.h"

#include <QFile>
#include <QBuffer>

#include "flipperzero/devicestate.h"
#include "flipperzero/protobufsession.h"
#include "flipperzero/rpc/storagemkdiroperation.h"
#include "flipperzero/rpc/storagewriteoperation.h"

#include "getfiletreeoperation.h"
#include "tarziparchive.h"
#include "tararchive.h"

using namespace Flipper;
using namespace Zero;

// The restore checks the whole backup before anything is written, lists /int on the device,
// refuses a backup whose files are folders there (or the other way round), makes the missing
// folders and writes every file. Files on the device that are not in the backup stay.
//
// Until 2026-11-12 it unpacked the archive into a temporary folder without checking names (an
// entry "int/../../x" was written outside it; the folder's name came from the device), removed
// every path of the backup from the device without `recursive`, /int itself first, and then
// made /int: on current firmware /int is an alias of /ext/.int, so the first step failed while
// /int held anything and the second always did. Any top-level folder of the archive, not only
// int/, was removed and written on the device. docs/RUST_PORT.md has the details; the Rust port
// (flipper_rpc::restore) does the same as this.

UserRestoreOperation::UserRestoreOperation(ProtobufSession *rpc, DeviceState *deviceState, const QUrl &backupUrl, QObject *parent):
    AbstractUtilityOperation(rpc, deviceState, parent),
    m_backupUrl(backupUrl),
    m_remoteDirName(QByteArrayLiteral("/int")),
    m_archive(nullptr)
{}

const QString UserRestoreOperation::description() const
{
    return QStringLiteral("Restore %1 @%2").arg(m_remoteDirName, deviceState()->name());
}

void UserRestoreOperation::nextStateLogic()
{
    if(operationState() == Ready) {
        setOperationState(ReadingArchive);
        readArchive();

    } else if(operationState() == ReadingArchive) {
        setOperationState(ReadingDeviceTree);
        readDeviceTree();

    } else if(operationState() == ReadingDeviceTree) {
        setOperationState(WritingFiles);
        writeFiles();

    } else if(operationState() == WritingFiles) {
        finish();
    }
}

QString UserRestoreOperation::checkBackup(FileNode *root, FileNode::FileInfoList &entries)
{
    entries.clear();

    for(const auto &top : root->toPreOrderList()) {
        if(top.absolutePath.isEmpty()) {
            continue;
        } else if(!top.absolutePath.startsWith(QStringLiteral("int/")) && top.absolutePath != QStringLiteral("int")) {
            return QStringLiteral("Everything in a backup must be in its int/ folder");
        }
    }

    auto *intNode = root->find(QStringLiteral("int"));
    if(!intNode || intNode->type() != FileNode::Type::Directory) {
        return QStringLiteral("No int/ folder in the archive");
    }

    qint64 total = 0;
    int count = 0;

    for(const auto &info : intNode->toPreOrderList()) {
        if(++count > MaxEntries) {
            return QStringLiteral("More than %1 entries").arg(MaxEntries);
        }

        for(const auto &c : info.absolutePath) {
            if(c.unicode() > 0x7f) {
                return QStringLiteral("The device takes only ASCII names: %1").arg(info.absolutePath);
            }
        }

        if(info.type == FileNode::Type::RegularFile) {
            const auto size = info.userData.value<TarArchive::FileInfo>().size;

            if(size > MaxFileSize) {
                return QStringLiteral("%1 is %2 bytes, more than %3").arg(info.absolutePath).arg(size).arg(MaxFileSize);
            }

            total += size;

            if(total > MaxTotalSize) {
                return QStringLiteral("More than %1 bytes of files").arg(MaxTotalSize);
            }
        }

        if(info.absolutePath != QStringLiteral("int")) {
            entries.append(info);
        }
    }

    return QString();
}

void UserRestoreOperation::readArchive()
{
    deviceState()->setStatusString(tr("Checking backup..."));

    auto *file = new QFile(m_backupUrl.toLocalFile(), this);
    const QFileInfo fileInfo(*file);

    // A FIFO or a device would hang the read or never end.
    if(!fileInfo.isFile()) {
        finishWithError(BackendError::DiskError, QStringLiteral("Not a regular file: %1").arg(file->fileName()));
        return;
    }

    m_archive = new TarZipArchive(file, MaxTarSize, this);

    if(m_archive->isError()) {
        finishWithError(BackendError::DataError, m_archive->errorString());
        return;
    }

    connect(m_archive, &TarZipArchive::ready, this, [=]() {
        if(m_archive->isError() || !m_archive->archiveIndex()) {
            finishWithError(BackendError::DataError, QStringLiteral("Backup refused: %1").arg(m_archive->errorString()));
            return;
        }

        const auto why = checkBackup(m_archive->archiveIndex()->root(), m_entries);

        if(!why.isEmpty()) {
            finishWithError(BackendError::DataError, QStringLiteral("Backup refused: %1").arg(why));
        } else {
            advanceOperationState();
        }
    });
}

void UserRestoreOperation::readDeviceTree()
{
    auto *operation = new GetFileTreeOperation(rpc(), deviceState(), m_remoteDirName, this);

    connect(operation, &AbstractOperation::finished, this, [=]() {
        if(operation->isError()) {
            finishWithError(BackendError::OperationError, operation->errorString());
            operation->deleteLater();
            return;
        }

        for(const auto &info : operation->files()) {
            m_deviceTree.insert(info.absolutePath, info.type == FileType::Directory);
        }

        operation->deleteLater();

        // A file in the backup that is a folder on the device, or the other way round, cannot be
        // restored: refuse it before anything is written.
        for(const auto &entry : qAsConst(m_entries)) {
            const auto path = QByteArrayLiteral("/") + entry.absolutePath.toLatin1();
            const auto isDir = entry.type == FileNode::Type::Directory;

            if(m_deviceTree.contains(path) && m_deviceTree.value(path) != isDir) {
                finishWithError(BackendError::OperationError, QStringLiteral("%1 is a %2 on the device and a %3 in the backup; nothing was written")
                                .arg(QString(path), isDir ? QStringLiteral("file") : QStringLiteral("folder"), isDir ? QStringLiteral("folder") : QStringLiteral("file")));
                return;
            }
        }

        advanceOperationState();
    });

    operation->start();
}

void UserRestoreOperation::writeFiles()
{
    deviceState()->setStatusString(tr("Restoring backup..."));

    QList<AbstractOperation*> operations;

    for(const auto &entry : qAsConst(m_entries)) {
        const auto path = QByteArrayLiteral("/") + entry.absolutePath.toLatin1();

        if(entry.type == FileNode::Type::Directory) {
            if(!m_deviceTree.contains(path)) {
                operations.append(rpc()->storageMkdir(path));
            }

        } else {
            const auto data = m_archive->archiveIndex()->fileData(entry.absolutePath);
            const auto expected = entry.userData.value<TarArchive::FileInfo>().size;

            if(data.size() != expected) {
                finishWithError(BackendError::DataError, QStringLiteral("Cannot read %1 from the backup").arg(entry.absolutePath));
                return;
            }

            auto *buffer = new QBuffer(this);
            buffer->setData(data);

            auto *op = rpc()->storageWrite(path, buffer);
            connect(op, &AbstractOperation::finished, buffer, &QObject::deleteLater);
            operations.append(op);
        }
    }

    if(operations.isEmpty()) {
        advanceOperationState();
        return;
    }

    auto remaining = operations.size();

    for(auto *op : qAsConst(operations)) {
        const auto isLast = (--remaining == 0);

        connect(op, &AbstractOperation::finished, this, [=]() {
            if(op->isError()) {
                finishWithError(BackendError::OperationError, op->errorString());
            } else if(isLast) {
                advanceOperationState();
            }

            op->deleteLater();
        });
    }
}
