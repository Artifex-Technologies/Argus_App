#include "chymaeradatastore.h"

#include <QDir>
#include <QFileInfo>
#include <QDateTime>
#include <QSqlError>
#include <QSqlQuery>
#include <QSqlDatabase>
#include <QLoggingCategory>

Q_LOGGING_CATEGORY(LOG_CHYMAERA_DB, "CHYMAERA_DB")

using namespace Chymaera;

namespace {
// Unique, stable connection name per datastore instance.
int nextConnectionId()
{
    static int counter = 0;
    return ++counter;
}
}

ChymaeraDatastore::ChymaeraDatastore(QObject *parent):
    QObject(parent),
    m_connectionName(QStringLiteral("chymaera_datastore_%1").arg(nextConnectionId())),
    m_open(false),
    m_currentSessionId(-1)
{}

ChymaeraDatastore::~ChymaeraDatastore()
{
    close();
}

bool ChymaeraDatastore::open(const QString &databasePath)
{
    close();

    const QFileInfo info(databasePath);
    const QDir dir = info.absoluteDir();

    if(!dir.exists() && !dir.mkpath(QStringLiteral("."))) {
        setError(QStringLiteral("Cannot create datastore directory: %1").arg(dir.absolutePath()));
        return false;
    }

    auto db = QSqlDatabase::addDatabase(QStringLiteral("QSQLITE"), m_connectionName);
    db.setDatabaseName(databasePath);

    if(!db.open()) {
        setError(QStringLiteral("Cannot open datastore: %1").arg(db.lastError().text()));
        QSqlDatabase::removeDatabase(m_connectionName);
        return false;
    }

    // Sensible pragmas for a single-writer local store.
    QSqlQuery pragma(db);
    pragma.exec(QStringLiteral("PRAGMA journal_mode = WAL"));
    pragma.exec(QStringLiteral("PRAGMA foreign_keys = ON"));

    m_path = databasePath;
    m_open = true;

    if(!ensureSchema()) {
        close();
        return false;
    }

    qCInfo(LOG_CHYMAERA_DB).noquote() << "Datastore opened at" << databasePath;
    emit opened();
    return true;
}

void ChymaeraDatastore::close()
{
    if(m_open) {
        {
            auto db = QSqlDatabase::database(m_connectionName, false);
            if(db.isOpen()) {
                db.close();
            }
        }
        QSqlDatabase::removeDatabase(m_connectionName);

        m_open = false;
        m_path.clear();
        m_currentSessionId = -1;
        m_currentSessionName.clear();

        emit closed();
    }
}

bool ChymaeraDatastore::isOpen() const
{
    return m_open;
}

QString ChymaeraDatastore::path() const
{
    return m_path;
}

QString ChymaeraDatastore::lastError() const
{
    return m_lastError;
}

qint64 ChymaeraDatastore::startSession(const QString &name, const QString &notes)
{
    if(!m_open) {
        setError(QStringLiteral("Datastore is not open"));
        return -1;
    }

    auto db = QSqlDatabase::database(m_connectionName);
    QSqlQuery q(db);
    q.prepare(QStringLiteral("INSERT INTO sessions(name, notes, started_at) VALUES(?, ?, ?)"));
    q.addBindValue(name);
    q.addBindValue(notes);
    q.addBindValue(QDateTime::currentSecsSinceEpoch());

    if(!q.exec()) {
        setError(QStringLiteral("Cannot start session: %1").arg(q.lastError().text()));
        return -1;
    }

    m_currentSessionId = q.lastInsertId().toLongLong();
    m_currentSessionName = name;
    emit sessionChanged();
    return m_currentSessionId;
}

void ChymaeraDatastore::endSession()
{
    if(!m_open || m_currentSessionId < 0) {
        return;
    }

    auto db = QSqlDatabase::database(m_connectionName);
    QSqlQuery q(db);
    q.prepare(QStringLiteral("UPDATE sessions SET ended_at = ? WHERE id = ?"));
    q.addBindValue(QDateTime::currentSecsSinceEpoch());
    q.addBindValue(m_currentSessionId);

    if(!q.exec()) {
        setError(QStringLiteral("Cannot end session: %1").arg(q.lastError().text()));
    }

    m_currentSessionId = -1;
    m_currentSessionName.clear();
    emit sessionChanged();
}

