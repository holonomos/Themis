//! SQLite access layer — schema, queries, and single-writer discipline.
//!
//! All DB access goes through `DbPool` which wraps a single `rusqlite::Connection`
//! behind `Arc<tokio::sync::Mutex<Connection>>`.  `rusqlite::Connection` is not
//! `Send` + `Sync`, so every access must be serialised.  The mutex is tokio's
//! async variant so callers don't block the runtime thread while waiting.
//!
//! All queries are intentionally kept short; the lock is never held across I/O.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use tokio::sync::Mutex;
use tracing::instrument;

use crate::lifecycle::LabState;

// ── DB handle ─────────────────────────────────────────────────────────────────

/// Shared, single-writer SQLite handle.
#[derive(Clone)]
pub struct DbPool {
    inner: Arc<Mutex<Connection>>,
}

impl DbPool {
    /// Open (or create) the database at `path`, run the DDL, and return a handle.
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // `rusqlite::Connection::open` is blocking I/O; spawn_blocking keeps
        // us off the async thread pool.
        let path_owned = path.to_path_buf();
        let conn = tokio::task::spawn_blocking(move || Connection::open(&path_owned))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;

        let pool = Self {
            inner: Arc::new(Mutex::new(conn)),
        };
        pool.run_migrations().await?;
        Ok(pool)
    }

    /// Open an in-memory database (for tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn open_in_memory() -> anyhow::Result<Self> {
        let conn = tokio::task::spawn_blocking(Connection::open_in_memory)
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;

        let pool = Self {
            inner: Arc::new(Mutex::new(conn)),
        };
        pool.run_migrations().await?;
        Ok(pool)
    }

    async fn run_migrations(&self) -> anyhow::Result<()> {
        let guard = self.inner.lock().await;
        guard.execute_batch(DDL)?;
        Ok(())
    }

    /// Acquire an exclusive lock on the SQLite connection.
    ///
    /// Callers must NOT hold the lock across `.await` points or any I/O.
    pub(crate) fn conn(&self) -> &Mutex<Connection> {
        &self.inner
    }
}

// ── Schema DDL ─────────────────────────────────────────────────────────────────

const DDL: &str = "
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS labs (
    name            TEXT PRIMARY KEY,
    template        TEXT NOT NULL,
    platform        TEXT NOT NULL,
    wan_interface   TEXT,
    themisfile      TEXT NOT NULL,
    state           TEXT NOT NULL,
    created_unix    INTEGER NOT NULL,
    updated_unix    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS nodes (
    lab_name        TEXT NOT NULL,
    name            TEXT NOT NULL,
    role            TEXT NOT NULL,
    state           TEXT NOT NULL,
    mgmt_ip         TEXT,
    updated_unix    INTEGER NOT NULL,
    PRIMARY KEY (lab_name, name),
    FOREIGN KEY (lab_name) REFERENCES labs(name) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS events (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    lab_name        TEXT NOT NULL,
    kind            TEXT NOT NULL,
    subject         TEXT NOT NULL,
    message         TEXT NOT NULL,
    payload_json    TEXT,
    timestamp_ns    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_events_lab_time ON events(lab_name, timestamp_ns);
";

// ── Row types ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LabRow {
    pub name: String,
    pub template: String,
    pub platform: String,
    #[allow(dead_code)]
    pub wan_interface: Option<String>,
    pub themisfile: String,
    pub state: LabState,
    pub created_unix: i64,
    #[allow(dead_code)]
    pub updated_unix: i64,
}

#[derive(Debug, Clone)]
pub struct NodeRow {
    #[allow(dead_code)]
    pub lab_name: String,
    pub name: String,
    #[allow(dead_code)]
    pub role: String,
    #[allow(dead_code)]
    pub state: String,
    #[allow(dead_code)]
    pub mgmt_ip: Option<String>,
    #[allow(dead_code)]
    pub updated_unix: i64,
}

// ── Lab queries ───────────────────────────────────────────────────────────────

/// Insert a new lab row.
#[instrument(skip(db, themisfile_content))]
pub async fn insert_lab(
    db: &DbPool,
    name: &str,
    template: &str,
    platform: &str,
    wan_interface: Option<&str>,
    themisfile_content: &str,
    state: LabState,
) -> anyhow::Result<()> {
    let now = unix_now();
    let name = name.to_string();
    let template = template.to_string();
    let platform = platform.to_string();
    let wan_interface = wan_interface.map(str::to_string);
    let themisfile_content = themisfile_content.to_string();
    let state_str = state.as_str().to_string();

    let conn = db.conn().lock().await;
    conn.execute(
        "INSERT INTO labs (name, template, platform, wan_interface, themisfile, state, created_unix, updated_unix)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![name, template, platform, wan_interface, themisfile_content, state_str, now],
    )?;
    Ok(())
}

/// Fetch a single lab row by name.
pub async fn get_lab(db: &DbPool, name: &str) -> anyhow::Result<Option<LabRow>> {
    let name = name.to_string();
    let conn = db.conn().lock().await;
    let mut stmt = conn.prepare(
        "SELECT name, template, platform, wan_interface, themisfile, state, created_unix, updated_unix
         FROM labs WHERE name = ?1"
    )?;
    let mut rows = stmt.query(params![name])?;
    if let Some(row) = rows.next()? {
        let state_str: String = row.get(5)?;
        Ok(Some(LabRow {
            name: row.get(0)?,
            template: row.get(1)?,
            platform: row.get(2)?,
            wan_interface: row.get(3)?,
            themisfile: row.get(4)?,
            state: LabState::from_str(&state_str)
                .ok_or_else(|| anyhow::anyhow!("unknown lab state: {state_str}"))?,
            created_unix: row.get(6)?,
            updated_unix: row.get(7)?,
        }))
    } else {
        Ok(None)
    }
}

/// List all lab rows.
pub async fn list_labs(db: &DbPool) -> anyhow::Result<Vec<LabRow>> {
    let conn = db.conn().lock().await;
    let mut stmt = conn.prepare(
        "SELECT name, template, platform, wan_interface, themisfile, state, created_unix, updated_unix
         FROM labs ORDER BY name"
    )?;
    let rows = stmt.query_map([], |row| {
        let state_str: String = row.get(5)?;
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
            state_str,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
        ))
    })?;

    let mut result = Vec::new();
    for r in rows {
        let (name, template, platform, wan_interface, themisfile, state_str, created_unix, updated_unix) = r?;
        let state = LabState::from_str(&state_str)
            .ok_or_else(|| anyhow::anyhow!("unknown lab state: {state_str}"))?;
        result.push(LabRow {
            name,
            template,
            platform,
            wan_interface,
            themisfile,
            state,
            created_unix,
            updated_unix,
        });
    }
    Ok(result)
}

