#include "chymaerabridge.h"

#include <QDir>
#include <QPair>
#include <QFile>
#include <QFileInfo>
#include <QDateTime>
#include <QJsonArray>
#include <QDirIterator>
#include <QSharedPointer>
#include <QCryptographicHash>
#include <QJsonDocument>
#include <QStandardPaths>
#include <QLoggingCategory>
#include <initializer_list>

#include "chymaera/datastore/chymaeradatastore.h"
#include "chymaera/sarina/sarinaserver.h"
#include "chymaera/exporters/flipperfileformat.h"
#include "chymaera/sync/capturesync.h"

#include "flipperzero/flipperzero.h"
#include "flipperzero/devicestate.h"
#include "flipperzero/protobufsession.h"

Q_LOGGING_CATEGORY(LOG_CHYMAERA, "CHYMAERA")

using namespace Chymaera;

Bridge::Bridge(QObject *parent):
    QObject(parent),
    m_datastore(new ChymaeraDatastore(this)),
    m_eventLog(new ChymaeraEventLog(this)),
    m_server(new SarinaServer(this))
{
    m_eventLog->setDatastore(m_datastore);

    if(m_datastore->open(defaultDatastorePath())) {
        emit datastoreChanged();
    }

    connect(m_datastore, &ChymaeraDatastore::sessionChanged, this, &Bridge::sessionChanged);
    connect(m_server, &SarinaServer::listeningChanged, this, &Bridge::serverStateChanged);
    connect(m_server, &SarinaServer::requestReceived, this, [this](const QString &cmd) {
        m_eventLog->append(Severity::Debug, Source::Sarina, QStringLiteral("API request: %1").arg(cmd));
    });

    m_server->setRequestHandler([this](const QJsonObject &req) {
        return handleApiRequest(req);
    });

    m_eventLog->append(Severity::Info, Source::System,
                       QStringLiteral("Chymaera subsystem %1 ready").arg(version()),
                       m_datastore->isOpen() ? QStringLiteral("datastore: %1").arg(m_datastore->path())
                                             : QStringLiteral("datastore unavailable"));
}

Bridge::~Bridge() = default;

ChymaeraEventLog *Bridge::eventLog() const
{
    return m_eventLog;
}

ChymaeraDatastore *Bridge::datastore() const
{
    return m_datastore;
}

bool Bridge::isServerRunning() const
{
    return m_server->isListening();
}

int Bridge::serverPort() const
{
    return m_server->port();
}

bool Bridge::isSessionActive() const
{
    return m_datastore->currentSessionId() >= 0;
}

QString Bridge::sessionName() const
{
    return m_datastore->currentSessionName();
}

QString Bridge::datastorePath() const
{
    return m_datastore->path();
}

QString Bridge::version() const
{
    return QStringLiteral("0.1.0");
}

QString Bridge::defaultDatastorePath()
{
    // One location for the GUI and the CLI alike (AppDataLocation differs per
    // executable), so captures pulled headlessly show up in the console and API.
    const QString base = QStandardPaths::writableLocation(QStandardPaths::GenericDataLocation);
    return QDir(base).filePath(QStringLiteral("Argus/chymaera/chymaera.db"));
}

void Bridge::log(const QString &message)
{
    m_eventLog->append(Severity::Info, Source::User, message);
}

void Bridge::logMessage(const QString &severity, const QString &message)
{
    m_eventLog->append(severityFromName(severity), Source::User, message);
}

void Bridge::clearLog()
{
    m_eventLog->clear();
}

bool Bridge::startSession(const QString &name, const QString &notes)
{
    const qint64 id = m_datastore->startSession(name, notes);
    if(id < 0) {
        m_eventLog->append(Severity::Error, Source::System,
                           QStringLiteral("Failed to start session '%1'").arg(name));
        return false;
    }
    m_eventLog->append(Severity::Notice, Source::System,
                       QStringLiteral("Session started: %1 (#%2)").arg(name).arg(id));
    return true;
}

void Bridge::endSession()
{
    if(!isSessionActive()) {
        return;
    }
    const QString name = sessionName();
    m_datastore->endSession();
    m_eventLog->append(Severity::Notice, Source::System,
                       QStringLiteral("Session ended: %1").arg(name));
}

