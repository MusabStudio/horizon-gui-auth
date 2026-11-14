//! SQLite account storage. `rusqlite` with `bundled` is one of the crates
//! `crates/horizon-mod-deps/Cargo.toml` reserves for native mods, so the mod
//! brings its own SQLite and needs no system library.
//!
//! The primary key is the lowercased player name. In offline mode the UUID is
//! `md5("OfflinePlayer:" + name)` (see `offline.rs`), so name and offline UUID
//! are the same identity; the UUID is stored for audit and for servers that
//! later switch to online mode.
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Account {
    pub name: String,
    pub uuid: String,
    pub hash: String,
    pub failed: u32,
    pub locked_until: i64,
    pub created_at: i64,
    pub last_login: i64,
}

#[derive(Clone, Debug)]
pub struct Store {
    path: PathBuf,
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS accounts (
    name         TEXT PRIMARY KEY COLLATE NOCASE,
    uuid         TEXT NOT NULL,
    hash         TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    last_ip      TEXT NOT NULL DEFAULT '',
    last_login   INTEGER NOT NULL DEFAULT 0,
    failed       INTEGER NOT NULL DEFAULT 0,
    locked_until INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS audit (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    at       INTEGER NOT NULL,
    name     TEXT NOT NULL,
    ip       TEXT NOT NULL,
    action   TEXT NOT NULL,
    detail   TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS audit_name_at ON audit(name, at);
"#;

impl Store {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn init(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let conn = self.conn()?;
        conn.execute_batch(SCHEMA).map_err(db_err)
    }

    fn conn(&self) -> Result<Connection, String> {
        let conn = Connection::open(&self.path).map_err(db_err)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(db_err)?;
        Ok(conn)
    }

    pub fn get(&self, name: &str) -> Result<Option<Account>, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT name, uuid, hash, failed, locked_until, created_at, last_login
             FROM accounts WHERE name = ?1 COLLATE NOCASE",
            params![name],
            |row| {
                Ok(Account {
                    name: row.get(0)?,
                    uuid: row.get(1)?,
                    hash: row.get(2)?,
                    failed: row.get::<_, i64>(3)?.max(0) as u32,
                    locked_until: row.get(4)?,
                    created_at: row.get(5)?,
                    last_login: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(db_err)
    }

    /// Insert a new account. `Ok(false)` means the name was already taken
    /// (another concurrent registration won).
    pub fn create(
        &self,
        name: &str,
        uuid: &str,
        hash: &str,
        ip: &str,
        now: i64,
    ) -> Result<bool, String> {
        let conn = self.conn()?;
        let changed = conn
            .execute(
                "INSERT INTO accounts
                    (name, uuid, hash, created_at, updated_at, last_ip, last_login, failed, locked_until)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, 0, 0, 0)
                 ON CONFLICT(name) DO NOTHING",
                params![name, uuid, hash, now, ip],
            )
            .map_err(db_err)?;
        Ok(changed == 1)
    }

    pub fn success(&self, name: &str, uuid: &str, ip: &str, now: i64) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE accounts
                SET uuid = ?2, failed = 0, locked_until = 0, last_login = ?4,
                    last_ip = ?3, updated_at = ?4
              WHERE name = ?1 COLLATE NOCASE",
            params![name, uuid, ip, now],
        )
        .map(|_| ())
        .map_err(db_err)
    }

    /// Record a wrong password. Returns `(attempts seen, locked_until_ms)`.
    /// When the count reaches `lock_after`, the counter resets and the account
    /// is locked for `lock_seconds`.
    pub fn failure(
        &self,
        name: &str,
        ip: &str,
        now: i64,
        lock_after: u32,
        lock_seconds: u64,
    ) -> Result<(u32, i64), String> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        let current: Option<(i64, i64)> = tx
            .query_row(
                "SELECT failed, locked_until FROM accounts WHERE name = ?1 COLLATE NOCASE",
                params![name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        let Some((failed, locked)) = current else {
            return Err("account disappeared".to_owned());
        };
        let attempts = failed.max(0) as u32 + 1;
        let (failed_next, locked_next) = if attempts >= lock_after {
            (0, now + (lock_seconds as i64) * 1000)
        } else {
            (attempts as i64, locked)
        };
        tx.execute(
            "UPDATE accounts
                SET failed = ?2, locked_until = ?3, last_ip = ?4, updated_at = ?5
              WHERE name = ?1 COLLATE NOCASE",
            params![name, failed_next, locked_next, ip, now],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok((attempts, locked_next))
    }

    pub fn set_password(&self, name: &str, hash: &str, now: i64) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE accounts SET hash = ?2, updated_at = ?3 WHERE name = ?1 COLLATE NOCASE",
            params![name, hash, now],
        )
        .map(|_| ())
        .map_err(db_err)
    }

    pub fn audit(&self, name: &str, ip: &str, action: &str, detail: &str, now: i64) {
        if let Ok(conn) = self.conn() {
            let _ = conn.execute(
                "INSERT INTO audit (at, name, ip, action, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![now, name, ip, action, detail],
            );
        }
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn db_err(e: rusqlite::Error) -> String {
    e.to_string()
}
