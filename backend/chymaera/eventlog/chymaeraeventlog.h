#pragma once

#include <QVector>
#include <QString>
#include <QVariant>
#include <QDateTime>
#include <QPointer>
#include <QAbstractListModel>

#include "chymaera/chymaeratypes.h"

namespace Chymaera {

class ChymaeraDatastore;

/* The live console/event stream (Roadmap Phase 1). It is both:
 *   - a QAbstractListModel the QML console binds to, and
 *   - the central sink every subsystem writes to (device events, the Sarina
 *     API, exporters, manual notes).
 *
 * Entries are kept in a capped in-memory ring for the UI and, when a datastore
 * is attached, persisted so a session survives a restart. */

class ChymaeraEventLog : public QAbstractListModel
{
    Q_OBJECT
    Q_PROPERTY(int count READ count NOTIFY countChanged)

public:
    enum Role {
        TimestampRole = Qt::UserRole, // ISO-8601 string
        EpochMsRole,                  // qint64 ms since epoch
        SeverityRole,                 // int (Severity)
        SeverityNameRole,             // QString
        SourceRole,                   // int (Source)
        SourceNameRole,               // QString
        MessageRole,                  // QString
        DetailRole                    // QString
    };
    Q_ENUM(Role)

    explicit ChymaeraEventLog(QObject *parent = nullptr);

    // When set, every appended entry is also written to the datastore.
    void setDatastore(ChymaeraDatastore *datastore);

    // Cap the in-memory ring. Oldest entries beyond this are dropped from the
    // model (they remain in the datastore). Default 5000.
    void setMaxEntries(int maxEntries);
    int maxEntries() const;

    int count() const;

    // The central append entry point.
    void append(Severity severity, Source source, const QString &message, const QString &detail = QString());

    // Convenience for QML / manual notes.
    Q_INVOKABLE void log(const QString &message);
    Q_INVOKABLE void clear();

    // Return the most recent entries as QVariantMaps (used by the control API).
    QVariantList tail(int limit) const;

    // QAbstractListModel API
    int rowCount(const QModelIndex &parent = QModelIndex()) const override;
    QVariant data(const QModelIndex &index, int role = Qt::DisplayRole) const override;
    QHash<int, QByteArray> roleNames() const override;

signals:
    void countChanged();
    void entryAppended(int severity, const QString &source, const QString &message);

private:
    struct Entry {
        QDateTime ts;
        Severity severity;
        Source source;
        QString message;
        QString detail;
    };

    QVariantMap entryToMap(const Entry &e) const;

    QVector<Entry> m_entries;
    QPointer<ChymaeraDatastore> m_datastore;
    int m_maxEntries;
};

}