bool Bridge::startServer(int port, const QString &token)
{
    m_server->setToken(token);
    const bool ok = m_server->start(static_cast<quint16>(port));
    if(ok) {
        m_eventLog->append(Severity::Notice, Source::Sarina,
                           QStringLiteral("Control API listening on 127.0.0.1:%1").arg(m_server->port()),
                           token.isEmpty() ? QStringLiteral("no token") : QStringLiteral("token required"));
    } else {
        m_eventLog->append(Severity::Error, Source::Sarina,
                           QStringLiteral("Control API failed to start on port %1").arg(port));
    }
    return ok;
}

void Bridge::stopServer()
{
    if(!m_server->isListening()) {
        return;
    }
    m_server->stop();
    m_eventLog->append(Severity::Notice, Source::Sarina, QStringLiteral("Control API stopped"));
}

// Kind for a file the Flipper-format parsers do not understand (pcap, csv...).
static QString rawKindForSuffix(const QString &suffix)
{
    if(suffix == QStringLiteral("pcap") || suffix == QStringLiteral("pcapng")) {
        return QStringLiteral("pcap");
    } else if(suffix == QStringLiteral("csv")) {
        return QStringLiteral("csv");
    } else if(suffix == QStringLiteral("log") || suffix == QStringLiteral("txt")) {
        return QStringLiteral("log");
    } else {
        return QStringLiteral("file");
    }
}

static QString sha256OfFile(const QString &path)
{
    QFile f(path);
    if(!f.open(QIODevice::ReadOnly)) {
        return QString();
    }

    QCryptographicHash hash(QCryptographicHash::Sha256);
    hash.addData(&f);
    return QString::fromLatin1(hash.result().toHex());
}

QString Bridge::recordCapture(const QString &parsePath, const QString &displayName, const QString &origin,
                              bool *duplicate)
{
    if(duplicate) {
        *duplicate = false;
    }

    const QFileInfo fi(parsePath);
    const QString sha256 = sha256OfFile(parsePath);
    if(sha256.isEmpty()) {
        m_eventLog->append(Severity::Warning, Source::Capture,
                           QStringLiteral("Cannot read capture: %1").arg(displayName));
        return QString();
    }

    if(m_datastore->hasCapture(sha256, origin)) {
        if(duplicate) {
            *duplicate = true;
        }
        return QString();
    }

    QVariantMap info = FlipperFiles::parseAuto(parsePath);
    if(info.isEmpty()) {
        // Not a Flipper-format file: keep it as evidence anyway, unparsed.
        info[QStringLiteral("kind")] = rawKindForSuffix(fi.suffix().toLower());
    }

    const QString kind = info.value(QStringLiteral("kind")).toString();

    QString summary;
    if(kind == QStringLiteral("nfc")) {
        summary = QStringLiteral("NFC %1 UID %2")
                      .arg(info.value(QStringLiteral("deviceType")).toString(),
                           info.value(QStringLiteral("uid")).toString());
    } else if(kind == QStringLiteral("subghz")) {
        summary = QStringLiteral("Sub-GHz %1 @ %2 (%3 samples)")
                      .arg(info.value(QStringLiteral("protocol")).toString(),
                           info.value(QStringLiteral("frequency")).toString(),
                           info.value(QStringLiteral("rawSampleCount")).toString());
    } else if(kind == QStringLiteral("generic")) {
        summary = QStringLiteral("%1 (%2)")
                      .arg(info.value(QStringLiteral("filetype")).toString(), displayName);
    } else {
        summary = QStringLiteral("%1 %2 (%3 bytes)").arg(kind, displayName).arg(fi.size());
    }

    const QByteArray metaJson = QJsonDocument(QJsonObject::fromVariantMap(info)).toJson(QJsonDocument::Compact);
    m_datastore->addCapture(kind, displayName, origin, fi.size(), QString::fromUtf8(metaJson),
                            sha256, fi.absoluteFilePath());
    m_eventLog->append(Severity::Info, Source::Capture,
                       QStringLiteral("Recorded capture: %1").arg(summary), origin);
    return summary;
}

