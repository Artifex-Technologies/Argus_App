//! The capture datastore: the port of `ChymaeraDatastore` (backend/chymaera/datastore). One
//! SQLite file shared with the C++ GUI and CLI and read by the Sarina API, so the schema, the
//! migrations and the stored values are the C++ app's, column for column.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

/// `Severity` in backend/chymaera/chymaeratypes.h (stored as its number).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Debug = 0,
    Info,
    Notice,
    Warning,
    Error,
    Critical,
}

/// `Source` in backend/chymaera/chymaeratypes.h (stored as its number).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    System = 0,
    Device,
    Sarina,
    Capture,
    Export,
    User,
}

/// One row of `captures`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub kind: String,
    pub name: String,
    pub source_path: String,
    pub size: u64,
    pub meta: String,
    pub sha256: String,
    pub local_path: String,
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

#[derive(Debug)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> StoreError {
        StoreError(format!("datastore: {e}"))
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Where the GUI and the CLI keep it: `$XDG_DATA_HOME/Argus/chymaera/chymaera.db`, or under
/// `~/.local/share` (Qt's GenericDataLocation).
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("Argus/chymaera/chymaera.db"))
}

impl Store {
    /// Opens (or creates) the datastore with the C++ app's pragmas, schema and migrations. A
    /// new folder for it is made private (0700).
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        if let Some(dir) = path.parent() {
            if !dir.exists() {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(dir)
                    .map_err(|e| StoreError(format!("cannot create {}: {e}", dir.display())))?;
            }
        }
        let conn = Connection::open(path)?;
        // The GUI may hold the file at the same time.
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions(
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               name TEXT NOT NULL,
               notes TEXT,
               started_at INTEGER NOT NULL,
               ended_at INTEGER);
             CREATE TABLE IF NOT EXISTS events(
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               session_id INTEGER,
               ts INTEGER NOT NULL,
               severity INTEGER NOT NULL,
               source INTEGER NOT NULL,
               message TEXT NOT NULL,
               detail TEXT);
             CREATE TABLE IF NOT EXISTS captures(
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               session_id INTEGER,
               ts INTEGER NOT NULL,
               type TEXT NOT NULL,
               name TEXT,
               source_path TEXT,
               size INTEGER,
               meta TEXT);
             CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);
             CREATE INDEX IF NOT EXISTS idx_captures_session ON captures(session_id);",
        )?;
        // Columns added after v0.1 of the C++ datastore.
        let mut columns = Vec::new();
        {
            let mut q = conn.prepare("PRAGMA table_info(captures)")?;
            let rows = q.query_map([], |r| r.get::<_, String>(1))?;
            for c in rows {
                columns.push(c?);
            }
        }
        if !columns.iter().any(|c| c == "sha256") {
            conn.execute_batch("ALTER TABLE captures ADD COLUMN sha256 TEXT")?;
        }
        if !columns.iter().any(|c| c == "local_path") {
            conn.execute_batch("ALTER TABLE captures ADD COLUMN local_path TEXT")?;
        }
        conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_captures_sha256 ON captures(sha256)")?;
        Ok(Store { conn, path: path.to_path_buf() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// True when this content from this device path is stored already (`hasCapture`).
    pub fn has_capture(&self, sha256: &str, source_path: &str) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM captures WHERE sha256 = ? AND source_path = ? LIMIT 1", params![sha256, source_path], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// `addCapture`, outside any session (as the C++ CLI's pulls).
    pub fn add_capture(&self, c: &Capture) -> Result<i64, StoreError> {
        let size = i64::try_from(c.size).map_err(|_| StoreError("capture too large".into()))?;
        self.conn.execute(
            "INSERT INTO captures(session_id, ts, type, name, source_path, size, meta, sha256, local_path)
             VALUES(NULL, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![now_ms(), c.kind, c.name, c.source_path, size, c.meta, c.sha256, c.local_path],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// `logEvent`, outside any session.
    pub fn log_event(&self, severity: Severity, source: Source, message: &str, detail: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO events(session_id, ts, severity, source, message, detail) VALUES(NULL, ?, ?, ?, ?, ?)",
            params![now_ms(), severity as i64, source as i64, message, detail],
        )?;
        Ok(())
    }

    /// Every capture, oldest first (for tests and comparisons).
    pub fn captures(&self) -> Result<Vec<Capture>, StoreError> {
        let mut q = self.conn.prepare("SELECT type, name, source_path, size, meta, sha256, local_path FROM captures ORDER BY id")?;
        let rows = q.query_map([], |r| {
            Ok(Capture {
                kind: r.get(0)?,
                name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                source_path: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                size: r.get::<_, Option<i64>>(3)?.unwrap_or(0).max(0) as u64,
                meta: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                sha256: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                local_path: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}