/// Update the state (and updated_unix timestamp) of a lab.
pub async fn update_lab_state(
    db: &DbPool,
    name: &str,
    state: LabState,
) -> anyhow::Result<()> {
    let now = unix_now();
    let name = name.to_string();
    let state_str = state.as_str().to_string();
    let conn = db.conn().lock().await;
    conn.execute(
        "UPDATE labs SET state = ?1, updated_unix = ?2 WHERE name = ?3",
        params![state_str, now, name],
    )?;
    Ok(())
}

/// Delete a lab and its nodes (via ON DELETE CASCADE).
#[allow(dead_code)] // available for admin / housekeeping operations
pub async fn delete_lab(db: &DbPool, name: &str) -> anyhow::Result<()> {
    let name = name.to_string();
    let conn = db.conn().lock().await;
    conn.execute("DELETE FROM labs WHERE name = ?1", params![name])?;
    Ok(())
}

// ── Node queries ──────────────────────────────────────────────────────────────

/// Insert (or replace) a node row.
pub async fn upsert_node(
    db: &DbPool,
    lab_name: &str,
    node_name: &str,
    role: &str,
    state: &str,
    mgmt_ip: Option<&str>,
) -> anyhow::Result<()> {
    let now = unix_now();
    let lab_name = lab_name.to_string();
    let node_name = node_name.to_string();
    let role = role.to_string();
    let state = state.to_string();
    let mgmt_ip = mgmt_ip.map(str::to_string);
    let conn = db.conn().lock().await;
    conn.execute(
        "INSERT INTO nodes (lab_name, name, role, state, mgmt_ip, updated_unix)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(lab_name, name) DO UPDATE SET
             state = excluded.state,
             mgmt_ip = excluded.mgmt_ip,
             updated_unix = excluded.updated_unix",
        params![lab_name, node_name, role, state, mgmt_ip, now],
    )?;
    Ok(())
}

/// Update the state of a single node.
pub async fn update_node_state(
    db: &DbPool,
    lab_name: &str,
    node_name: &str,
    state: &str,
) -> anyhow::Result<()> {
    let now = unix_now();
    let lab_name = lab_name.to_string();
    let node_name = node_name.to_string();
    let state = state.to_string();
    let conn = db.conn().lock().await;
    conn.execute(
        "UPDATE nodes SET state = ?1, updated_unix = ?2 WHERE lab_name = ?3 AND name = ?4",
        params![state, now, lab_name, node_name],
    )?;
    Ok(())
}

/// Fetch all node rows for a lab.
pub async fn get_nodes(db: &DbPool, lab_name: &str) -> anyhow::Result<Vec<NodeRow>> {
    let lab_name = lab_name.to_string();
    let conn = db.conn().lock().await;
    let mut stmt = conn.prepare(
        "SELECT lab_name, name, role, state, mgmt_ip, updated_unix
         FROM nodes WHERE lab_name = ?1 ORDER BY name"
    )?;
    let rows = stmt.query_map(params![lab_name], |row| {
        Ok(NodeRow {
            lab_name: row.get(0)?,
            name: row.get(1)?,
            role: row.get(2)?,
            state: row.get(3)?,
            mgmt_ip: row.get(4)?,
            updated_unix: row.get(5)?,
        })
    })?;
    let mut result = Vec::new();
    for r in rows {
        result.push(r?);
    }
    Ok(result)
}

