use rusqlite::{ffi, Connection, Result};
use sqlite_vec::sqlite3_vec_init;
use std::path::Path;
use std::sync::Once;

static VEC_INIT: Once = Once::new();

fn ensure_vec_extension() {
    VEC_INIT.call_once(|| {
        // Register sqlite-vec as an auto-extension so every Connection opened
        // afterwards has vec0 available (this matches the sqlite-vec crate's
        // documented loading pattern).
        unsafe {
            type EntryPoint = unsafe extern "C" fn(
                *mut ffi::sqlite3,
                *mut *mut i8,
                *const ffi::sqlite3_api_routines,
            ) -> i32;
            let entry: EntryPoint =
                std::mem::transmute(sqlite3_vec_init as *const ());
            ffi::sqlite3_auto_extension(Some(entry));
        }
    });
}

/// Open the workspace sqlite file, creating `state/`, the file, and the full
/// schema if any of them are missing. Use this instead of `Connection::open`
/// in commands, so that workspaces loaded from saved settings never hit a
/// "unable to open database file" error.
pub fn open_workspace_db(workspace_path: &Path) -> Result<Connection> {
    let state = workspace_path.join("state");
    // Surface filesystem errors (e.g. macOS TCC blocking ~/Documents access)
    // rather than silently swallowing them and letting the SQLite open fail
    // with an opaque "unable to open database file" message.
    if let Err(e) = std::fs::create_dir_all(&state) {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some(format!(
                "Workspace not accessible ({}). Re-select the folder in Settings to grant permission.",
                e
            )),
        ));
    }
    if let Err(e) = std::fs::create_dir_all(workspace_path.join("sources")) {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some(format!(
                "Workspace not accessible ({}). Re-select the folder in Settings to grant permission.",
                e
            )),
        ));
    }
    init_db(state.join("app.sqlite"), 768)
}

