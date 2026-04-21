pub mod db;
pub mod ingest;
pub mod llm;
pub mod kg;
pub mod graph;
pub mod search;
pub mod workbench;

use std::fs;
use std::path::{Path, PathBuf};
use tauri::command;

#[command]
async fn ingest_conversations(
    file_path: String,
    workspace_path: String,
    force: Option<bool>,
) -> Result<String, String> {
    let mut conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;

    ingest::parser::process_export_file(
        &mut conn,
        Path::new(&file_path),
        Path::new(&workspace_path),
        force.unwrap_or(false),
    )
    .map_err(|e: anyhow::Error| e.to_string())
}

/// Drop everything under the workspace except the workspace.yaml and the
/// folder itself. Used by the "Reset workspace" danger button.
#[command]
async fn reset_workspace(workspace_path: String) -> Result<(), String> {
    ingest::parser::reset_workspace_data(Path::new(&workspace_path))
        .map_err(|e| e.to_string())
}

#[command]
async fn check_llm_health(base_url: String, api_key: String) -> Result<llm::HealthStatus, String> {
    use llm::LlmClient;
    let client = llm::openai::OpenAiCompatibleClient::new(base_url, api_key);
    client.health().await.map_err(|e| e.to_string())
}

#[command]
async fn list_available_models(
    base_url: String,
    api_key: String,
) -> Result<Vec<String>, String> {
    use llm::LlmClient;
    let client = llm::openai::OpenAiCompatibleClient::new(base_url, api_key);
    client.list_models().await.map_err(|e| e.to_string())
}

