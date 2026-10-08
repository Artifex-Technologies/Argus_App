#pragma once

#include <QHash>
#include <QObject>
#include <QString>
#include <QJsonObject>

#include <functional>

class QTcpServer;
class QTcpSocket;

namespace Chymaera {

/* The Sarina control API (Roadmap Phase 4): a local, scriptable interface that
 * lets the assistant layer drive the app and query captured data.
 *
 * Transport is deliberately simple and dependency-free: newline-delimited JSON
 * over TCP, bound to loopback only (127.0.0.1). Each line is one request
 * object; each response is one JSON line. The server does transport + auth and
 * delegates the actual command handling to a callback supplied by the bridge,
 * keeping this class free of any dependency on the rest of the subsystem.
 *
 * Security posture: loopback-bound, non-loopback peers are dropped, and an
 * optional shared token can be required on every request. It is off by default
 * and must be started explicitly. */

class SarinaServer : public QObject
{
    Q_OBJECT

public:
    // A handler takes the parsed request object and returns the response object.
    using RequestHandler = std::function<QJsonObject(const QJsonObject &request)>;

    static constexpr quint16 DefaultPort = 44700;

    explicit SarinaServer(QObject *parent = nullptr);
    ~SarinaServer() override;

    void setRequestHandler(RequestHandler handler);

    // Require this token in every request's "token" field. Empty disables auth.
    void setToken(const QString &token);

    // Start listening on 127.0.0.1:<port>. Returns false on bind failure.
    bool start(quint16 port = DefaultPort);
    void stop();

    bool isListening() const;
    quint16 port() const;

signals:
    void listeningChanged();
    void requestReceived(const QString &command);
    void errorOccured(const QString &message);

private slots:
    void onNewConnection();
    void onReadyRead();
    void onDisconnected();

private:
    void processLine(QTcpSocket *socket, const QByteArray &line);
    void sendObject(QTcpSocket *socket, const QJsonObject &obj);

    QTcpServer *m_server;
    QHash<QTcpSocket *, QByteArray> m_buffers;
    RequestHandler m_handler;
    QString m_token;
};

}