pub fn init_db<P: AsRef<Path>>(db_path: P, vec_dim: usize) -> Result<Connection> {
    ensure_vec_extension();
    // SQLite refuses to create the file if the parent directory is missing.
    // Workspaces loaded from saved settings can miss state/ if the folder was
    // moved or never initialized via init_workspace, so make sure it exists.
    if let Some(parent) = db_path.as_ref().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(db_path)?;

    // Set pragmas
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        PRAGMA synchronous = NORMAL;
        "#,
    )?;

    // Create tables
    conn.execute_batch(
        r#"
        -- Source files registry
        CREATE TABLE IF NOT EXISTS source_registry (
            source_id          TEXT PRIMARY KEY,
            internal_filename  TEXT NOT NULL UNIQUE,
            file_size          INTEGER NOT NULL,
            export_date        TEXT,
            imported_at        TEXT NOT NULL,
            conversation_count INTEGER NOT NULL DEFAULT 0,
            original_filename  TEXT
        );

        -- Message-level index (now storing text chunk in SQLite for O(1) reads)
        CREATE TABLE IF NOT EXISTS message_index (
            conversation_id  TEXT NOT NULL,
            message_id       TEXT NOT NULL,
            parent_id        TEXT,
            role             TEXT NOT NULL,
            model            TEXT,
            create_time      REAL,
            byte_start       INTEGER NOT NULL,
            byte_length      INTEGER NOT NULL,
            text_content     BLOB,  -- Zstd compressed text content (new)
            source_id        TEXT NOT NULL,
            on_visible_path  INTEGER NOT NULL DEFAULT 0,
            is_system        INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (conversation_id, message_id),
            FOREIGN KEY (source_id) REFERENCES source_registry(source_id)
        );
        CREATE INDEX IF NOT EXISTS idx_msg_conv ON message_index(conversation_id);
        CREATE INDEX IF NOT EXISTS idx_msg_src ON message_index(source_id);

        -- Conversation-level cache of user-authored messages, materialized
        -- during ingest so KG extraction can read one row instead of
        -- rehydrating every message on demand.
        CREATE TABLE IF NOT EXISTS conversation_user_cache (
            conversation_id   TEXT PRIMARY KEY,
            source_id         TEXT NOT NULL,
            user_messages     BLOB NOT NULL, -- Zstd compressed JSON array
            user_message_count INTEGER NOT NULL DEFAULT 0,
            char_count        INTEGER NOT NULL DEFAULT 0,
            updated_at        TEXT NOT NULL,
            FOREIGN KEY (conversation_id) REFERENCES node(id) ON DELETE CASCADE,
            FOREIGN KEY (source_id) REFERENCES source_registry(source_id)
        );
        CREATE INDEX IF NOT EXISTS idx_conv_user_cache_src
            ON conversation_user_cache(source_id);

        -- Multi-source presence tracking
        CREATE TABLE IF NOT EXISTS conversation_sources (
            conversation_id  TEXT NOT NULL,
            source_id        TEXT NOT NULL,
            update_time      REAL,
            turn_count       INTEGER,
            is_canonical     INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (conversation_id, source_id),
            FOREIGN KEY (source_id) REFERENCES source_registry(source_id)
        );

        -- Graph nodes
        CREATE TABLE IF NOT EXISTS node (
            id        TEXT PRIMARY KEY,
            type      TEXT NOT NULL,          
            name      TEXT NOT NULL,
            props     TEXT,                   
            created   TEXT NOT NULL,
            updated   TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_node_type ON node(type);
        CREATE INDEX IF NOT EXISTS idx_node_name ON node(name);

        -- Graph edges
        CREATE TABLE IF NOT EXISTS edge (
            id        INTEGER PRIMARY KEY AUTOINCREMENT,
            src_id    TEXT NOT NULL,
            dst_id    TEXT NOT NULL,
            type      TEXT NOT NULL,          
            props     TEXT,                   
            FOREIGN KEY (src_id) REFERENCES node(id) ON DELETE CASCADE,
            FOREIGN KEY (dst_id) REFERENCES node(id) ON DELETE CASCADE,
            UNIQUE (src_id, dst_id, type)
        );
        CREATE INDEX IF NOT EXISTS idx_edge_src ON edge(src_id, type);
        CREATE INDEX IF NOT EXISTS idx_edge_dst ON edge(dst_id, type);

        -- Extraction tracking
        CREATE TABLE IF NOT EXISTS extraction_state (
            conversation_id  TEXT PRIMARY KEY,
            status           TEXT NOT NULL,   
            content_hash     TEXT NOT NULL,
            prompt_version   TEXT NOT NULL,
            model_used       TEXT,
            started_at       TEXT,
            completed_at     TEXT,
            error            TEXT,
            FOREIGN KEY (conversation_id) REFERENCES node(id)
        );

        -- Parked items
        CREATE TABLE IF NOT EXISTS parked_extractions (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id  TEXT NOT NULL,
            kind             TEXT NOT NULL,   
            payload          TEXT NOT NULL,   
            confidence       REAL NOT NULL,
            created_at       TEXT NOT NULL,
            FOREIGN KEY (conversation_id) REFERENCES node(id)
        );

        -- Run manifest
        CREATE TABLE IF NOT EXISTS manifest (
            run_id      TEXT PRIMARY KEY,
            ts          TEXT NOT NULL,
            kind        TEXT NOT NULL,        
            parsed      INTEGER,
            skipped     INTEGER,
            errors      INTEGER,
            notes       TEXT
        );

        -- Saved snapshots
        CREATE TABLE IF NOT EXISTS snapshots (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            name       TEXT NOT NULL,
            state      TEXT NOT NULL,         
            created_at TEXT NOT NULL
        );

        -- Editable prompts
        CREATE TABLE IF NOT EXISTS prompts (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            kind        TEXT NOT NULL,        
            version     INTEGER NOT NULL,
            body        TEXT NOT NULL,
            created_at  TEXT NOT NULL,
            is_active   INTEGER NOT NULL DEFAULT 0,
            UNIQUE (kind, version)
        );

        -- Full-text index
        CREATE VIRTUAL TABLE IF NOT EXISTS node_fts USING fts5(
            node_id UNINDEXED,
            name,
            description
        );
        "#,
    )?;

    // Dynamic schema for node_vec
    let create_vec_sql = format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS node_vec USING vec0(
            node_id TEXT PRIMARY KEY,
            embedding FLOAT[{}]
        );",
        vec_dim
    );
    conn.execute_batch(&create_vec_sql)?;

    Ok(conn)
}
