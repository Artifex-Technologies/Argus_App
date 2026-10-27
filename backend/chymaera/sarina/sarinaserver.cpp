#include "sarinaserver.h"

#include <QTcpServer>
#include <QTcpSocket>
#include <QHostAddress>
#include <QJsonDocument>
#include <QLoggingCategory>
#include <QDir>
#include <QFile>
#include <QRandomGenerator>
#include <QStandardPaths>

Q_LOGGING_CATEGORY(LOG_SARINA, "SARINA")

using namespace Chymaera;

SarinaServer::SarinaServer(QObject *parent):
    QObject(parent),
    m_server(new QTcpServer(this))
{
    connect(m_server, &QTcpServer::newConnection, this, &SarinaServer::onNewConnection);
}

SarinaServer::~SarinaServer()
{
    stop();
}

void SarinaServer::setRequestHandler(RequestHandler handler)
{
    m_handler = std::move(handler);
}

void SarinaServer::setToken(const QString &token)
{
    m_token = token;
}

bool SarinaServer::start(quint16 port)
{
    if(m_server->isListening()) {
        stop();
    }

    // Loopback is not a boundary between accounts: every local user (and the sarina service account) can connect. With no
    // token given, one is made here and written where only this account can read it ($XDG_RUNTIME_DIR/argus/token, 0600);
    // Sarina's console clients read it from there.
    if(m_token.isEmpty()) {
        const QString runtime = QStandardPaths::writableLocation(QStandardPaths::RuntimeLocation);
        if(runtime.isEmpty()) {
            emit errorOccured(QStringLiteral("No private runtime folder for the control token; not starting"));
            return false;
        }
        const QString dir = runtime + QStringLiteral("/argus");
        QDir().mkpath(dir);
        QFile::setPermissions(dir, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner);
        quint32 words[8];
        QRandomGenerator::system()->fillRange(words);
        const QString token = QString::fromLatin1(QByteArray(reinterpret_cast<const char *>(words), sizeof(words)).toHex());
        QFile f(dir + QStringLiteral("/token"));
        f.remove();
        if(!f.open(QIODevice::WriteOnly | QIODevice::NewOnly)
           || !f.setPermissions(QFile::ReadOwner | QFile::WriteOwner)
           || f.write(token.toLatin1()) != token.size()) {
            emit errorOccured(QStringLiteral("Cannot write the control token to %1; not starting").arg(f.fileName()));
            return false;
        }
        f.close();
        m_token = token;
        m_tokenFile = f.fileName();
    }

    // Loopback only — never expose the control API on the network.
    if(!m_server->listen(QHostAddress::LocalHost, port)) {
        const auto msg = QStringLiteral("Cannot listen on 127.0.0.1:%1: %2")
                             .arg(port).arg(m_server->errorString());
        qCWarning(LOG_SARINA).noquote() << msg;
        emit errorOccured(msg);
        return false;
    }

    qCInfo(LOG_SARINA).noquote() << "Sarina control API listening on 127.0.0.1:" << m_server->serverPort();
    emit listeningChanged();
    return true;
}

void SarinaServer::stop()
{
    if(!m_server->isListening() && m_buffers.isEmpty()) {
        return;
    }

    const auto sockets = m_buffers.keys();
    for(QTcpSocket *s : sockets) {
        s->disconnectFromHost();
        s->deleteLater();
    }
    m_buffers.clear();

    if(m_server->isListening()) {
        m_server->close();
    }
    if(!m_tokenFile.isEmpty()) {
        QFile::remove(m_tokenFile);
        m_tokenFile.clear();
        m_token.clear();
    }

    emit listeningChanged();
}

bool SarinaServer::isListening() const
{
    return m_server->isListening();
}

quint16 SarinaServer::port() const
{
    return m_server->isListening() ? m_server->serverPort() : 0;
}

void SarinaServer::onNewConnection()
{
    while(m_server->hasPendingConnections()) {
        QTcpSocket *socket = m_server->nextPendingConnection();

        // Defence in depth: reject anything that is not from loopback.
        const QHostAddress peer = socket->peerAddress();
        if(!peer.isLoopback()) {
            qCWarning(LOG_SARINA).noquote() << "Rejecting non-loopback peer" << peer.toString();
            socket->disconnectFromHost();
            socket->deleteLater();
            continue;
        }

        m_buffers.insert(socket, QByteArray());
        connect(socket, &QTcpSocket::readyRead, this, &SarinaServer::onReadyRead);
        connect(socket, &QTcpSocket::disconnected, this, &SarinaServer::onDisconnected);
    }
}

void SarinaServer::onReadyRead()
{
    auto *socket = qobject_cast<QTcpSocket *>(sender());
    if(!socket || !m_buffers.contains(socket)) {
        return;
    }

    QByteArray &buffer = m_buffers[socket];
    buffer.append(socket->readAll());

    // Cap unbounded buffering from a misbehaving client (1 MiB per line).
    if(buffer.size() > (1 << 20)) {
        sendObject(socket, { {QStringLiteral("ok"), false}, {QStringLiteral("error"), QStringLiteral("request too large")} });
        socket->disconnectFromHost();
        return;
    }

    int newline;
    while((newline = buffer.indexOf('\n')) >= 0) {
        const QByteArray line = buffer.left(newline);
        buffer.remove(0, newline + 1);
        processLine(socket, line.trimmed());
    }
}

void SarinaServer::onDisconnected()
{
    auto *socket = qobject_cast<QTcpSocket *>(sender());
    if(!socket) {
        return;
    }
    m_buffers.remove(socket);
    socket->deleteLater();
}

void SarinaServer::processLine(QTcpSocket *socket, const QByteArray &line)
{
    if(line.isEmpty()) {
        return;
    }

    QJsonParseError parseError {};
    const QJsonDocument doc = QJsonDocument::fromJson(line, &parseError);
    if(parseError.error != QJsonParseError::NoError || !doc.isObject()) {
        sendObject(socket, {
            {QStringLiteral("ok"), false},
            {QStringLiteral("error"), QStringLiteral("invalid JSON: %1").arg(parseError.errorString())}
        });
        return;
    }

    const QJsonObject request = doc.object();

    // Token check: always set once listening; compared in constant time.
    const QByteArray given = request.value(QStringLiteral("token")).toString().toUtf8();
    const QByteArray want = m_token.toUtf8();
    unsigned char diff = given.size() == want.size() ? 0 : 1;
    for(qsizetype i = 0; i < want.size(); ++i) {
        diff |= static_cast<unsigned char>(want[i] ^ (i < given.size() ? given[i] : 0));
    }
    if(m_token.isEmpty() || diff != 0) {
        sendObject(socket, {
            {QStringLiteral("ok"), false},
            {QStringLiteral("error"), QStringLiteral("unauthorized")}
        });
        return;
    }

    const QString command = request.value(QStringLiteral("cmd")).toString();
    emit requestReceived(command);

    QJsonObject response;
    if(m_handler) {
        response = m_handler(request);
    } else {
        response = {
            {QStringLiteral("ok"), false},
            {QStringLiteral("error"), QStringLiteral("no handler configured")}
        };
    }

    // Echo the request id back so async clients can correlate.
    if(request.contains(QStringLiteral("id"))) {
        response.insert(QStringLiteral("id"), request.value(QStringLiteral("id")));
    }

    sendObject(socket, response);
}

void SarinaServer::sendObject(QTcpSocket *socket, const QJsonObject &obj)
{
    if(!socket) {
        return;
    }
    socket->write(QJsonDocument(obj).toJson(QJsonDocument::Compact));
    socket->write("\n");
    socket->flush();
}
