-- A datastore as v0.1 of the C++ app made it (no sha256/local_path columns) with one capture recorded
-- outside a session; the server adds the columns when it opens it, so the row has NULL in them.
CREATE TABLE sessions(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, notes TEXT, started_at INTEGER NOT NULL, ended_at INTEGER);
CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER, ts INTEGER NOT NULL, severity INTEGER NOT NULL, source INTEGER NOT NULL, message TEXT NOT NULL, detail TEXT);
CREATE TABLE captures(id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER, ts INTEGER NOT NULL, type TEXT NOT NULL, name TEXT, source_path TEXT, size INTEGER, meta TEXT);
INSERT INTO captures(session_id, ts, type, name, source_path, size, meta) VALUES(NULL, 1757600000000, 'nfc', 'old.nfc', '/ext/nfc/old.nfc', 42, '{"kind":"nfc"}');