QString Bridge::importCapture(const QString &path)
{
    bool duplicate = false;
    const QString summary = recordCapture(path, QFileInfo(path).fileName(), path, &duplicate);
    return duplicate ? QStringLiteral("Already recorded: %1").arg(QFileInfo(path).fileName()) : summary;
}

QString Bridge::importCaptureUrl(const QUrl &fileUrl)
{
    return importCapture(fileUrl.isLocalFile() ? fileUrl.toLocalFile() : fileUrl.toString());
}

// --- device integration (Phase 1 / 2) --------------------------------------

bool Bridge::isDeviceConnected() const
{
    return !m_device.isNull();
}

QString Bridge::deviceName() const
{
    if(m_device.isNull() || !m_device->deviceState()) {
        return QString();
    }
    return m_device->deviceState()->name();
}

void Bridge::attachDevice(Flipper::FlipperZero *device)
{
    if(m_device == device) {
        return;
    }

    detachDevice();

    m_device = device;
    if(m_device.isNull()) {
        emit deviceChanged();
        return;
    }

    auto *state = m_device->deviceState();
    auto *rpc = m_device->rpc();

    // Mirror the device lifecycle into the event log (read-only observation).
    if(state) {
        m_deviceConnections << connect(state, &Flipper::Zero::DeviceState::statusStringChanged, this, [this, state]() {
            m_eventLog->append(Severity::Info, Source::Device, state->statusString());
        });
        m_deviceConnections << connect(state, &Flipper::Zero::DeviceState::isOnlineChanged, this, [this, state]() {
            m_eventLog->append(Severity::Notice, Source::Device,
                               state->isOnline() ? QStringLiteral("Device online") : QStringLiteral("Device offline"));
        });
        m_deviceConnections << connect(state, &Flipper::Zero::DeviceState::isErrorChanged, this, [this, state]() {
            if(state->isError()) {
                m_eventLog->append(Severity::Error, Source::Device, state->errorString());
            }
        });
    }
    if(rpc) {
        m_deviceConnections << connect(rpc, &Flipper::Zero::ProtobufSession::sessionStateChanged, this, [this]() {
            m_eventLog->append(Severity::Debug, Source::Device, QStringLiteral("RPC session state changed"));
        });
    }

    m_eventLog->append(Severity::Notice, Source::Device,
                       QStringLiteral("Device attached: %1").arg(deviceName()));
    emit deviceChanged();
}

void Bridge::detachDevice()
{
    for(const auto &c : qAsConst(m_deviceConnections)) {
        disconnect(c);
    }
    m_deviceConnections.clear();

    if(!m_device.isNull()) {
        m_eventLog->append(Severity::Notice, Source::Device, QStringLiteral("Device detached"));
    }
    m_device = nullptr;
    emit deviceChanged();
}

bool Bridge::isPulling() const
{
    return !m_sync.isNull();
}

bool Bridge::pullPath(const QString &remotePath)
{
    return startPull({remotePath}, remotePath);
}

bool Bridge::pullAll()
{
    return startPull(CaptureSync::defaultRoots(), QStringLiteral("all capture folders"));
}

QStringList Bridge::pullAllRoots()
{
    return CaptureSync::defaultRoots();
}