qint64 ChymaeraDatastore::currentSessionId() const
{
    return m_currentSessionId;
}

QString ChymaeraDatastore::currentSessionName() const
{
    return m_currentSessionName;
}

qint64 ChymaeraDatastore::logEvent(Severity severity, Source source, const QString &message, const QString &detail)
{
    if(!m_open) {
        return -1;
    }

    auto db = QSqlDatabase::database(m_connectionName);
    QSqlQuery q(db);
    q.prepare(QStringLiteral(
        "INSERT INTO events(session_id, ts, severity, source, message, detail) VALUES(?, ?, ?, ?, ?, ?)"));
    q.addBindValue(m_currentSessionId < 0 ? QVariant() : QVariant(m_currentSessionId));
    q.addBindValue(QDateTime::currentMSecsSinceEpoch());
    q.addBindValue(static_cast<int>(severity));
    q.addBindValue(static_cast<int>(source));
    q.addBindValue(message);
    q.addBindValue(detail);

    if(!q.exec()) {
        setError(QStringLiteral("Cannot log event: %1").arg(q.lastError().text()));
        return -1;
    }

    return q.lastInsertId().toLongLong();
}

qint64 ChymaeraDatastore::addCapture(const QString &type, const QString &name, const QString &sourcePath,
                                     qint64 size, const QString &metaJson,
                                     const QString &sha256, const QString &localPath)
{
    if(!m_open) {
        setError(QStringLiteral("Datastore is not open"));
        return -1;
    }

    auto db = QSqlDatabase::database(m_connectionName);
    QSqlQuery q(db);
    q.prepare(QStringLiteral(
        "INSERT INTO captures(session_id, ts, type, name, source_path, size, meta, sha256, local_path)"
        " VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?)"));
    q.addBindValue(m_currentSessionId < 0 ? QVariant() : QVariant(m_currentSessionId));
    q.addBindValue(QDateTime::currentMSecsSinceEpoch());
    q.addBindValue(type);
    q.addBindValue(name);
    q.addBindValue(sourcePath);
    q.addBindValue(size);
    q.addBindValue(metaJson);
    q.addBindValue(sha256.isEmpty() ? QVariant() : QVariant(sha256));
    q.addBindValue(localPath.isEmpty() ? QVariant() : QVariant(localPath));

    if(!q.exec()) {
        setError(QStringLiteral("Cannot add capture: %1").arg(q.lastError().text()));
        return -1;
    }

    return q.lastInsertId().toLongLong();
}

bool ChymaeraDatastore::hasCapture(const QString &sha256, const QString &sourcePath) const
{
    if(!m_open || sha256.isEmpty()) {
        return false;
    }

    auto db = QSqlDatabase::database(m_connectionName);
    QSqlQuery q(db);
    q.prepare(QStringLiteral("SELECT 1 FROM captures WHERE sha256 = ? AND source_path = ? LIMIT 1"));
    q.addBindValue(sha256);
    q.addBindValue(sourcePath);

    return q.exec() && q.next();
}

QVariantList ChymaeraDatastore::recentEvents(int limit) const
{
    QVariantList out;
    if(!m_open) {
        return out;
    }

    auto db = QSqlDatabase::database(m_connectionName);
    QSqlQuery q(db);
    q.prepare(QStringLiteral(
        "SELECT id, session_id, ts, severity, source, message, detail FROM events ORDER BY id DESC LIMIT ?"));
    q.addBindValue(limit);

    if(!q.exec()) {
        return out;
    }

    while(q.next()) {
        QVariantMap row;
        row[QStringLiteral("id")] = q.value(0);
        row[QStringLiteral("session_id")] = q.value(1);
        row[QStringLiteral("ts")] = q.value(2);
        row[QStringLiteral("severity")] = severityName(static_cast<Severity>(q.value(3).toInt()));
        row[QStringLiteral("source")] = sourceName(static_cast<Source>(q.value(4).toInt()));
        row[QStringLiteral("message")] = q.value(5);
        row[QStringLiteral("detail")] = q.value(6);
        out.prepend(row); // restore chronological order
    }

    return out;
}

