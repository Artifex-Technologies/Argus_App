#pragma once

#include <QObject>
#include <QString>
#include <QVariantList>

#include "chymaera/chymaeratypes.h"

namespace Chymaera {

/* The offload database: an embedded SQLite store that lets the desktop act as
 * external storage for engagement data (Roadmap Phase 2 — "extra power /
 * database"). It records sessions, an event/console log, and captures/artifacts
 * pulled off the Flipper.
 *
 * Single-threaded: all access happens on the owning thread's event loop, so no
 * locking is needed. Each instance uses its own named QSqlDatabase connection. */

class ChymaeraDatastore : public QObject
{
    Q_OBJECT

public:
    explicit ChymaeraDatastore(QObject *parent = nullptr);
    ~ChymaeraDatastore() override;

    // Open (creating if needed) the database file at the given path and ensure
    // the schema exists. Returns false and emits errorOccured() on failure.
    bool open(const QString &databasePath);
    void close();

    bool isOpen() const;
    QString path() const;
    QString lastError() const;

    // Sessions. startSession() returns the new session id (or -1 on failure)
    // and makes it current; endSession() stamps the current session's end time.
    qint64 startSession(const QString &name, const QString &notes = QString());
    void endSession();
    qint64 currentSessionId() const;
    QString currentSessionName() const;

    // Append one event to the log table. Returns the row id, or -1 on failure.
    qint64 logEvent(Severity severity, Source source, const QString &message, const QString &detail = QString());

    // Record a capture/artifact (an NFC dump, a Sub-GHz file, a pulled file...).
    qint64 addCapture(const QString &type, const QString &name, const QString &sourcePath,
                      qint64 size, const QString &metaJson = QString());

    // Read-back helpers used by the control API and CLI. Each element is a
    // QVariantMap keyed by column name.
    QVariantList recentEvents(int limit = 200) const;
    QVariantList captures(int limit = 200) const;
    int eventCount() const;

signals:
    void opened();
    void closed();
    void sessionChanged();
    void errorOccured(const QString &message);

private:
    bool ensureSchema();
    void setError(const QString &message);

    QString m_connectionName;
    QString m_path;
    QString m_lastError;
    bool m_open;

    qint64 m_currentSessionId;
    QString m_currentSessionName;
};

}