bool Bridge::startPull(const QStringList &remoteRoots, const QString &label)
{
    if(m_device.isNull()) {
        m_eventLog->append(Severity::Warning, Source::Device,
                           QStringLiteral("Cannot pull %1: no device attached").arg(label));
        return false;
    }

    if(isPulling()) {
        m_eventLog->append(Severity::Warning, Source::Device,
                           QStringLiteral("Cannot pull %1: a pull is already running").arg(label));
        return false;
    }

    // Local staging directory next to the datastore; files keep their device paths below it.
    const QString base = QDir(QFileInfo(m_datastore->path()).absolutePath()).filePath(QStringLiteral("pulls"));
    const QString localDir = QDir(base).filePath(QDateTime::currentDateTime().toString(QStringLiteral("yyyyMMdd-hhmmss")));

    if(!QDir().mkpath(localDir)) {
        m_eventLog->append(Severity::Error, Source::Device,
                           QStringLiteral("Cannot create staging dir: %1").arg(localDir));
        return false;
    }

    m_eventLog->append(Severity::Notice, Source::Device,
                       QStringLiteral("Pulling %1 from device…").arg(label));
    emit pullStarted(label);

    auto *sync = new CaptureSync(m_device, remoteRoots, localDir, this);
    m_sync = sync;
    emit pullingChanged();

    auto newCount = QSharedPointer<int>::create(0);
    auto duplicateCount = QSharedPointer<int>::create(0);

    connect(sync, &CaptureSync::rootMissing, this, [this](const QString &root, const QString &reason) {
        m_eventLog->append(Severity::Info, Source::Device,
                           QStringLiteral("Skipped %1: not on the device or unreadable").arg(root), reason);
    });

    connect(sync, &CaptureSync::fileReceived, this, [this, newCount, duplicateCount](const QString &localPath, const QString &remotePath) {
        bool duplicate = false;
        const QString summary = recordCapture(localPath, QFileInfo(localPath).fileName(), remotePath, &duplicate);
        if(duplicate) {
            ++*duplicateCount;
            QFile::remove(localPath); // already kept from an earlier pull
        } else if(!summary.isEmpty()) {
            ++*newCount;
        }
    });

    connect(sync, &CaptureSync::fileFailed, this, [this](const QString &remotePath, const QString &reason) {
        m_eventLog->append(Severity::Warning, Source::Device,
                           QStringLiteral("Could not read %1").arg(remotePath), reason);
    });

    connect(sync, &CaptureSync::finished, this, [this, sync, label, localDir, newCount, duplicateCount](bool ok) {
        const auto severity = ok ? Severity::Notice : Severity::Warning;
        m_eventLog->append(severity, Source::Device,
                           QStringLiteral("Pulled %1: %2 new, %3 already stored, %4 failed (%5 bytes read)")
                               .arg(label).arg(*newCount).arg(*duplicateCount)
                               .arg(sync->failedCount()).arg(sync->byteCount()),
                           localDir);

        // Leave no empty staging folders behind when everything was a duplicate.
        QDirIterator it(localDir, QDir::Dirs | QDir::NoDotAndDotDot, QDirIterator::Subdirectories);
        QStringList dirs;
        while(it.hasNext()) {
            dirs.prepend(it.next());
        }
        for(const auto &dir : qAsConst(dirs)) {
            QDir().rmdir(dir);
        }
        QDir().rmdir(localDir);

        sync->deleteLater();
        emit pullingChanged();
        emit pullFinished(ok, *newCount);
    });

    sync->start();
    return true;
}

bool Bridge::exportEventLog(const QString &path)
{
    QFile f(path);
    if(!f.open(QIODevice::WriteOnly | QIODevice::Text)) {
        m_eventLog->append(Severity::Error, Source::Export,
                           QStringLiteral("Cannot write log to %1: %2").arg(path, f.errorString()));
        return false;
    }

    const QVariantList entries = m_eventLog->tail(m_eventLog->count());
    for(const QVariant &v : entries) {
        const QVariantMap e = v.toMap();
        const QString line = QStringLiteral("%1 [%2/%3] %4%5\n")
            .arg(e.value(QStringLiteral("timestamp")).toString(),
                 e.value(QStringLiteral("severity")).toString(),
                 e.value(QStringLiteral("source")).toString(),
                 e.value(QStringLiteral("message")).toString(),
                 e.value(QStringLiteral("detail")).toString().isEmpty()
                     ? QString()
                     : QStringLiteral(" — %1").arg(e.value(QStringLiteral("detail")).toString()));
        f.write(line.toUtf8());
    }

    f.close();
    m_eventLog->append(Severity::Info, Source::Export,
                       QStringLiteral("Exported %1 log entries").arg(entries.size()), path);
    return true;
}

