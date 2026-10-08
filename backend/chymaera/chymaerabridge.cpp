#include "chymaerabridge.h"

#include <QDir>
#include <QPair>
#include <QFile>
#include <QFileInfo>
#include <QDateTime>
#include <QJsonArray>
#include <QDirIterator>
#include <QJsonDocument>
#include <QStandardPaths>
#include <QLoggingCategory>
#include <initializer_list>

#include "chymaera/datastore/chymaeradatastore.h"
#include "chymaera/sarina/sarinaserver.h"
#include "chymaera/exporters/flipperfileformat.h"

#include "flipperzero/flipperzero.h"
#include "flipperzero/devicestate.h"
#include "flipperzero/protobufsession.h"
#include "flipperzero/utilityinterface.h"
#include "abstractoperation.h"
#include "flipperzero/utility/directorydownloadoperation.h"

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
    const QString base = QStandardPaths::writableLocation(QStandardPaths::AppDataLocation);
    return QDir(base).filePath(QStringLiteral("chymaera/chymaera.db"));
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

QString Bridge::recordCapture(const QString &parsePath, const QString &displayName, const QString &origin)
{
    const QVariantMap info = FlipperFiles::parseAuto(parsePath);
    if(info.isEmpty()) {
        m_eventLog->append(Severity::Warning, Source::Capture,
                           QStringLiteral("Could not parse capture: %1").arg(displayName));
        return QString();
    }

    const QFileInfo fi(parsePath);
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
    } else {
        summary = QStringLiteral("%1 (%2)")
                      .arg(info.value(QStringLiteral("filetype")).toString(), displayName);
    }

    const QByteArray metaJson = QJsonDocument(QJsonObject::fromVariantMap(info)).toJson(QJsonDocument::Compact);
    m_datastore->addCapture(kind, displayName, origin, fi.size(), QString::fromUtf8(metaJson));
    m_eventLog->append(Severity::Info, Source::Capture,
                       QStringLiteral("Recorded capture: %1").arg(summary), origin);
    return summary;
}

QString Bridge::importCapture(const QString &path)
{
    return recordCapture(path, QFileInfo(path).fileName(), path);
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

bool Bridge::pullPath(const QString &remotePath)
{
    if(m_device.isNull()) {
        m_eventLog->append(Severity::Warning, Source::Device,
                           QStringLiteral("Cannot pull %1: no device attached").arg(remotePath));
        return false;
    }

    // Local staging directory next to the datastore.
    const QString base = QDir(QFileInfo(m_datastore->path()).absolutePath()).filePath(QStringLiteral("pulls"));
    QString safeLeaf = remotePath;
    safeLeaf.replace(QLatin1Char('/'), QLatin1Char('_'));
    const QString localDir = QDir(base).filePath(
        QStringLiteral("%1%2").arg(QDateTime::currentDateTime().toString(QStringLiteral("yyyyMMdd-hhmmss")), safeLeaf));

    if(!QDir().mkpath(localDir)) {
        m_eventLog->append(Severity::Error, Source::Device,
                           QStringLiteral("Cannot create staging dir: %1").arg(localDir));
        return false;
    }

    m_eventLog->append(Severity::Notice, Source::Device,
                       QStringLiteral("Pulling %1 from device…").arg(remotePath));
    emit pullStarted(remotePath);

    AbstractOperation *op = m_device->utility()->downloadDirectory(localDir, remotePath.toUtf8());

    connect(op, &AbstractOperation::finished, this, [this, op, localDir, remotePath]() {
        if(op->isError()) {
            m_eventLog->append(Severity::Error, Source::Device,
                               QStringLiteral("Pull of %1 failed: %2").arg(remotePath, op->errorString()));
            emit pullFinished(false, 0);
            return;
        }

        const int count = ingestDirectory(localDir, remotePath);
        m_eventLog->append(Severity::Notice, Source::Device,
                           QStringLiteral("Pulled %1: %2 file(s) ingested").arg(remotePath).arg(count));
        emit pullFinished(true, count);
    });

    return true;
}

int Bridge::ingestDirectory(const QString &localDir, const QString &remotePath)
{
    int count = 0;
    QDirIterator it(localDir, QDir::Files, QDirIterator::Subdirectories);
    while(it.hasNext()) {
        const QString localFile = it.next();
        const QString name = QFileInfo(localFile).fileName();
        const QString origin = QStringLiteral("%1/%2").arg(remotePath, name);
        recordCapture(localFile, name, origin);
        ++count;
    }
    return count;
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
            QStringLiteral("device.status"), QStringLiteral("capture.pull")
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
    }

    return fail(QStringLiteral("unknown command: %1").arg(cmd));
}
