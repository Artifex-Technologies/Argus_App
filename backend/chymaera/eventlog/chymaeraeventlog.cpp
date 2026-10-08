#include "chymaeraeventlog.h"

#include "chymaera/datastore/chymaeradatastore.h"

using namespace Chymaera;

ChymaeraEventLog::ChymaeraEventLog(QObject *parent):
    QAbstractListModel(parent),
    m_maxEntries(5000)
{}

void ChymaeraEventLog::setDatastore(ChymaeraDatastore *datastore)
{
    m_datastore = datastore;
}

void ChymaeraEventLog::setMaxEntries(int maxEntries)
{
    m_maxEntries = qMax(1, maxEntries);
}

int ChymaeraEventLog::maxEntries() const
{
    return m_maxEntries;
}

int ChymaeraEventLog::count() const
{
    return m_entries.size();
}

void ChymaeraEventLog::append(Severity severity, Source source, const QString &message, const QString &detail)
{
    // Persist first so the datastore keeps the full history even when the
    // in-memory ring drops the oldest entries.
    if(m_datastore && m_datastore->isOpen()) {
        m_datastore->logEvent(severity, source, message, detail);
    }

    const Entry entry { QDateTime::currentDateTime(), severity, source, message, detail };

    const int row = m_entries.size();
    beginInsertRows(QModelIndex(), row, row);
    m_entries.append(entry);
    endInsertRows();

    // Trim the front of the ring if we are over the cap.
    if(m_entries.size() > m_maxEntries) {
        const int overflow = m_entries.size() - m_maxEntries;
        beginRemoveRows(QModelIndex(), 0, overflow - 1);
        m_entries.remove(0, overflow);
        endRemoveRows();
    }

    emit countChanged();
    emit entryAppended(static_cast<int>(severity), sourceName(source), message);
}

void ChymaeraEventLog::log(const QString &message)
{
    append(Severity::Info, Source::User, message);
}

void ChymaeraEventLog::clear()
{
    if(m_entries.isEmpty()) {
        return;
    }

    beginResetModel();
    m_entries.clear();
    endResetModel();

    emit countChanged();
}

QVariantList ChymaeraEventLog::tail(int limit) const
{
    QVariantList out;
    const int start = qMax(0, m_entries.size() - qMax(0, limit));
    for(int i = start; i < m_entries.size(); ++i) {
        out.append(entryToMap(m_entries.at(i)));
    }
    return out;
}

int ChymaeraEventLog::rowCount(const QModelIndex &parent) const
{
    if(parent.isValid()) {
        return 0;
    }
    return m_entries.size();
}

QVariant ChymaeraEventLog::data(const QModelIndex &index, int role) const
{
    if(!index.isValid() || index.row() < 0 || index.row() >= m_entries.size()) {
        return QVariant();
    }

    const Entry &e = m_entries.at(index.row());

    switch(role) {
    case TimestampRole:    return e.ts.toString(Qt::ISODateWithMs);
    case EpochMsRole:      return e.ts.toMSecsSinceEpoch();
    case SeverityRole:     return static_cast<int>(e.severity);
    case SeverityNameRole: return severityName(e.severity);
    case SourceRole:       return static_cast<int>(e.source);
    case SourceNameRole:   return sourceName(e.source);
    case MessageRole:      return e.message;
    case DetailRole:       return e.detail;
    default:               return QVariant();
    }
}

QHash<int, QByteArray> ChymaeraEventLog::roleNames() const
{
    return {
        { TimestampRole,    "timestamp" },
        { EpochMsRole,      "epochMs" },
        { SeverityRole,     "severity" },
        { SeverityNameRole, "severityName" },
        { SourceRole,       "source" },
        { SourceNameRole,   "sourceName" },
        { MessageRole,      "message" },
        { DetailRole,       "detail" }
    };
}

QVariantMap ChymaeraEventLog::entryToMap(const Entry &e) const
{
    QVariantMap m;
    m[QStringLiteral("timestamp")] = e.ts.toString(Qt::ISODateWithMs);
    m[QStringLiteral("epochMs")] = e.ts.toMSecsSinceEpoch();
    m[QStringLiteral("severity")] = severityName(e.severity);
    m[QStringLiteral("source")] = sourceName(e.source);
    m[QStringLiteral("message")] = e.message;
    m[QStringLiteral("detail")] = e.detail;
    return m;
}
