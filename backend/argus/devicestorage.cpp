#include "devicestorage.h"

#include <QDir>
#include <QFile>
#include <QTimer>
#include <QFileInfo>
#include <QDirIterator>
#include <QCryptographicHash>

#include "flipperzero/flipperzero.h"
#include "flipperzero/devicestate.h"
#include "flipperzero/protobufsession.h"
#include "flipperzero/utilityinterface.h"
#include "flipperzero/rpc/storagestatoperation.h"
#include "flipperzero/rpc/storagereadoperation.h"
#include "flipperzero/rpc/storagewriteoperation.h"
#include "flipperzero/rpc/storagemd5sumoperation.h"
#include "flipperzero/rpc/propertygetoperation.h"
#include "flipperzero/utility/getfiletreeoperation.h"
#include "flipperzero/utility/pathcreateoperation.h"

using namespace Argus;

bool Argus::isSafeDevicePath(const QString &path)
{
    if(!(path == QLatin1String("/ext") || path.startsWith(QLatin1String("/ext/")))) {
        return false;
    }
    for(const auto &c : path) {
        if(c.category() == QChar::Other_Control || c == QLatin1Char('\\')) {
            return false;
        }
    }
    const auto parts = path.split(QLatin1Char('/')).mid(1);
    for(const auto &part : parts) {
        if(part.isEmpty() || part == QLatin1String(".") || part == QLatin1String("..")) {
            return false;
        }
    }
    return true;
}

// ---- LocalDeviceStorage ---------------------------------------------------------------------

LocalDeviceStorage::LocalDeviceStorage(const QString &rootDir, const QString &api, QObject *parent):
    DeviceStorage(parent),
    m_root(QDir(rootDir).absolutePath()),
    m_api(api)
{}

QString LocalDeviceStorage::name() const
{
    return QStringLiteral("folder %1").arg(m_root);
}

QString LocalDeviceStorage::localPath(const QString &remote) const
{
    if(!isSafeDevicePath(remote)) {
        return QString();
    }
    return QDir::cleanPath(m_root + remote);
}

void LocalDeviceStorage::listTree(const QString &root, ListDone done)
{
    const auto local = localPath(root);
    QTimer::singleShot(0, this, [=, this]() {
        if(local.isEmpty() || !QFileInfo(local).isDir()) {
            done(false, QStringLiteral("no such folder: %1").arg(root), {});
            return;
        }
        QList<RemoteEntry> entries;
        QDirIterator it(local, QDir::AllEntries | QDir::NoDotAndDotDot | QDir::Hidden | QDir::System, QDirIterator::Subdirectories);
        while(it.hasNext()) {
            const auto p = it.next();
            const QFileInfo fi(p);
            if(fi.isSymLink()) {
                continue;
            }
            entries.append({root + p.mid(local.size()), fi.isDir() ? 0 : fi.size(), fi.isDir()});
        }
        done(true, QString(), entries);
    });
}

void LocalDeviceStorage::read(const QString &remote, const QString &localPathOut, Done done)
{
    const auto src = localPath(remote);
    QTimer::singleShot(0, this, [=, this]() {
        if(src.isEmpty() || !QFileInfo(src).isFile()) {
            done(false, QStringLiteral("no such file: %1").arg(remote));
            return;
        }
        QFile::remove(localPathOut);
        done(QFile::copy(src, localPathOut), QStringLiteral("copy failed"));
    });
}

void LocalDeviceStorage::write(const QString &localPathIn, const QString &remote, Done done)
{
    const auto dst = localPath(remote);
    QTimer::singleShot(0, this, [=, this]() {
        if(dst.isEmpty() || !QFileInfo(QFileInfo(dst).absolutePath()).isDir()) {
            done(false, QStringLiteral("no folder for %1").arg(remote));
            return;
        }
        QFile::remove(dst);
        done(QFile::copy(localPathIn, dst), QStringLiteral("write failed"));
    });
}

void LocalDeviceStorage::stat(const QString &remote, StatDone done)
{
    const auto p = localPath(remote);
    QTimer::singleShot(0, this, [=, this]() {
        const QFileInfo fi(p);
        done(!p.isEmpty() && fi.exists(), fi.isDir(), fi.isFile() ? fi.size() : 0);
    });
}

void LocalDeviceStorage::md5(const QString &remote, Md5Done done)
{
    const auto p = localPath(remote);
    QTimer::singleShot(0, this, [=, this]() {
        QFile f(p);
        if(p.isEmpty() || !f.open(QIODevice::ReadOnly)) {
            done(false, QString());
            return;
        }
        QCryptographicHash h(QCryptographicHash::Md5);
        h.addData(&f);
        done(true, QString::fromLatin1(h.result().toHex()));
    });
}

void LocalDeviceStorage::mkpath(const QString &remote, Done done)
{
    const auto p = localPath(remote);
    QTimer::singleShot(0, this, [=, this]() {
        done(!p.isEmpty() && QDir().mkpath(p), QStringLiteral("mkdir failed"));
    });
}

void LocalDeviceStorage::firmwareApi(ApiDone done)
{
    QTimer::singleShot(0, this, [=, this]() { done(m_api); });
}

