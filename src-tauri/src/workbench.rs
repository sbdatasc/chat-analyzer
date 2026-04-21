use std::path::Path;
use rusqlite::params;
use tauri::command;
use serde::Serialize;

#[derive(Serialize)]
pub struct WorkbenchMessage {
    pub message_id: String,
    pub role: String,
    pub text: String,
}

#[derive(Serialize)]
pub struct WorkbenchNode {
    pub id: String,
    pub node_type: String, // e.g., 'topic', 'entity'
    pub name: String,
}

#[derive(Serialize)]
pub struct WorkbenchParkedItem {
    pub id: i64,
    pub kind: String,
    pub payload: String, // Stringified JSON of the parsed item
    pub confidence: f64,
}

#[derive(Serialize)]
pub struct WorkbenchData {
    pub messages: Vec<WorkbenchMessage>,
    pub solid_nodes: Vec<WorkbenchNode>,
    pub parked_items: Vec<WorkbenchParkedItem>,
}

#[command]
pub async fn get_conversation_workbench(
    conversation_id: String,
    workspace_path: String,
) -> Result<WorkbenchData, String> {
    let conn = crate::db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;

    // 1. Fetch messages
    let mut messages = Vec::new();
    let mut stmt_msgs = conn
        .prepare("SELECT message_id, role, text_content FROM message_index WHERE conversation_id = ?1 ORDER BY create_time ASC")
        .map_err(|e| e.to_string())?;
    
    let mut rows_msgs = stmt_msgs.query(params![conversation_id]).map_err(|e| e.to_string())?;
    while let Ok(Some(row)) = rows_msgs.next() {
        let message_id: String = row.get(0).unwrap_or_default();
        let role: String = row.get(1).unwrap_or_default();
        let blob: Vec<u8> = row.get(2).unwrap_or_default();
        
        let text = if let Ok(decompressed) = zstd::stream::decode_all(&blob[..]) {
            String::from_utf8(decompressed).unwrap_or_default()
        } else {
            "".to_string()
        };
        
        messages.push(WorkbenchMessage {
            message_id,
            role,
            text,
        });
    }

    // 2. Fetch solid extracted nodes linked to this conversation
    let mut solid_nodes = Vec::new();
    let mut stmt_nodes = conn.prepare("
        SELECT n.id, n.type, n.name 
        FROM node n 
        JOIN edge e ON e.dst_id = n.id 
        WHERE e.src_id = ?1
    ").map_err(|e| e.to_string())?;
    
    let mut rows_nodes = stmt_nodes.query(params![conversation_id]).map_err(|e| e.to_string())?;
    while let Ok(Some(row)) = rows_nodes.next() {
        solid_nodes.push(WorkbenchNode {
            id: row.get(0).unwrap_or_default(),
            node_type: row.get(1).unwrap_or_default(),
            name: row.get(2).unwrap_or_default(),
        });
    }

    // 3. Fetch parked items
    let mut parked_items = Vec::new();
    let mut stmt_parked = conn.prepare("
        SELECT id, kind, payload, confidence 
        FROM parked_extractions 
        WHERE conversation_id = ?1
    ").map_err(|e| e.to_string())?;
    
    let mut rows_parked = stmt_parked.query(params![conversation_id]).map_err(|e| e.to_string())?;
    while let Ok(Some(row)) = rows_parked.next() {
        parked_items.push(WorkbenchParkedItem {
            id: row.get(0).unwrap_or_default(),
            kind: row.get(1).unwrap_or_default(),
            payload: row.get(2).unwrap_or_default(),
            confidence: row.get(3).unwrap_or_default(),
        });
    }

    Ok(WorkbenchData {
        messages,
        solid_nodes,
        parked_items,
    })
}

#[command]
pub async fn approve_parked_extraction(
    extraction_id: i64,
    workspace_path: String,
) -> Result<(), String> {
    let workspace = Path::new(&workspace_path);
    let conn = crate::db::open_workspace_db(workspace)
        .map_err(|e| e.to_string())?;

    let (conversation_id, kind, payload, confidence): (String, String, String, f64) = {
        let mut stmt = conn.prepare("SELECT conversation_id, kind, payload, confidence FROM parked_extractions WHERE id = ?1").map_err(|e| e.to_string())?;
        let mut rows = stmt.query(params![extraction_id]).map_err(|e| e.to_string())?;
        if let Ok(Some(row)) = rows.next() {
            (
                row.get(0).unwrap_or_default(),
                row.get(1).unwrap_or_default(),
                row.get(2).unwrap_or_default(),
                row.get(3).unwrap_or_default(),
            )
        } else {
            return Err("Parked extraction not found".into());
        }
    };

    let cfg = crate::read_workspace_config(workspace);
    let embed_model = if cfg.jobs.embedding.model.trim().is_empty() {
        "nomic-embed-text".to_string()
    } else {
        cfg.jobs.embedding.model.clone()
    };
    let embed_client = crate::llm::openai::OpenAiCompatibleClient::new(
        cfg.endpoints.default.base_url.clone(),
        cfg.endpoints.default.api_key.clone(),
    );

    crate::ingest::extraction::approve_parked_payload(
        workspace,
        extraction_id,
        &conversation_id,
        &kind,
        &payload,
        confidence,
        &embed_client,
        &embed_model,
    )
    .await
    .map_err(|e| e.to_string())
}

#[command]
pub async fn reject_parked_extraction(
    extraction_id: i64,
    workspace_path: String,
) -> Result<(), String> {
    let conn = crate::db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;

    conn.execute("DELETE FROM parked_extractions WHERE id = ?1", params![extraction_id])
        .map_err(|e| e.to_string())?;

    Ok(())
}
