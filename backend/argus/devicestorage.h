#pragma once

#include <functional>

#include <QList>
#include <QObject>
#include <QString>
#include <QPointer>

namespace Flipper {
class FlipperZero;
}

namespace Argus {

struct RemoteEntry {
    QString path;
    qint64 size;
    bool isDir;
};

/* The SD card as Argus's features see it: list, read, write, stat, MD5 and mkdir, all
 * asynchronous. RpcDeviceStorage is the real one (qFlipper's RPC storage operations);
 * LocalDeviceStorage is a folder that plays the card, for tests and for trying the features
 * without a Flipper (`<folder>/ext/...`).
 *
 * Callbacks are always called later from the event loop, never from inside the call. */
class DeviceStorage : public QObject
{
    Q_OBJECT

public:
    using Done = std::function<void(bool ok, const QString &error)>;
    using ListDone = std::function<void(bool ok, const QString &error, const QList<RemoteEntry> &entries)>;
    using StatDone = std::function<void(bool exists, bool isDir, qint64 size)>;
    using Md5Done = std::function<void(bool ok, const QString &md5)>;
    using ApiDone = std::function<void(const QString &api)>;

    using QObject::QObject;

    virtual QString name() const = 0;
    // Every file and folder below root (root itself excluded). ok=false if root cannot be listed.
    virtual void listTree(const QString &root, ListDone done) = 0;
    virtual void read(const QString &remote, const QString &localPath, Done done) = 0;
    virtual void write(const QString &localPath, const QString &remote, Done done) = 0;
    virtual void stat(const QString &remote, StatDone done) = 0;
    virtual void md5(const QString &remote, Md5Done done) = 0;
    // Create the folder and its parents.
    virtual void mkpath(const QString &remote, Done done) = 0;
    // "MAJOR.MINOR/TARGET" (e.g. "88.2/7"), or empty if it cannot be read.
    virtual void firmwareApi(ApiDone done) = 0;
};

/* A device path Argus will touch: absolute, under /ext, no "."/".." parts, no backslash or control characters. */
bool isSafeDevicePath(const QString &path);

class LocalDeviceStorage : public DeviceStorage
{
    Q_OBJECT

public:
    LocalDeviceStorage(const QString &rootDir, const QString &api = QStringLiteral("88.2/7"), QObject *parent = nullptr);

    QString name() const override;
    void listTree(const QString &root, ListDone done) override;
    void read(const QString &remote, const QString &localPath, Done done) override;
    void write(const QString &localPath, const QString &remote, Done done) override;
    void stat(const QString &remote, StatDone done) override;
    void md5(const QString &remote, Md5Done done) override;
    void mkpath(const QString &remote, Done done) override;
    void firmwareApi(ApiDone done) override;

    // Where a device path lives in the folder (empty if the path is not safe).
    QString localPath(const QString &remote) const;

private:
    QString m_root;
    QString m_api;
};

class RpcDeviceStorage : public DeviceStorage
{
    Q_OBJECT

public:
    RpcDeviceStorage(Flipper::FlipperZero *device, QObject *parent = nullptr);

    QString name() const override;
    void listTree(const QString &root, ListDone done) override;
    void read(const QString &remote, const QString &localPath, Done done) override;
    void write(const QString &localPath, const QString &remote, Done done) override;
    void stat(const QString &remote, StatDone done) override;
    void md5(const QString &remote, Md5Done done) override;
    void mkpath(const QString &remote, Done done) override;
    void firmwareApi(ApiDone done) override;

private:
    QPointer<Flipper::FlipperZero> m_device;
};

}
