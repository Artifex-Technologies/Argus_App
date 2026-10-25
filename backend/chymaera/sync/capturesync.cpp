#include "capturesync.h"

#include <QDir>
#include <QFile>
#include <QFileInfo>

#include "flipperzero/flipperzero.h"
#include "flipperzero/protobufsession.h"
#include "flipperzero/rpc/storagereadoperation.h"
#include "flipperzero/utility/getfiletreeoperation.h"

using namespace Chymaera;
using namespace Flipper;
using namespace Zero;

CaptureSync::CaptureSync(FlipperZero *device, const QStringList &remoteRoots,
                         const QString &stagingDir, QObject *parent):
    QObject(parent),
    m_device(device),
    m_roots(remoteRoots),
    m_stagingDir(stagingDir),
    m_fileCount(0),
    m_failedCount(0),
    m_missingRootCount(0),
    m_byteCount(0),
    m_finished(false)
{}

QStringList CaptureSync::defaultRoots()
{
    return {
        // Stock apps' saved captures
        QStringLiteral("/ext/nfc"),
        QStringLiteral("/ext/subghz"),
        QStringLiteral("/ext/lfrfid"),
        QStringLiteral("/ext/infrared"),
        QStringLiteral("/ext/ibutton"),
        // Wi-Fi devboard (Marauder companion app)
        QStringLiteral("/ext/apps_data/marauder/pcaps"),
        QStringLiteral("/ext/apps_data/marauder/logs"),
        // Argus firmware apps
        QStringLiteral("/ext/rf_monitor"),
        QStringLiteral("/ext/wifi_scan"),
        QStringLiteral("/ext/ld2450")
    };
}

bool CaptureSync::isExcluded(const QByteArray &remotePath)
{
    static const QList<QByteArray> excludedDirs = {
        QByteArrayLiteral("assets"), // key dictionaries, keeloq manufacturer keys
        QByteArrayLiteral(".cache")
    };

    const auto segments = remotePath.split('/');
    for(const auto &segment : segments) {
        if(excludedDirs.contains(segment)) {
            return true;
        }
    }

    return false;
}

void CaptureSync::start()
{
    if(m_device.isNull()) {
        finish(false);
        return;
    }

    listNextRoot();
}

QString CaptureSync::stagingDir() const
{
    return m_stagingDir;
}

int CaptureSync::fileCount() const
{
    return m_fileCount;
}

int CaptureSync::failedCount() const
{
    return m_failedCount;
}

int CaptureSync::missingRootCount() const
{
    return m_missingRootCount;
}

qint64 CaptureSync::byteCount() const
{
    return m_byteCount;
}

void CaptureSync::listNextRoot()
{
    if(m_device.isNull()) {
        finish(false);
        return;
    }

    if(m_roots.isEmpty()) {
        readNextFile();
        return;
    }

    const auto root = m_roots.takeFirst();
    auto *operation = new GetFileTreeOperation(m_device->rpc(), m_device->deviceState(), root.toUtf8(), this);

    connect(operation, &AbstractOperation::finished, this, [=]() {
        if(operation->isError()) {
            ++m_missingRootCount;
            emit rootMissing(root, operation->errorString());

        } else {
            for(const auto &fileInfo : operation->files()) {
                if(fileInfo.type == FileType::RegularFile && !isExcluded(fileInfo.absolutePath)) {
                    m_queue.append(fileInfo);
                }
            }
        }

        operation->deleteLater();
        listNextRoot();
    });

    operation->start();
}

void CaptureSync::readNextFile()
{
    if(m_device.isNull()) {
        finish(false);
        return;
    }

    if(m_queue.isEmpty()) {
        finish(m_failedCount == 0);
        return;
    }

    const auto fileInfo = m_queue.takeFirst();
    const auto remotePath = QString::fromUtf8(fileInfo.absolutePath);
    const auto localPath = QDir(m_stagingDir).filePath(remotePath.mid(1));

    if(!QDir().mkpath(QFileInfo(localPath).absolutePath())) {
        ++m_failedCount;
        emit fileFailed(remotePath, QStringLiteral("cannot create local directory"));
        readNextFile();
        return;
    }

    auto *file = new QFile(localPath, this);
    auto *operation = m_device->rpc()->storageRead(fileInfo.absolutePath, file);

    connect(operation, &AbstractOperation::finished, this, [=]() {
        if(operation->isError()) {
            ++m_failedCount;
            emit fileFailed(remotePath, operation->errorString());
        } else {
            ++m_fileCount;
            m_byteCount += fileInfo.size;
            emit fileReceived(localPath, remotePath);
        }

        file->deleteLater();
        readNextFile();
    });
}

void CaptureSync::finish(bool ok)
{
    if(m_finished) {
        return;
    }

    m_finished = true;
    emit finished(ok);
}
