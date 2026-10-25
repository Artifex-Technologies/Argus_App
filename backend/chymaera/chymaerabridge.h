#pragma once

#include <QUrl>
#include <QVector>
#include <QStringList>
#include <QObject>
#include <QString>
#include <QPointer>
#include <QJsonObject>

#include "chymaera/eventlog/chymaeraeventlog.h"

namespace Flipper {
class FlipperZero;
}

namespace Chymaera {

class ChymaeraDatastore;
class SarinaServer;
class CaptureSync;

/* The Chymaera façade: the single object the GUI (as the "Chymaera" QML
 * singleton) and headless callers talk to. It owns and wires together the
 * datastore, the live event log, and the Sarina control API, and it implements
 * the control-API command handler.
 *
 * Everything here is pure QtCore/Sql/Network, so the object lives happily
 * inside the (GUI-less) backend static library. */

class Bridge : public QObject
{
    Q_OBJECT
    Q_PROPERTY(Chymaera::ChymaeraEventLog* eventLog READ eventLog CONSTANT)
    Q_PROPERTY(bool serverRunning READ isServerRunning NOTIFY serverStateChanged)
    Q_PROPERTY(int serverPort READ serverPort NOTIFY serverStateChanged)
    Q_PROPERTY(bool sessionActive READ isSessionActive NOTIFY sessionChanged)
    Q_PROPERTY(QString sessionName READ sessionName NOTIFY sessionChanged)
    Q_PROPERTY(QString datastorePath READ datastorePath NOTIFY datastoreChanged)
    Q_PROPERTY(QString version READ version CONSTANT)
    Q_PROPERTY(bool deviceConnected READ isDeviceConnected NOTIFY deviceChanged)
    Q_PROPERTY(QString deviceName READ deviceName NOTIFY deviceChanged)
    Q_PROPERTY(bool pulling READ isPulling NOTIFY pullingChanged)

public:
    explicit Bridge(QObject *parent = nullptr);
    ~Bridge() override;

    ChymaeraEventLog *eventLog() const;
    ChymaeraDatastore *datastore() const;

    bool isServerRunning() const;
    int serverPort() const;
    bool isSessionActive() const;
    QString sessionName() const;
    QString datastorePath() const;
    QString version() const;

    bool isDeviceConnected() const;
    QString deviceName() const;
    bool isPulling() const;

    /* Device integration (Roadmap Phase 1/2). Observe a connected Flipper's
     * lifecycle into the event log, and pull files off it into the datastore.
     * Read-only: nothing here transmits, flashes, or mutates device state. */
    void attachDevice(Flipper::FlipperZero *device);
    void detachDevice();

    // Default on-disk location of the datastore (under the app data dir).
    static QString defaultDatastorePath();

    /* Actions — callable from QML and from C++ (CLI). */

    Q_INVOKABLE void log(const QString &message);
    Q_INVOKABLE void logMessage(const QString &severity, const QString &message);
    Q_INVOKABLE void clearLog();

    Q_INVOKABLE bool startSession(const QString &name, const QString &notes = QString());
    Q_INVOKABLE void endSession();

    // Start/stop the loopback control API. token is optional (empty = no auth).
    Q_INVOKABLE bool startServer(int port = 44700, const QString &token = QString());
    Q_INVOKABLE void stopServer();

    // Parse a Flipper capture file (.nfc/.sub/...) and record it. Returns a
    // short human-readable summary (empty on failure).
    Q_INVOKABLE QString importCapture(const QString &path);
    Q_INVOKABLE QString importCaptureUrl(const QUrl &fileUrl);

    // Dump the current in-memory event log to a text file.
    Q_INVOKABLE bool exportEventLog(const QString &path);

    // Pull a device directory (e.g. "/ext/nfc") off the attached Flipper into
    // the datastore, parsing any captures found. Read-only. Returns false if no
    // device is attached; completion is reported via pullFinished().
    Q_INVOKABLE bool pullPath(const QString &remotePath);

    // Pull every known capture folder (CaptureSync::defaultRoots()) in one go.
    // Folders missing on the device are skipped; files already stored (same
    // SHA-256 from the same device path) are not stored again. Read-only.
    Q_INVOKABLE bool pullAll();
    static QStringList pullAllRoots();

    // The control-API command dispatcher (also reused by the CLI/tests).
    QJsonObject handleApiRequest(const QJsonObject &request);

signals:
    void serverStateChanged();
    void sessionChanged();
    void datastoreChanged();
    void deviceChanged();
    void pullStarted(const QString &remotePath);
    void pullFinished(bool ok, int fileCount);
    void pullingChanged();

private:
    QString recordCapture(const QString &parsePath, const QString &displayName, const QString &origin,
                          bool *duplicate = nullptr);
    bool startPull(const QStringList &remoteRoots, const QString &label);

    ChymaeraDatastore *m_datastore;
    ChymaeraEventLog *m_eventLog;
    SarinaServer *m_server;

    QPointer<Flipper::FlipperZero> m_device;
    QPointer<CaptureSync> m_sync;
    QVector<QMetaObject::Connection> m_deviceConnections;
};

}