// ── Event queries ─────────────────────────────────────────────────────────────

/// Persist an event to the events table.
pub async fn insert_event(
    db: &DbPool,
    lab_name: &str,
    kind: &str,
    subject: &str,
    message: &str,
    payload_json: Option<&str>,
    timestamp_ns: i64,
) -> anyhow::Result<()> {
    let lab_name = lab_name.to_string();
    let kind = kind.to_string();
    let subject = subject.to_string();
    let message = message.to_string();
    let payload_json = payload_json.map(str::to_string);
    let conn = db.conn().lock().await;
    conn.execute(
        "INSERT INTO events (lab_name, kind, subject, message, payload_json, timestamp_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![lab_name, kind, subject, message, payload_json, timestamp_ns],
    )?;
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Current time as Unix epoch seconds.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::LabState;

    async fn open_db() -> DbPool {
        DbPool::open_in_memory().await.expect("open_in_memory")
    }

    #[tokio::test]
    async fn schema_init_succeeds() {
        let _db = open_db().await;
    }

    #[tokio::test]
    async fn insert_and_get_lab_round_trip() {
        let db = open_db().await;
        insert_lab(
            &db,
            "alpha",
            "clos-3tier",
            "frr-fedora",
            Some("eth0"),
            "fabric \"alpha\" {}",
            LabState::Defined,
        )
        .await
        .unwrap();

        let row = get_lab(&db, "alpha").await.unwrap().expect("row must exist");
        assert_eq!(row.name, "alpha");
        assert_eq!(row.template, "clos-3tier");
        assert_eq!(row.platform, "frr-fedora");
        assert_eq!(row.wan_interface.as_deref(), Some("eth0"));
        assert_eq!(row.state, LabState::Defined);
    }

    #[tokio::test]
    async fn get_lab_missing_returns_none() {
        let db = open_db().await;
        let row = get_lab(&db, "does-not-exist").await.unwrap();
        assert!(row.is_none());
    }

    #[tokio::test]
    async fn update_lab_state_persists() {
        let db = open_db().await;
        insert_lab(
            &db, "beta", "clos-3tier", "frr-fedora", None,
            "content", LabState::Defined,
        ).await.unwrap();

        update_lab_state(&db, "beta", LabState::Running).await.unwrap();

        let row = get_lab(&db, "beta").await.unwrap().unwrap();
        assert_eq!(row.state, LabState::Running);
    }

    #[tokio::test]
    async fn list_labs_returns_all() {
        let db = open_db().await;
        insert_lab(&db, "lab1", "t", "p", None, "c", LabState::Defined).await.unwrap();
        insert_lab(&db, "lab2", "t", "p", None, "c", LabState::Running).await.unwrap();

        let labs = list_labs(&db).await.unwrap();
        assert_eq!(labs.len(), 2);
    }

    #[tokio::test]
    async fn upsert_node_and_get_nodes() {
        let db = open_db().await;
        insert_lab(&db, "lab", "t", "p", None, "c", LabState::Defined).await.unwrap();

        upsert_node(&db, "lab", "spine-1", "spine", "provisioning", Some("10.0.0.1"))
            .await
            .unwrap();
        upsert_node(&db, "lab", "leaf-1", "leaf", "provisioning", Some("10.0.0.2"))
            .await
            .unwrap();

        let nodes = get_nodes(&db, "lab").await.unwrap();
        assert_eq!(nodes.len(), 2);
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"spine-1"));
        assert!(names.contains(&"leaf-1"));
    }

    #[tokio::test]
    async fn update_node_state_persists() {
        let db = open_db().await;
        insert_lab(&db, "lab", "t", "p", None, "c", LabState::Defined).await.unwrap();
        upsert_node(&db, "lab", "n1", "spine", "provisioning", None).await.unwrap();

        update_node_state(&db, "lab", "n1", "running").await.unwrap();

        let nodes = get_nodes(&db, "lab").await.unwrap();
        assert_eq!(nodes[0].state, "running");
    }

    #[tokio::test]
    async fn insert_event_succeeds() {
        let db = open_db().await;
        insert_lab(&db, "lab", "t", "p", None, "c", LabState::Defined).await.unwrap();
        insert_event(
            &db, "lab", "LAB_STATE", "lab", "transitioned to Running",
            None, 123456789,
        ).await.unwrap();
    }

    #[tokio::test]
    async fn delete_lab_cascades_to_nodes() {
        let db = open_db().await;
        insert_lab(&db, "lab", "t", "p", None, "c", LabState::Defined).await.unwrap();
        upsert_node(&db, "lab", "n1", "spine", "provisioning", None).await.unwrap();

        delete_lab(&db, "lab").await.unwrap();

        // Lab gone.
        assert!(get_lab(&db, "lab").await.unwrap().is_none());
        // Nodes gone via cascade.
        let nodes = get_nodes(&db, "lab").await.unwrap();
        assert!(nodes.is_empty());
    }
}