QVariantList ChymaeraDatastore::captures(int limit) const
{
    QVariantList out;
    if(!m_open) {
        return out;
    }

    auto db = QSqlDatabase::database(m_connectionName);
    QSqlQuery q(db);
    q.prepare(QStringLiteral(
        "SELECT id, session_id, ts, type, name, source_path, size, meta, sha256, local_path"
        " FROM captures ORDER BY id DESC LIMIT ?"));
    q.addBindValue(limit);

    if(!q.exec()) {
        return out;
    }

    while(q.next()) {
        QVariantMap row;
        row[QStringLiteral("id")] = q.value(0);
        row[QStringLiteral("session_id")] = q.value(1);
        row[QStringLiteral("ts")] = q.value(2);
        row[QStringLiteral("type")] = q.value(3);
        row[QStringLiteral("name")] = q.value(4);
        row[QStringLiteral("source_path")] = q.value(5);
        row[QStringLiteral("size")] = q.value(6);
        row[QStringLiteral("meta")] = q.value(7);
        row[QStringLiteral("sha256")] = q.value(8);
        row[QStringLiteral("local_path")] = q.value(9);
        out.append(row);
    }

    return out;
}

int ChymaeraDatastore::eventCount() const
{
    if(!m_open) {
        return 0;
    }

    auto db = QSqlDatabase::database(m_connectionName);
    QSqlQuery q(db);
    if(q.exec(QStringLiteral("SELECT COUNT(*) FROM events")) && q.next()) {
        return q.value(0).toInt();
    }
    return 0;
}

bool ChymaeraDatastore::ensureSchema()
{
    auto db = QSqlDatabase::database(m_connectionName);
    QSqlQuery q(db);

    const QStringList statements = {
        QStringLiteral(
            "CREATE TABLE IF NOT EXISTS sessions("
            "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
            "  name TEXT NOT NULL,"
            "  notes TEXT,"
            "  started_at INTEGER NOT NULL,"
            "  ended_at INTEGER)"),
        QStringLiteral(
            "CREATE TABLE IF NOT EXISTS events("
            "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
            "  session_id INTEGER,"
            "  ts INTEGER NOT NULL,"
            "  severity INTEGER NOT NULL,"
            "  source INTEGER NOT NULL,"
            "  message TEXT NOT NULL,"
            "  detail TEXT)"),
        QStringLiteral(
            "CREATE TABLE IF NOT EXISTS captures("
            "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
            "  session_id INTEGER,"
            "  ts INTEGER NOT NULL,"
            "  type TEXT NOT NULL,"
            "  name TEXT,"
            "  source_path TEXT,"
            "  size INTEGER,"
            "  meta TEXT)"),
        QStringLiteral("CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id)"),
        QStringLiteral("CREATE INDEX IF NOT EXISTS idx_captures_session ON captures(session_id)")
    };

    for(const auto &sql : statements) {
        if(!q.exec(sql)) {
            setError(QStringLiteral("Schema error: %1").arg(q.lastError().text()));
            return false;
        }
    }

    // Columns added after v0.1; databases created earlier get them here.
    QStringList captureColumns;
    if(q.exec(QStringLiteral("PRAGMA table_info(captures)"))) {
        while(q.next()) {
            captureColumns.append(q.value(1).toString());
        }
    }

    QStringList migrations;
    if(!captureColumns.contains(QStringLiteral("sha256"))) {
        migrations.append(QStringLiteral("ALTER TABLE captures ADD COLUMN sha256 TEXT"));
    }
    if(!captureColumns.contains(QStringLiteral("local_path"))) {
        migrations.append(QStringLiteral("ALTER TABLE captures ADD COLUMN local_path TEXT"));
    }
    migrations.append(QStringLiteral("CREATE INDEX IF NOT EXISTS idx_captures_sha256 ON captures(sha256)"));

    for(const auto &sql : qAsConst(migrations)) {
        if(!q.exec(sql)) {
            setError(QStringLiteral("Schema migration error: %1").arg(q.lastError().text()));
            return false;
        }
    }

    return true;
}

void ChymaeraDatastore::setError(const QString &message)
{
    m_lastError = message;
    qCWarning(LOG_CHYMAERA_DB).noquote() << message;
    emit errorOccured(message);
}