// ---- RpcDeviceStorage -----------------------------------------------------------------------

using namespace Flipper;
using namespace Flipper::Zero;

RpcDeviceStorage::RpcDeviceStorage(FlipperZero *device, QObject *parent):
    DeviceStorage(parent),
    m_device(device)
{}

QString RpcDeviceStorage::name() const
{
    return m_device.isNull() ? QStringLiteral("no device") : QStringLiteral("Flipper");
}

static void later(QObject *ctx, std::function<void()> f)
{
    QTimer::singleShot(0, ctx, f);
}

void RpcDeviceStorage::listTree(const QString &root, ListDone done)
{
    if(m_device.isNull() || !isSafeDevicePath(root)) {
        later(this, [=, this]() { done(false, QStringLiteral("no device or bad path"), {}); });
        return;
    }
    auto *op = new GetFileTreeOperation(m_device->rpc(), m_device->deviceState(), root.toUtf8(), this);
    connect(op, &AbstractOperation::finished, this, [=, this]() {
        QList<RemoteEntry> entries;
        if(!op->isError()) {
            for(const auto &fi : op->files()) {
                const auto path = QString::fromUtf8(fi.absolutePath);
                if(path == root || (fi.type != FileType::RegularFile && fi.type != FileType::Directory)) {
                    continue;
                }
                entries.append({path, fi.size, fi.type == FileType::Directory});
            }
        }
        done(!op->isError(), op->errorString(), entries);
        op->deleteLater();
    });
    op->start();
}

void RpcDeviceStorage::read(const QString &remote, const QString &localPath, Done done)
{
    if(m_device.isNull() || !isSafeDevicePath(remote)) {
        later(this, [=, this]() { done(false, QStringLiteral("no device or bad path")); });
        return;
    }
    auto *file = new QFile(localPath, this);
    auto *op = m_device->rpc()->storageRead(remote.toUtf8(), file);
    connect(op, &AbstractOperation::finished, this, [=, this]() {
        file->close();
        done(!op->isError(), op->errorString());
        file->deleteLater();
    });
}

void RpcDeviceStorage::write(const QString &localPath, const QString &remote, Done done)
{
    if(m_device.isNull() || !isSafeDevicePath(remote)) {
        later(this, [=, this]() { done(false, QStringLiteral("no device or bad path")); });
        return;
    }
    auto *file = new QFile(localPath, this);
    auto *op = m_device->rpc()->storageWrite(remote.toUtf8(), file);
    connect(op, &AbstractOperation::finished, this, [=, this]() {
        file->close();
        done(!op->isError(), op->errorString());
        file->deleteLater();
    });
}

void RpcDeviceStorage::stat(const QString &remote, StatDone done)
{
    if(m_device.isNull() || !isSafeDevicePath(remote)) {
        later(this, [=, this]() { done(false, false, 0); });
        return;
    }
    auto *op = m_device->rpc()->storageStat(remote.toUtf8());
    connect(op, &AbstractOperation::finished, this, [=, this]() {
        const bool exists = !op->isError() && op->hasFile();
        done(exists, exists && op->type() == StorageStatOperation::Directory, exists ? (qint64)op->size() : 0);
    });
}

void RpcDeviceStorage::md5(const QString &remote, Md5Done done)
{
    if(m_device.isNull() || !isSafeDevicePath(remote)) {
        later(this, [=, this]() { done(false, QString()); });
        return;
    }
    auto *op = m_device->rpc()->storageMd5Sum(remote.toUtf8());
    connect(op, &AbstractOperation::finished, this, [=, this]() {
        done(!op->isError(), QString::fromLatin1(op->md5Sum()).toLower());
    });
}

void RpcDeviceStorage::mkpath(const QString &remote, Done done)
{
    if(m_device.isNull() || !isSafeDevicePath(remote)) {
        later(this, [=, this]() { done(false, QStringLiteral("no device or bad path")); });
        return;
    }
    auto *op = m_device->utility()->createPath(remote.toUtf8());
    connect(op, &AbstractOperation::finished, this, [=, this]() {
        done(!op->isError(), op->errorString());
    });
}

void RpcDeviceStorage::firmwareApi(ApiDone done)
{
    if(m_device.isNull()) {
        later(this, [=, this]() { done(QString()); });
        return;
    }
    // qFlipper's DeviceInfo does not keep the API version; the devinfo property has it.
    auto *op = m_device->rpc()->propertyGet(QByteArrayLiteral("devinfo"));
    connect(op, &AbstractOperation::finished, this, [=, this]() {
        if(op->isError()) {
            done(QString());
            return;
        }
        const auto major = op->value(QByteArrayLiteral("firmware.api.major"));
        const auto minor = op->value(QByteArrayLiteral("firmware.api.minor"));
        const auto target = op->value(QByteArrayLiteral("hardware.target"));
        if(major.isEmpty() || minor.isEmpty()) {
            done(QString());
            return;
        }
        done(QStringLiteral("%1.%2/%3").arg(QString::fromLatin1(major), QString::fromLatin1(minor),
                                            target.isEmpty() ? QStringLiteral("7") : QString::fromLatin1(target)));
    });
}