#[command]
async fn extract_conversation(
    conversation_id: String,
    workspace_path: String,
    base_url: String,
    api_key: String,
    model: String,
    embed_model: Option<String>,
    triage_model: Option<String>,
    // Separate client for embeddings. The workspace vec table is dimensioned
    // for 768-dim nomic-embed-text, so embeddings must stay on the endpoint
    // that serves that model — typically local Ollama, even when extraction
    // itself is routed to cloud. Callers MUST supply both fields; we no
    // longer substitute a hardcoded URL / model name if they're missing.
    embed_base_url: Option<String>,
    embed_api_key: Option<String>,
) -> Result<String, String> {
    use llm::LlmClient;

    // Surface clear errors instead of silently substituting hardcoded fallbacks.
    if model.trim().is_empty() {
        return Err("No extraction model selected. Configure it in Settings → LLM Routing.".to_string());
    }
    let embed_model = embed_model
        .and_then(|m| if m.trim().is_empty() { None } else { Some(m) })
        .ok_or_else(|| "No embedding model selected. Configure it in Settings → LLM Routing (Embedding).".to_string())?;
    let embed_base_url = embed_base_url
        .and_then(|u| if u.trim().is_empty() { None } else { Some(u) })
        .ok_or_else(|| "No embedding endpoint configured. Configure Local LLM in Settings.".to_string())?;
    // Triage shares the extraction client; if the caller doesn't pick one
    // explicitly, reuse the extraction model — that's the only model guaranteed
    // to work on this client, and it's derived from the user's selection, not
    // a hardcoded value.
    let triage = triage_model
        .and_then(|m| if m.trim().is_empty() { None } else { Some(m) })
        .unwrap_or_else(|| model.clone());

    let chat_client = llm::openai::OpenAiCompatibleClient::new(base_url, api_key);
    let embed_client = llm::openai::OpenAiCompatibleClient::new(
        embed_base_url,
        embed_api_key.unwrap_or_default(),
    );

    ingest::extraction::extract_single_conversation(
        Path::new(&workspace_path),
        &chat_client as &dyn LlmClient,
        &embed_client as &dyn LlmClient,
        &model,
        &triage,
        &embed_model,
        &conversation_id,
    )
    .await
    .map_err(|e| e.to_string())?;

    Ok(format!("Extracted conversation {}", conversation_id))
}

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
pub struct WorkspaceConfig {
    pub endpoints: WorkspaceEndpoints,
    pub jobs: WorkspaceJobs,
    pub ui: WorkspaceUI,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct WorkspaceEndpoints {
    pub default: EndpointConfig,
    pub cloud: EndpointConfig,
}

impl Default for WorkspaceEndpoints {
    fn default() -> Self {
        Self {
            default: EndpointConfig {
                base_url: "http://localhost:11434/v1".to_string(),
                api_key: "".to_string(),
                label: "Local Ollama".to_string(),
                provider: "local".to_string(),
            },
            cloud: EndpointConfig {
                base_url: "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
                api_key: "".to_string(),
                label: "Google Gemini".to_string(),
                provider: "gemini".to_string(),
            },
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
pub struct EndpointConfig {
    pub base_url: String,
    pub api_key: String,
    pub label: String,
    #[serde(default)]
    pub provider: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct WorkspaceJobs {
    pub extraction: JobConfig,
    pub embedding: JobConfig,
    pub triage_preview: JobConfig,
    pub evaluation: JobConfig,
    pub chat: JobConfig,
}

impl Default for WorkspaceJobs {
    fn default() -> Self {
        Self {
            extraction: JobConfig { endpoint: "default".into(), model: "qwen2.5:7b".into(), backup_endpoint: String::new(), backup_model: String::new() },
            embedding: JobConfig { endpoint: "default".into(), model: "nomic-embed-text".into(), backup_endpoint: String::new(), backup_model: String::new() },
            triage_preview: JobConfig { endpoint: "default".into(), model: "qwen2.5:3b".into(), backup_endpoint: String::new(), backup_model: String::new() },
            evaluation: JobConfig { endpoint: "default".into(), model: "qwen2.5:7b".into(), backup_endpoint: String::new(), backup_model: String::new() },
            chat: JobConfig { endpoint: "default".into(), model: "qwen2.5:7b".into(), backup_endpoint: String::new(), backup_model: String::new() },
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
pub struct JobConfig {
    pub endpoint: String,
    pub model: String,
    /// Optional explicit backup endpoint. Empty string / missing field = no
    /// backup (strict: a failure on `endpoint` is a hard failure). When set,
    /// the worker tries `endpoint` first, then `backup_endpoint`.
    #[serde(default)]
    pub backup_endpoint: String,
    #[serde(default)]
    pub backup_model: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct WorkspaceUI {
    pub split_ratio: f64,
    pub graph_node_cap: i64,
    pub focus_stack_size: i64,
}

impl Default for WorkspaceUI {
    fn default() -> Self {
        Self {
            split_ratio: 0.6,
            graph_node_cap: 25,
            focus_stack_size: 4,
        }
    }
}

pub(crate) fn read_workspace_config(workspace_dir: &Path) -> WorkspaceConfig {
    let yaml_path = workspace_dir.join("workspace.yaml");
    if let Ok(yaml) = fs::read_to_string(&yaml_path) {
        if let Ok(config) = serde_yaml::from_str(&yaml) {
            return config;
        }
    }
    // Fallback if missing or unparsable
    WorkspaceConfig::default()
}

#[command]
async fn get_workspace_config(workspace_path: String) -> Result<WorkspaceConfig, String> {
    Ok(read_workspace_config(Path::new(&workspace_path)))
}

#[command]
async fn update_workspace_config(workspace_path: String, config: WorkspaceConfig) -> Result<(), String> {
    let yaml_path = Path::new(&workspace_path).join("workspace.yaml");
    let yaml = serde_yaml::to_string(&config).map_err(|e| e.to_string())?;
    fs::write(&yaml_path, yaml).map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(serde::Serialize)]
struct HomeTile {
    id: String,
    name: String,
    kind: String,
}

#[derive(serde::Serialize)]
struct HomeData {
    recent: Vec<HomeTile>,
    orphan_count: i64,
    orphan_since: String,
    snapshots: Vec<HomeTile>,
    growing: Vec<HomeTile>,
}

#[command]
async fn get_home_tiles(workspace_path: String) -> Result<HomeData, String> {
    let conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;

    let mut recent = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT id, name FROM node WHERE type = 'concept' ORDER BY created DESC LIMIT 5",
    ) {
        let iter = stmt.query_map([], |r| {
            Ok(HomeTile {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: "concept".into(),
            })
        });
        if let Ok(iter) = iter {
            for t in iter.flatten() {
                recent.push(t);
            }
        }
    }

    let orphan_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (
                SELECT c.id
                FROM node c
                JOIN edge e ON e.dst_id = c.id AND e.type = 'discusses'
                WHERE c.type = 'concept' AND c.created < datetime('now', '-90 days')
                GROUP BY c.id
                HAVING COUNT(DISTINCT e.src_id) = 1
            )",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let mut snapshots = Vec::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT id, name FROM snapshots ORDER BY created_at DESC LIMIT 10")
    {
        let iter = stmt.query_map([], |r| {
            Ok(HomeTile {
                id: r.get::<_, i64>(0)?.to_string(),
                name: r.get(1)?,
                kind: "snapshot".into(),
            })
        });
        if let Ok(iter) = iter {
            for t in iter.flatten() {
                snapshots.push(t);
            }
        }
    }

    let mut growing = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT t.id, t.name
         FROM node t
         JOIN edge e ON e.dst_id = t.id AND e.type = 'belongs_to'
         WHERE t.type = 'topic'
         GROUP BY t.id
         ORDER BY COUNT(DISTINCT e.src_id) DESC
         LIMIT 3",
    ) {
        let iter = stmt.query_map([], |r| {
            Ok(HomeTile {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: "topic".into(),
            })
        });
        if let Ok(iter) = iter {
            for t in iter.flatten() {
                growing.push(t);
            }
        }
    }

    Ok(HomeData {
        recent,
        orphan_count,
        orphan_since: chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::days(90))
            .map(|d| d.format("%b %Y").to_string())
            .unwrap_or_default(),
        snapshots,
        growing,
    })
}

#[command]
async fn save_snapshot(
    workspace_path: String,
    name: String,
    state: String,
) -> Result<i64, String> {
    let conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO snapshots (name, state, created_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![name, state, chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

#[command]
async fn load_snapshot(workspace_path: String, id: i64) -> Result<String, String> {
    let conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT state FROM snapshots WHERE id = ?1",
        rusqlite::params![id],
        |r| r.get::<_, String>(0),
    )
    .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
struct WorkspaceStatus {
    conversations: i64,
    messages: i64,
    nodes: i64,
    concepts: i64,
    topics: i64,
    entities: i64,
    pending: i64,
    processing: i64,
    done: i64,
    failed: i64,
    parked: i64,
}

#[command]
async fn get_workspace_status(workspace_path: String) -> Result<WorkspaceStatus, String> {
    let conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    let scalar = |sql: &str| -> i64 {
        conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0)
    };
    Ok(WorkspaceStatus {
        conversations: scalar("SELECT COUNT(*) FROM node WHERE type='conversation'"),
        messages: scalar("SELECT COUNT(*) FROM message_index"),
        nodes: scalar("SELECT COUNT(*) FROM node"),
        concepts: scalar("SELECT COUNT(*) FROM node WHERE type='concept'"),
        topics: scalar("SELECT COUNT(*) FROM node WHERE type='topic'"),
        entities: scalar("SELECT COUNT(*) FROM node WHERE type='entity'"),
        pending: scalar("SELECT COUNT(*) FROM extraction_state WHERE status='pending'"),
        processing: scalar("SELECT COUNT(*) FROM extraction_state WHERE status='processing'"),
        done: scalar("SELECT COUNT(*) FROM extraction_state WHERE status='done'"),
        failed: scalar("SELECT COUNT(*) FROM extraction_state WHERE status='failed'"),
        parked: scalar("SELECT COUNT(*) FROM parked_extractions"),
    })
}

#[command]
async fn list_pending_extractions(workspace_path: String) -> Result<Vec<String>, String> {
    let conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    // 'skipped' stays out of the worker queue — user decided to stop retrying.
    // 'failed' items DO come back so Sync all retries them.
    let mut stmt = conn
        .prepare(
            "SELECT es.conversation_id
             FROM extraction_state es
             LEFT JOIN node n ON n.id = es.conversation_id
             WHERE es.status IN ('pending','stale','failed')
             ORDER BY
               CAST(coalesce(json_extract(n.props, '$.turn_count'), 999999) AS INTEGER) ASC,
               coalesce(json_extract(n.props, '$.chatgpt_updated'), 0) DESC,
               es.conversation_id",
        )
        .map_err(|e| e.to_string())?;
    let ids: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(ids)
}

#[derive(serde::Serialize)]
struct FailedExtraction {
    id: String,
    title: String,
    error: String,
}

#[command]
async fn list_failed_extractions(
    workspace_path: String,
) -> Result<Vec<FailedExtraction>, String> {
    let conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT es.conversation_id, coalesce(n.name, '(untitled)'), coalesce(es.error, '')
             FROM extraction_state es
             LEFT JOIN node n ON n.id = es.conversation_id
             WHERE es.status = 'failed'
             ORDER BY es.completed_at DESC
             LIMIT 200",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<FailedExtraction> = stmt
        .query_map([], |r| {
            Ok(FailedExtraction {
                id: r.get(0)?,
                title: r.get(1)?,
                error: r.get(2)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

#[command]
async fn skip_extraction(workspace_path: String, conversation_id: String) -> Result<(), String> {
    let conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE extraction_state SET status='skipped', error=NULL WHERE conversation_id=?1",
        rusqlite::params![conversation_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
async fn retry_extraction(workspace_path: String, conversation_id: String) -> Result<(), String> {
    // Flip status back to 'pending' so the next sync picks it up.
    let conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE extraction_state SET status='pending', error=NULL WHERE conversation_id=?1",
        rusqlite::params![conversation_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn retry_all_failed_extractions_impl(workspace_path: &Path) -> Result<i64, String> {
    let conn = db::open_workspace_db(workspace_path)
        .map_err(|e| e.to_string())?;
    let changed = conn
        .execute(
            "UPDATE extraction_state
             SET status='pending', error=NULL
             WHERE status='failed'",
            [],
        )
        .map_err(|e| e.to_string())?;
    Ok(changed as i64)
}

#[derive(serde::Serialize)]
pub struct NodeNeighbor {
    pub id: String,
    pub name: String,
    pub node_type: String,
    pub edge_type: String,
    pub direction: &'static str, // "incoming" (edge ends here) or "outgoing" (edge starts here)
}

#[derive(serde::Serialize)]
pub struct NodeDetail {
    pub id: String,
    pub name: String,
    pub node_type: String,
    pub description: Option<String>,
    pub tier: Option<String>,
    pub confidence: Option<f64>,
    pub incoming_count: i64,
    pub outgoing_count: i64,
    pub neighbors: Vec<NodeNeighbor>,
}

/// Return everything the UI needs to let the user *explore* a node's
/// connections: its own metadata, neighbor counts, and the top neighbors
/// split into incoming / outgoing by edge direction.
#[command]
async fn get_node_detail(
    workspace_path: String,
    node_id: String,
) -> Result<NodeDetail, String> {
    let conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;

    let (name, node_type, props_json): (String, String, Option<String>) = conn
        .query_row(
            "SELECT name, type, props FROM node WHERE id = ?1",
            rusqlite::params![node_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| format!("Node not found: {}", e))?;

    let (description, tier, confidence) = match props_json.as_deref() {
        Some(s) if !s.is_empty() => {
            let v: serde_json::Value = serde_json::from_str(s).unwrap_or_default();
            (
                v.get("description").and_then(|x| x.as_str()).map(String::from),
                v.get("tier").and_then(|x| x.as_str()).map(String::from),
                v.get("confidence").and_then(|x| x.as_f64()),
            )
        }
        _ => (None, None, None),
    };

    let incoming_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edge WHERE dst_id = ?1",
            rusqlite::params![node_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let outgoing_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edge WHERE src_id = ?1",
            rusqlite::params![node_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let mut neighbors: Vec<NodeNeighbor> = Vec::new();

    // Incoming: things that point AT this node. For an entity, this is every
    // conversation that mentions it (edge type = 'mentions', src = conversation).
    let mut stmt_in = conn
        .prepare(
            "SELECT n.id, n.name, n.type, e.type
             FROM edge e
             JOIN node n ON n.id = e.src_id
             WHERE e.dst_id = ?1
             ORDER BY n.updated DESC
             LIMIT 50",
        )
        .map_err(|e| e.to_string())?;
    let rows_in = stmt_in
        .query_map(rusqlite::params![node_id], |r| {
            Ok(NodeNeighbor {
                id: r.get(0)?,
                name: r.get(1)?,
                node_type: r.get(2)?,
                edge_type: r.get(3)?,
                direction: "incoming",
            })
        })
        .map_err(|e| e.to_string())?;
    for n in rows_in.flatten() {
        neighbors.push(n);
    }

    // Outgoing: things this node points AT. For a conversation, these are
    // the topics / concepts / entities it discusses.
    let mut stmt_out = conn
        .prepare(
            "SELECT n.id, n.name, n.type, e.type
             FROM edge e
             JOIN node n ON n.id = e.dst_id
             WHERE e.src_id = ?1
             ORDER BY n.updated DESC
             LIMIT 50",
        )
        .map_err(|e| e.to_string())?;
    let rows_out = stmt_out
        .query_map(rusqlite::params![node_id], |r| {
            Ok(NodeNeighbor {
                id: r.get(0)?,
                name: r.get(1)?,
                node_type: r.get(2)?,
                edge_type: r.get(3)?,
                direction: "outgoing",
            })
        })
        .map_err(|e| e.to_string())?;
    for n in rows_out.flatten() {
        neighbors.push(n);
    }

    Ok(NodeDetail {
        id: node_id,
        name,
        node_type,
        description,
        tier,
        confidence,
        incoming_count,
        outgoing_count,
        neighbors,
    })
}

/// Scope of a knowledge-graph rebuild. `refresh` only re-runs extraction for
/// conversations that haven't been processed yet. `rebuild_all` wipes every
/// derived node (topic/entity/concept/pattern) and re-queues every
/// conversation — conversation nodes and the ingested message store are
/// untouched, so you don't have to re-import the export.
#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KgRebuildScope {
    Refresh,
    RebuildAll,
}

#[derive(serde::Serialize)]
pub struct KgRebuildResult {
    pub scope: &'static str,
    pub derived_nodes_deleted: i64,
    pub edges_deleted: i64,
    pub parked_deleted: i64,
    pub conversations_requeued: i64,
}

#[command]
async fn rebuild_knowledge_graph(
    workspace_path: String,
    scope: KgRebuildScope,
) -> Result<KgRebuildResult, String> {
    let mut conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    match scope {
        KgRebuildScope::Refresh => {
            // Pull back failed + skipped items so the next Sync All picks them up,
            // but don't touch anything extraction already completed successfully.
            let requeued = tx
                .execute(
                    "UPDATE extraction_state
                     SET status='pending', error=NULL
                     WHERE status IN ('failed', 'skipped', 'stale')",
                    [],
                )
                .map_err(|e| e.to_string())? as i64;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(KgRebuildResult {
                scope: "refresh",
                derived_nodes_deleted: 0,
                edges_deleted: 0,
                parked_deleted: 0,
                conversations_requeued: requeued,
            })
        }
        KgRebuildScope::RebuildAll => {
            // 1. Wipe every edge that points at a derived node. Edges between
            //    two conversations don't exist in the current schema, so this
            //    is effectively all non-conversation-related edges.
            let edges_deleted = tx
                .execute(
                    "DELETE FROM edge
                     WHERE src_id IN (SELECT id FROM node WHERE type != 'conversation')
                        OR dst_id IN (SELECT id FROM node WHERE type != 'conversation')",
                    [],
                )
                .map_err(|e| e.to_string())? as i64;

            // 2. Drop the derived nodes' vector rows and FTS rows before the
            //    node delete (foreign-key-free virtual tables don't cascade).
            tx.execute(
                "DELETE FROM node_vec WHERE node_id IN (SELECT id FROM node WHERE type != 'conversation')",
                [],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "DELETE FROM node_fts WHERE node_id IN (SELECT id FROM node WHERE type != 'conversation')",
                [],
            )
            .map_err(|e| e.to_string())?;

            // 3. Delete the derived nodes themselves.
            let derived_nodes_deleted = tx
                .execute("DELETE FROM node WHERE type != 'conversation'", [])
                .map_err(|e| e.to_string())? as i64;

            // 4. Clear parked items — they're per-conversation proposals that
            //    must be regenerated by the fresh run.
            let parked_deleted = tx
                .execute("DELETE FROM parked_extractions", [])
                .map_err(|e| e.to_string())? as i64;

            // 5. Requeue every conversation that's ever been seen by extraction.
            //    Rows without an extraction_state row get inserted as pending.
            let updated = tx
                .execute(
                    "UPDATE extraction_state
                     SET status='pending', error=NULL, started_at=NULL, completed_at=NULL",
                    [],
                )
                .map_err(|e| e.to_string())? as i64;
            let inserted = tx
                .execute(
                    "INSERT INTO extraction_state (conversation_id, status, content_hash, prompt_version, model_used, started_at)
                     SELECT n.id, 'pending', '', '1', NULL, NULL
                     FROM node n
                     LEFT JOIN extraction_state es ON es.conversation_id = n.id
                     WHERE n.type = 'conversation' AND es.conversation_id IS NULL",
                    [],
                )
                .map_err(|e| e.to_string())? as i64;

            tx.commit().map_err(|e| e.to_string())?;
            Ok(KgRebuildResult {
                scope: "rebuild_all",
                derived_nodes_deleted,
                edges_deleted,
                parked_deleted,
                conversations_requeued: updated + inserted,
            })
        }
    }
}

#[command]
async fn retry_all_failed_extractions(workspace_path: String) -> Result<i64, String> {
    retry_all_failed_extractions_impl(Path::new(&workspace_path))
}

/// Overwrite the error text on a failed extraction. The worker uses this after
/// trying primary + fallback endpoints so the "Failed extractions" list shows
/// *why* both attempts failed, not just the last one.
#[command]
async fn record_extraction_failure(
    workspace_path: String,
    conversation_id: String,
    error: String,
) -> Result<(), String> {
    let conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE extraction_state SET status='failed', error=?1, completed_at=?2 WHERE conversation_id=?3",
        rusqlite::params![error, chrono::Utc::now().to_rfc3339(), conversation_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Rows stuck in 'processing' are orphans from a prior run that was stopped
/// (user clicked Stop, app quit, LLM hung past timeout). They're not actually
/// in flight — roll them back to 'pending' so the next sync retries them.
#[command]
async fn reset_stuck_processing(workspace_path: String) -> Result<i64, String> {
    let conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    let changed = conn
        .execute(
            "UPDATE extraction_state SET status='pending', started_at=NULL, error=NULL WHERE status='processing'",
            [],
        )
        .map_err(|e| e.to_string())?;
    Ok(changed as i64)
}

#[command]
async fn get_initial_graph(workspace_path: String) -> Result<graph::GraphData, String> {
    let conn = db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    
    graph::get_initial_graph(&conn)
}

#[command]
async fn init_workspace(path: String) -> Result<String, String> {
    let workspace_dir = PathBuf::from(&path);
    
    // Create folders
    let sources_dir = workspace_dir.join("sources");
    let state_dir = workspace_dir.join("state");
    
    fs::create_dir_all(&sources_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&state_dir).map_err(|e| e.to_string())?;
    
    // Create DB
    let db_path = state_dir.join("app.sqlite");
    
    // Default config using nomic-embed-text dimensionality (768)
    // Could be parameterized later if user selects different model
    let vec_dim = 768; 
    
    db::init_db(&db_path, vec_dim).map_err(|e| e.to_string())?;

    // Create default workspace.yaml if not exists
    let yaml_path = workspace_dir.join("workspace.yaml");
    if !yaml_path.exists() {
        let config = WorkspaceConfig::default();
        let yaml = serde_yaml::to_string(&config).map_err(|e| e.to_string())?;
        fs::write(&yaml_path, yaml).map_err(|e| e.to_string())?;
    }
    
    Ok(path)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            init_workspace, ingest_conversations, reset_workspace, check_llm_health,
            list_available_models,
            extract_conversation, list_pending_extractions, list_failed_extractions,
            skip_extraction, retry_extraction, retry_all_failed_extractions, rebuild_knowledge_graph,
            get_node_detail,
            record_extraction_failure, reset_stuck_processing, get_initial_graph,
            get_home_tiles, save_snapshot, load_snapshot, get_workspace_config,
            update_workspace_config,
            get_workspace_status,
            search::search_nodes, search::lens_territory, search::lens_drift,
            search::lens_drift_data, search::lens_bridges, search::lens_orphans,
            search::lens_path, search::orphan_promote, search::orphan_archive,
            search::ask_question,
            workbench::get_conversation_workbench, workbench::approve_parked_extraction, workbench::reject_parked_extraction
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
