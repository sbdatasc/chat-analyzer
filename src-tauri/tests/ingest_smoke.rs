use std::fs;
use std::path::PathBuf;
use std::collections::VecDeque;
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use cairn_lib::llm::{ChatOpts, ChatResponse, HealthStatus, LlmClient, Message};
use zstd::stream::encode_all;

fn temp_workspace() -> PathBuf {
    let base = std::env::temp_dir().join(format!("cairn-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(base.join("sources")).unwrap();
    fs::create_dir_all(base.join("state")).unwrap();
    base
}

fn seed_conversation(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    title: &str,
    message_id: &str,
    text: &str,
) {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO source_registry (source_id, internal_filename, file_size, imported_at, conversation_count, original_filename)
         VALUES (?1, ?2, 0, ?3, 1, 'fixture.json')",
        rusqlite::params!["src-1", "fixture.json", now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO node (id, type, name, props, created, updated)
         VALUES (?1, 'conversation', ?2, '{}', ?3, ?3)",
        rusqlite::params![conversation_id, title, now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message_index (
            conversation_id, message_id, role, create_time, byte_start, byte_length, text_content,
            source_id, on_visible_path, is_system
         ) VALUES (?1, ?2, 'user', 1.0, 0, 0, ?3, 'src-1', 1, 0)",
        rusqlite::params![
            conversation_id,
            message_id,
            encode_all(text.as_bytes(), 3).unwrap()
        ],
    )
    .unwrap();
}

fn embedding_for(input: &str) -> Vec<f32> {
    let mut v = vec![0.0_f32; 768];
    let slot = match input {
        s if s.contains("old-concept") => 0,
        s if s.contains("old-topic") => 1,
        s if s.contains("new-concept") => 2,
        s if s.contains("research-ops") => 3,
        _ => 4,
    };
    v[slot] = 1.0;
    v
}

struct MockLlm {
    chat_responses: Mutex<VecDeque<String>>,
}

impl MockLlm {
    fn with_chat(responses: Vec<String>) -> Self {
        Self {
            chat_responses: Mutex::new(responses.into()),
        }
    }
}

#[async_trait]
impl LlmClient for MockLlm {
    async fn chat(&self, _model: &str, _messages: Vec<Message>, _opts: ChatOpts) -> Result<ChatResponse> {
        let mut guard = self.chat_responses.lock().unwrap();
        let content = guard
            .pop_front()
            .ok_or_else(|| anyhow!("no scripted chat response"))?;
        Ok(ChatResponse {
            message: Message {
                role: "assistant".to_string(),
                content,
            },
        })
    }

    async fn embed(&self, _model: &str, input: &str) -> Result<Vec<f32>> {
        Ok(embedding_for(input))
    }

    async fn embed_many(&self, _model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(inputs.iter().map(|s| embedding_for(s)).collect())
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        Ok(vec!["mock".to_string()])
    }

    async fn health(&self) -> Result<HealthStatus> {
        Ok(HealthStatus {
            chat: true,
            embeddings: true,
            models: true,
            json_mode: true,
            models_error: None,
            chat_error: None,
            embeddings_error: None,
        })
    }
}

#[test]
fn end_to_end_ingest_populates_tables() {
    let workspace = temp_workspace();
    let db_path = workspace.join("state").join("app.sqlite");

    let mut conn = cairn_lib::db::init_db(&db_path, 768).expect("init_db");

    // Copy fixture to a temp location the ingester can consume.
    let src_fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("small-export.json");
    let staging = workspace.join("upload.json");
    fs::copy(&src_fixture, &staging).expect("stage fixture");

    let source_id = cairn_lib::ingest::parser::process_export_file(
        &mut conn,
        &staging,
        &workspace,
        false,
    )
    .expect("process_export_file");
    assert_eq!(source_id.len(), 64, "source_id is sha256 hex");

    // File copied into sources/; the user's original stays in place.
    let sources: Vec<_> = fs::read_dir(workspace.join("sources"))
        .unwrap()
        .collect();
    assert_eq!(sources.len(), 1, "exactly one file copied to sources/");
    assert!(staging.exists(), "original upload is preserved after ingest");

    // source_registry row
    let (count_sr, count_conv, count_msg): (i64, i64, i64) = {
        let sr: i64 = conn
            .query_row("SELECT COUNT(*) FROM source_registry", [], |r| r.get(0))
            .unwrap();
        let conv: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM node WHERE type='conversation'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let msg: i64 = conn
            .query_row("SELECT COUNT(*) FROM message_index", [], |r| r.get(0))
            .unwrap();
        (sr, conv, msg)
    };
    assert_eq!(count_sr, 1);
    assert_eq!(count_conv, 2, "two conversation nodes");
    assert_eq!(count_msg, 3, "only user-authored messages are indexed");

    // Visible-path walk covers the user-authored visible path.
    let visible: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM message_index WHERE on_visible_path = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(visible, 3, "visible_path should cover the 3 stored user messages");

    // User-only ingest means no system rows are stored.
    let system: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM message_index WHERE is_system = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(system, 0, "system messages are excluded from the archive");
    let non_user: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM message_index WHERE role != 'user'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(non_user, 0, "assistant/system rows are not stored");

    // extraction_state has pending rows
    let pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM extraction_state WHERE status = 'pending'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 2, "both conversations queued for extraction");

    // Re-import same file → rejected unless force=true.
    fs::copy(&src_fixture, &staging).expect("stage again");
    let second = cairn_lib::ingest::parser::process_export_file(
        &mut conn,
        &staging,
        &workspace,
        false,
    );
    assert!(second.is_err(), "second ingest of same checksum is rejected");

    // With force=true the purge+reimport path runs and succeeds.
    fs::copy(&src_fixture, &staging).expect("stage again for force");
    let third = cairn_lib::ingest::parser::process_export_file(
        &mut conn,
        &staging,
        &workspace,
        true,
    );
    assert!(third.is_ok(), "force=true re-imports after purging");

    // After override there's still exactly one source and two conversations.
    let sr_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM source_registry", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sr_count, 1, "override leaves a single source_registry row");
    let conv_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM node WHERE type='conversation'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(conv_count, 2, "override rebuilds the conversations");
}

#[test]
fn reset_workspace_clears_data_and_leaves_schema() {
    let workspace = temp_workspace();
    let db_path = workspace.join("state").join("app.sqlite");
    let mut conn = cairn_lib::db::init_db(&db_path, 768).expect("init_db");

    let src_fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("small-export.json");
    let staging = workspace.join("upload.json");
    fs::copy(&src_fixture, &staging).expect("stage");
    cairn_lib::ingest::parser::process_export_file(&mut conn, &staging, &workspace, false)
        .expect("ingest");
    drop(conn);

    cairn_lib::ingest::parser::reset_workspace_data(&workspace).expect("reset");

    // Sources folder is empty.
    let sources: Vec<_> = fs::read_dir(workspace.join("sources"))
        .unwrap()
        .collect();
    assert!(sources.is_empty(), "reset clears sources/");

    // Schema is recreated; tables exist but empty.
    let conn2 = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn2
        .query_row("SELECT COUNT(*) FROM source_registry", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    let nodes: i64 = conn2
        .query_row("SELECT COUNT(*) FROM node", [], |r| r.get(0))
        .unwrap();
    assert_eq!(nodes, 0);
}

#[test]
fn vec0_match_roundtrip() {
    // Prove that sqlite-vec is actually registered and MATCH works for KNN.
    // This is the core of the semantic dedup + vector-retrieval paths.
    let workspace = temp_workspace();
    let db_path = workspace.join("state").join("app.sqlite");
    let conn = cairn_lib::db::init_db(&db_path, 4).expect("init_db");

    let a: [f32; 4] = [1.0, 0.0, 0.0, 0.0];
    let b: [f32; 4] = [0.0, 1.0, 0.0, 0.0];
    let c: [f32; 4] = [0.99, 0.01, 0.0, 0.0]; // near a

    let as_bytes = |v: &[f32]| -> Vec<u8> {
        let mut out = Vec::with_capacity(v.len() * 4);
        for x in v {
            out.extend_from_slice(&x.to_le_bytes());
        }
        out
    };

    conn.execute(
        "INSERT INTO node_vec (node_id, embedding) VALUES (?1, ?2)",
        rusqlite::params!["a", as_bytes(&a)],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO node_vec (node_id, embedding) VALUES (?1, ?2)",
        rusqlite::params!["b", as_bytes(&b)],
    )
    .unwrap();

    let mut stmt = conn
        .prepare(
            "SELECT node_id, distance FROM node_vec
             WHERE embedding MATCH ?1 AND k = 2
             ORDER BY distance ASC",
        )
        .expect("prepare MATCH");
    let rows: Vec<(String, f64)> = stmt
        .query_map(rusqlite::params![as_bytes(&c)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "a", "nearest neighbor should be 'a'");
    assert!(rows[0].1 < rows[1].1, "distances are ordered");
}

#[test]
fn lens_territory_runs_against_empty_graph() {
    // Initialize a fresh DB and make sure the Territory lens SQL is valid
    // even when there are no topics (should return zero rows, not error).
    let workspace = temp_workspace();
    let db_path = workspace.join("state").join("app.sqlite");
    let _conn = cairn_lib::db::init_db(&db_path, 768).expect("init_db");

    // Territory issues the canonical SQL; we just verify prepare + query succeed.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let rows: Vec<String> = conn
        .prepare(
            "SELECT t.id FROM node t
             JOIN edge e ON e.dst_id = t.id AND e.type = 'belongs_to'
             JOIN node c ON c.id = e.src_id AND c.type = 'conversation'
             WHERE t.type = 'topic'
             GROUP BY t.id
             ORDER BY COUNT(DISTINCT e.src_id) DESC
             LIMIT 20",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    assert_eq!(rows.len(), 0);
}

#[tokio::test]
async fn retry_all_failed_moves_failed_rows_back_to_pending() {
    let workspace = temp_workspace();
    let db_path = workspace.join("state").join("app.sqlite");
    let conn = cairn_lib::db::init_db(&db_path, 768).expect("init_db");
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO node (id, type, name, props, created, updated)
         VALUES ('failed-1', 'conversation', 'Failed 1', '{}', ?1, ?1)",
        rusqlite::params![now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO node (id, type, name, props, created, updated)
         VALUES ('failed-2', 'conversation', 'Failed 2', '{}', ?1, ?1)",
        rusqlite::params![now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO node (id, type, name, props, created, updated)
         VALUES ('done-1', 'conversation', 'Done 1', '{}', ?1, ?1)",
        rusqlite::params![now],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO extraction_state (conversation_id, status, content_hash, prompt_version, completed_at, error)
         VALUES ('failed-1', 'failed', 'h1', '1', ?1, 'bad json')",
        rusqlite::params![now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO extraction_state (conversation_id, status, content_hash, prompt_version, completed_at, error)
         VALUES ('failed-2', 'failed', 'h2', '1', ?1, 'timeout')",
        rusqlite::params![now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO extraction_state (conversation_id, status, content_hash, prompt_version)
         VALUES ('done-1', 'done', 'h3', '1')",
        [],
    )
    .unwrap();
    drop(conn);

    let retried = cairn_lib::retry_all_failed_extractions_impl(&workspace)
        .expect("retry all failed");
    assert_eq!(retried, 2);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM extraction_state WHERE status = 'pending'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 2);
    let remaining_failed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM extraction_state WHERE status = 'failed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining_failed, 0);
}

#[tokio::test]
async fn re_extract_replaces_previous_graph_state() {
    let workspace = temp_workspace();
    let db_path = workspace.join("state").join("app.sqlite");
    let conn = cairn_lib::db::init_db(&db_path, 768).expect("init_db");
    let conversation_id = "conv-reextract";
    seed_conversation(
        &conn,
        conversation_id,
        "Reextract Conversation",
        "m1",
        &"a".repeat(400),
    );
    drop(conn);

    let first = MockLlm::with_chat(vec![r#"{
        "topics":[{"name":"Old Topic","confidence":0.9,"message_ids":["m1"]}],
        "entities":[],
        "concepts":[{"name":"Old Concept","description":"first pass","confidence":0.9,"message_ids":["m1"]}],
        "prompt_patterns":[]
    }"#.to_string()]);
    cairn_lib::ingest::extraction::extract_single_conversation(
        &workspace,
        &first,
        &first,
        "mock",
        "mock",
        "mock-embed",
        conversation_id,
    )
    .await
    .expect("first extraction");

    let second = MockLlm::with_chat(vec![r#"{
        "topics":[],
        "entities":[],
        "concepts":[{"name":"New Concept","description":"second pass","confidence":0.95,"message_ids":["m1"]}],
        "prompt_patterns":[]
    }"#.to_string()]);
    cairn_lib::ingest::extraction::extract_single_conversation(
        &workspace,
        &second,
        &second,
        "mock",
        "mock",
        "mock-embed",
        conversation_id,
    )
    .await
    .expect("second extraction");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let derived_names: Vec<String> = conn
        .prepare("SELECT name FROM node WHERE type != 'conversation' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    assert_eq!(derived_names, vec!["new-concept".to_string()]);

    let edge_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edge WHERE src_id = ?1",
            rusqlite::params![conversation_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(edge_count, 1, "old edges should be cleared before re-insert");

    let vec_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM node_vec", [], |r| r.get(0))
        .unwrap();
    assert_eq!(vec_count, 1, "orphaned embeddings should be removed");

    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM node_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fts_count, 1, "orphaned FTS rows should be removed");
}

#[tokio::test]
async fn approving_parked_item_uses_canonical_graph_rules() {
    let workspace = temp_workspace();
    let db_path = workspace.join("state").join("app.sqlite");
    let conn = cairn_lib::db::init_db(&db_path, 768).expect("init_db");
    let conversation_id = "conv-workbench";
    seed_conversation(
        &conn,
        conversation_id,
        "Workbench Conversation",
        "m1",
        &"b".repeat(400),
    );
    conn.execute(
        "INSERT INTO parked_extractions (conversation_id, kind, payload, confidence, created_at)
         VALUES (?1, 'topic', ?2, 0.42, ?3)",
        rusqlite::params![
            conversation_id,
            r#"{"name":"Research Ops","description":"coordination","confidence":0.42,"message_ids":["m1"]}"#,
            chrono::Utc::now().to_rfc3339()
        ],
    )
    .unwrap();
    let parked_id = conn.last_insert_rowid();
    drop(conn);

    let mock = MockLlm::with_chat(vec![]);
    cairn_lib::ingest::extraction::approve_parked_payload(
        &workspace,
        parked_id,
        conversation_id,
        "topic",
        r#"{"name":"Research Ops","description":"coordination","confidence":0.42,"message_ids":["m1"]}"#,
        0.42,
        &mock,
        "mock-embed",
    )
    .await
    .expect("approve parked");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let node_id: String = conn
        .query_row(
            "SELECT id FROM node WHERE name = 'research-ops'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let edge_type: String = conn
        .query_row(
            "SELECT type FROM edge WHERE src_id = ?1 AND dst_id = ?2",
            rusqlite::params![conversation_id, &node_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(edge_type, "belongs_to");

    let created: String = conn
        .query_row("SELECT created FROM node WHERE id = ?1", rusqlite::params![&node_id], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        created.contains('T'),
        "approved nodes should keep RFC3339-style timestamps"
    );

    let parked_remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM parked_extractions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(parked_remaining, 0);

    let vec_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM node_vec", [], |r| r.get(0))
        .unwrap();
    assert_eq!(vec_count, 1, "approved nodes should be indexed for vector search");

    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM node_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fts_count, 1, "approved nodes should be indexed for FTS");
}
