#include "sarinaserver.h"

#include <QTcpServer>
#include <QTcpSocket>
#include <QHostAddress>
#include <QJsonDocument>
#include <QLoggingCategory>

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

    // Token check (when configured).
    if(!m_token.isEmpty() && request.value(QStringLiteral("token")).toString() != m_token) {
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