QJsonObject Bridge::handleApiRequest(const QJsonObject &request)
{
    const QString cmd = request.value(QStringLiteral("cmd")).toString();

    auto ok = [](std::initializer_list<QPair<QString, QJsonValue>> extra) {
        QJsonObject o { {QStringLiteral("ok"), true} };
        for(const auto &kv : extra) {
            o.insert(kv.first, kv.second);
        }
        return o;
    };
    auto fail = [](const QString &error) {
        return QJsonObject { {QStringLiteral("ok"), false}, {QStringLiteral("error"), error} };
    };

    if(cmd == QStringLiteral("ping")) {
        return ok({ {QStringLiteral("pong"), true}, {QStringLiteral("version"), version()} });

    } else if(cmd == QStringLiteral("help")) {
        const QJsonArray cmds {
            QStringLiteral("ping"), QStringLiteral("status"), QStringLiteral("help"),
            QStringLiteral("log.tail"), QStringLiteral("log.add"),
            QStringLiteral("session.start"), QStringLiteral("session.end"),
            QStringLiteral("capture.list"), QStringLiteral("capture.import"),
            QStringLiteral("device.status"), QStringLiteral("capture.pull"),
            QStringLiteral("capture.pullAll")
        };
        return ok({ {QStringLiteral("commands"), cmds} });

    } else if(cmd == QStringLiteral("status")) {
        return ok({
            {QStringLiteral("serverRunning"), isServerRunning()},
            {QStringLiteral("serverPort"), serverPort()},
            {QStringLiteral("sessionActive"), isSessionActive()},
            {QStringLiteral("sessionName"), sessionName()},
            {QStringLiteral("datastorePath"), datastorePath()},
            {QStringLiteral("eventCount"), m_datastore->eventCount()},
            {QStringLiteral("version"), version()}
        });

    } else if(cmd == QStringLiteral("log.tail")) {
        const int limit = request.value(QStringLiteral("limit")).toInt(50);
        return ok({ {QStringLiteral("entries"), QJsonArray::fromVariantList(m_eventLog->tail(limit))} });

    } else if(cmd == QStringLiteral("log.add")) {
        const QString message = request.value(QStringLiteral("message")).toString();
        if(message.isEmpty()) {
            return fail(QStringLiteral("message is required"));
        }
        const Severity sev = severityFromName(request.value(QStringLiteral("severity")).toString(), Severity::Info);
        m_eventLog->append(sev, Source::Sarina, message);
        return ok({});

    } else if(cmd == QStringLiteral("session.start")) {
        const QString name = request.value(QStringLiteral("name")).toString();
        if(name.isEmpty()) {
            return fail(QStringLiteral("name is required"));
        }
        if(!startSession(name, request.value(QStringLiteral("notes")).toString())) {
            return fail(QStringLiteral("could not start session"));
        }
        return ok({ {QStringLiteral("sessionId"), static_cast<double>(m_datastore->currentSessionId())} });

    } else if(cmd == QStringLiteral("session.end")) {
        endSession();
        return ok({});

    } else if(cmd == QStringLiteral("capture.list")) {
        const int limit = request.value(QStringLiteral("limit")).toInt(50);
        return ok({ {QStringLiteral("captures"), QJsonArray::fromVariantList(m_datastore->captures(limit))} });

    } else if(cmd == QStringLiteral("capture.import")) {
        const QString path = request.value(QStringLiteral("path")).toString();
        if(path.isEmpty()) {
            return fail(QStringLiteral("path is required"));
        }
        const QString summary = importCapture(path);
        if(summary.isEmpty()) {
            return fail(QStringLiteral("could not parse capture"));
        }
        return ok({ {QStringLiteral("summary"), summary} });

    } else if(cmd == QStringLiteral("device.status")) {
        return ok({
            {QStringLiteral("connected"), isDeviceConnected()},
            {QStringLiteral("name"), deviceName()}
        });

    } else if(cmd == QStringLiteral("capture.pull")) {
        const QString path = request.value(QStringLiteral("path")).toString();
        if(path.isEmpty()) {
            return fail(QStringLiteral("path is required"));
        }
        if(!pullPath(path)) {
            return fail(QStringLiteral("no device attached or staging failed"));
        }
        // Asynchronous: watch log.tail / pullFinished for completion.
        return ok({ {QStringLiteral("started"), true}, {QStringLiteral("path"), path} });

    } else if(cmd == QStringLiteral("capture.pullAll")) {
        if(!pullAll()) {
            return fail(QStringLiteral("no device attached, a pull is already running, or staging failed"));
        }
        // Asynchronous, like capture.pull.
        return ok({ {QStringLiteral("started"), true},
                    {QStringLiteral("paths"), QJsonArray::fromStringList(pullAllRoots())} });
    }

    return fail(QStringLiteral("unknown command: %1").arg(cmd));
}
