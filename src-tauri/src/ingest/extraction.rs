use rusqlite::{Connection, OptionalExtension, Transaction, params};
use anyhow::{Result, anyhow};
use crate::llm::{LlmClient, Message, ChatOpts};
use crate::kg::extraction::sanitize_extraction_output;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;
use futures::future::try_join_all;
use std::fs::OpenOptions;
use std::io::Write;

const SIM_THRESHOLD_ENTITY: f32 = 0.92;
const SIM_THRESHOLD_TOPIC: f32 = 0.88;
const PARKED_THRESHOLD: f64 = 0.60;
const SOLID_THRESHOLD: f64 = 0.85;

// Conversations whose visible-path messages total fewer chars than this skip
// the map stage entirely. The old threshold (15k chars) pushed too many
// medium-size conversations through map-reduce, which multiplies HTTP/LLM
// round-trips and dominates perceived ingest time. This keeps single-pass as
// the default for anything that still fits comfortably inside the 32k-context
// local models we target.
const SINGLE_PASS_THRESHOLD: usize = 45_000;

#[derive(serde::Deserialize)]
struct StoredUserMessage {
    message_id: String,
    create_time: f64,
    on_visible_path: bool,
    text: String,
}

// #region agent log
const DEBUG_LOG_PATH: &str = "/Users/saurav/projects/apps/chat-analyzer/.cursor/debug-a5604b.log";
fn agent_log(hypothesis_id: &str, location: &str, message: &str, data: Value) {
    let payload = json!({
        "sessionId": "a5604b",
        "runId": "pre-fix",
        "hypothesisId": hypothesis_id,
        "location": location,
        "message": message,
        "data": data,
        "timestamp": chrono::Utc::now().timestamp_millis(),
    });
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(DEBUG_LOG_PATH) {
        let _ = writeln!(f, "{}", payload.to_string());
    }
}
// #endregion

fn vec_bytes(v: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for x in v {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    bytes
}

fn normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

// For normalized vectors: dist² = 2(1 - cos), so dist = sqrt(2(1 - cos)).
fn dist_for_cos(cos: f32) -> f32 {
    (2.0 * (1.0 - cos)).max(0.0).sqrt()
}

fn tier_for(confidence: f64) -> &'static str {
    if confidence >= SOLID_THRESHOLD {
        "solid"
    } else {
        "amber"
    }
}

fn normalize_name(s: &str) -> String {
    let lowered = s.trim().to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    for c in lowered.chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn extract_balanced_json_object(text: &str) -> Option<&str> {
    let mut start: Option<usize> = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => {
                if start.is_none() {
                    start = Some(idx);
                }
                depth += 1;
            }
            '}' => {
                if depth == 0 {
                    continue;
                }
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        return Some(&text[s..=idx]);
                    }
                }
            }
            _ => {}
        }
    }

    None
}

fn extract_balanced_json_value(text: &str) -> Option<&str> {
    // Try object first (current behavior), then array.
    if let Some(obj) = extract_balanced_json_object(text) {
        return Some(obj);
    }

    let mut start: Option<usize> = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '[' => {
                if start.is_none() {
                    start = Some(idx);
                }
                depth += 1;
            }
            ']' => {
                if depth == 0 {
                    continue;
                }
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        return Some(&text[s..=idx]);
                    }
                }
            }
            _ => {}
        }
    }

    None
}

/// Normalize common LLM output mistakes so serde_json can accept it:
/// - Smart / curly quotes → straight quotes (models sometimes emit “ ” around keys/values).
/// - Trailing commas before `]` or `}` (extremely common from LLMs).
fn repair_common_json_mistakes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '“' | '”' | '„' | '‟' => out.push('"'),
            '‘' | '’' | '‚' | '‛' => out.push('\''),
            _ => out.push(ch),
        }
    }
    // Strip trailing commas in arrays/objects: `,]` → `]`, `,}` → `}`, with
    // whitespace tolerance. Not a JSON-tokenizer-correct implementation but
    // sufficient for LLM-emitted payloads which don't use literal `,]` inside
    // string values.
    let mut result = String::with_capacity(out.len());
    let bytes = out.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            result.push(b as char);
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            result.push('"');
            i += 1;
            continue;
        }
        if b == b',' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\n' || bytes[j] == b'\r' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b']' || bytes[j] == b'}') {
                // Skip the comma — the trailing one is what's breaking serde.
                i += 1;
                continue;
            }
        }
        result.push(b as char);
        i += 1;
    }
    result
}

/// Last-resort repair: if the LLM emitted a truncated or unterminated value
/// (open `{[` with no matching close), append the minimum closing chars
/// needed so serde_json can at least see a structurally complete value. This
/// is safe-ish because the caller tolerates missing fields on individual
/// items — the alternative is the whole conversation failing.
fn close_unbalanced_json(s: &str) -> String {
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escape = false;
    for ch in s.chars() {
        if in_string {
            if escape {
                escape = false;
                continue;
            }
            match ch {
                '\\' => escape = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.last().copied() == Some(ch) {
                    stack.pop();
                }
            }
            _ => {}
        }
    }
    let mut out = s.to_string();
    // If we ended mid-string, close it first so trailing close chars don't
    // fall inside the unterminated string.
    if in_string {
        out.push('"');
    }
    while let Some(closer) = stack.pop() {
        out.push(closer);
    }
    out
}

fn parse_json_response(text: &str) -> Result<Value> {
    let trimmed = text.trim();

    // Fast path: well-formed JSON straight from the model.
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Ok(v);
    }

    // Markdown-fenced JSON (```json ... ```).
    for marker in ["```json", "```"] {
        if let Some(start) = text.find(marker) {
            let rest = &text[start + marker.len()..];
            if let Some(end) = rest.find("```") {
                let fenced = rest[..end].trim();
                if let Ok(v) = serde_json::from_str::<Value>(fenced) {
                    return Ok(v);
                }
                if let Some(val) = extract_balanced_json_value(fenced) {
                    if let Ok(v) = serde_json::from_str::<Value>(val) {
                        return Ok(v);
                    }
                }
            }
        }
    }

    // Balanced-brace extraction on raw text.
    if let Some(val) = extract_balanced_json_value(text) {
        if let Ok(v) = serde_json::from_str::<Value>(val) {
            return Ok(v);
        }
    }

    // Common-mistake repair pass: smart quotes, trailing commas.
    let repaired = repair_common_json_mistakes(trimmed);
    if let Ok(v) = serde_json::from_str::<Value>(&repaired) {
        return Ok(v);
    }
    if let Some(val) = extract_balanced_json_value(&repaired) {
        if let Ok(v) = serde_json::from_str::<Value>(val) {
            return Ok(v);
        }
    }

    // Last resort: if the response is truncated (open braces/brackets with
    // no matching closers), append the minimum closers to make it valid.
    // Saves a conversation from a hard-fail when the model cut off cleanly.
    let closed = close_unbalanced_json(&repaired);
    if let Ok(v) = serde_json::from_str::<Value>(&closed) {
        return Ok(v);
    }
    if let Some(val) = extract_balanced_json_value(&closed) {
        if let Ok(v) = serde_json::from_str::<Value>(val) {
            return Ok(v);
        }
    }

    Err(anyhow!("no parseable JSON object in model response"))
}

fn load_conversation_user_messages(
    db: &Connection,
    conversation_id: &str,
) -> Result<Vec<StoredUserMessage>> {
    let cached_blob: Option<Vec<u8>> = db
        .query_row(
            "SELECT user_messages FROM conversation_user_cache WHERE conversation_id = ?1",
            params![conversation_id],
            |r| r.get(0),
        )
        .optional()?;

    if let Some(blob) = cached_blob {
        let decompressed = zstd::stream::decode_all(&blob[..]).unwrap_or_default();
        let mut messages: Vec<StoredUserMessage> = serde_json::from_slice(&decompressed)
            .map_err(|e| anyhow!("invalid cached conversation transcript: {}", e))?;
        messages.sort_by(|a, b| {
            a.create_time
                .total_cmp(&b.create_time)
                .then_with(|| a.message_id.cmp(&b.message_id))
        });
        return Ok(messages);
    }

    let mut stmt = db.prepare(
        "SELECT message_id, text_content, create_time, on_visible_path
         FROM message_index
         WHERE conversation_id = ?1 AND role = 'user'
         ORDER BY create_time ASC",
    )?;
    let mut rows = stmt.query(params![conversation_id])?;
    let mut messages = Vec::new();
    while let Some(row) = rows.next()? {
        let message_id: String = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        let create_time: f64 = row.get::<_, Option<f64>>(2)?.unwrap_or(0.0);
        let on_visible_path: i64 = row.get(3)?;
        let decompressed = zstd::stream::decode_all(&blob[..]).unwrap_or_default();
        let text = String::from_utf8(decompressed).unwrap_or_default();
        messages.push(StoredUserMessage {
            message_id,
            create_time,
            on_visible_path: on_visible_path == 1,
            text,
        });
    }
    Ok(messages)
}

fn canonical_edge_type(kind: &str) -> &'static str {
    match kind {
        "topic" => "belongs_to",
        "entity" => "mentions",
        "concept" => "discusses",
        "pattern" => "uses_pattern",
        _ => "relates_to",
    }
}

fn parse_proposal(kind: &str, raw: Value, fallback_confidence: f64) -> Result<Proposed> {
    let name = raw
        .get("name")
        .and_then(|v| v.as_str())
        .map(normalize_name)
        .unwrap_or_default();

    if name.is_empty() {
        return Err(anyhow!("proposal missing name"));
    }

    let description = raw
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from)
        .filter(|s| !s.trim().is_empty());

    let message_ids = raw
        .get("message_ids")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(Proposed {
        kind: kind.to_string(),
        name,
        description,
        confidence: raw
            .get("confidence")
            .and_then(|v| v.as_f64())
            .unwrap_or(fallback_confidence),
        message_ids,
        raw,
    })
}

fn clear_conversation_derivations(
    tx: &Transaction<'_>,
    conversation_id: &str,
) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT DISTINCT dst_id
         FROM edge
         WHERE src_id = ?1
           AND type IN ('belongs_to', 'mentions', 'discusses', 'uses_pattern')",
    )?;
    let candidate_node_ids: Vec<String> = stmt
        .query_map(params![conversation_id], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();

    tx.execute(
        "DELETE FROM parked_extractions WHERE conversation_id = ?1",
        params![conversation_id],
    )?;
    tx.execute(
        "DELETE FROM edge
         WHERE src_id = ?1
           AND type IN ('belongs_to', 'mentions', 'discusses', 'uses_pattern')",
        params![conversation_id],
    )?;

    for node_id in candidate_node_ids {
        let remaining_edges: i64 = tx.query_row(
            "SELECT COUNT(*) FROM edge WHERE src_id = ?1 OR dst_id = ?1",
            params![&node_id],
            |r| r.get(0),
        )?;

        if remaining_edges > 0 {
            continue;
        }

        let node_type: Option<String> = tx
            .query_row(
                "SELECT type FROM node WHERE id = ?1",
                params![&node_id],
                |r| r.get(0),
            )
            .optional()?;

        if node_type.as_deref() == Some("conversation") {
            continue;
        }

        tx.execute("DELETE FROM node_vec WHERE node_id = ?1", params![&node_id])?;
        tx.execute("DELETE FROM node_fts WHERE node_id = ?1", params![&node_id])?;
        tx.execute("DELETE FROM node WHERE id = ?1", params![&node_id])?;
    }

    Ok(())
}

fn upsert_decision(
    tx: &Transaction<'_>,
    conversation_id: &str,
    decision: Decision,
    now: &str,
    force_insert: bool,
    approved_by_user: bool,
) -> Result<()> {
    let p = decision.proposal;
    if !force_insert && p.confidence < PARKED_THRESHOLD {
        tx.execute(
            "INSERT INTO parked_extractions (conversation_id, kind, payload, confidence, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![conversation_id, p.kind, p.raw.to_string(), p.confidence, now],
        )?;
        return Ok(());
    }

    let node_id = if let Some(existing_id) = decision.merge_into {
        tx.execute(
            "UPDATE node SET updated = ?1 WHERE id = ?2",
            params![now, &existing_id],
        )?;
        existing_id
    } else {
        let new_id = uuid::Uuid::new_v4().to_string();
        let tier = if approved_by_user {
            "solid"
        } else {
            tier_for(p.confidence)
        };
        let props = json!({
            "tier": tier,
            "confidence": p.confidence,
            "description": p.description.clone().unwrap_or_default(),
            "message_ids": p.message_ids.clone(),
            "approved": approved_by_user,
        });
        tx.execute(
            "INSERT INTO node (id, type, name, props, created, updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![&new_id, p.kind, p.name, props.to_string(), now],
        )?;
        if let Some(ref emb) = decision.embedding {
            let bytes = vec_bytes(emb);
            let _ = tx.execute(
                "INSERT INTO node_vec (node_id, embedding) VALUES (?1, ?2)",
                params![&new_id, bytes],
            );
        }
        let desc = p.description.clone().unwrap_or_default();
        let _ = tx.execute(
            "INSERT INTO node_fts (node_id, name, description) VALUES (?1, ?2, ?3)",
            params![&new_id, p.name, desc],
        );
        new_id
    };

    let edge_type = canonical_edge_type(&p.kind);
    let edge_props = json!({
        "count": 1,
        "message_ids": p.message_ids,
    });
    let edge_message_ids = edge_props
        .get("message_ids")
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]))
        .to_string();
    tx.execute(
        "INSERT INTO edge (src_id, dst_id, type, props) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(src_id, dst_id, type) DO UPDATE SET
           props = json_set(
             json_set(
               coalesce(edge.props, '{}'),
               '$.count',
               coalesce(json_extract(edge.props, '$.count'), 0) + 1
             ),
             '$.message_ids',
             json(?5)
           )",
        params![
            conversation_id,
            node_id,
            edge_type,
            edge_props.to_string(),
            edge_message_ids
        ],
    )?;

    Ok(())
}

fn find_merge_candidate(
    db: &Connection,
    kind: &str,
    embedding: &[f32],
) -> Result<Option<String>> {
    let threshold_cos = if kind == "topic" {
        SIM_THRESHOLD_TOPIC
    } else {
        SIM_THRESHOLD_ENTITY
    };
    let max_dist = dist_for_cos(threshold_cos);
    let query_bytes = vec_bytes(embedding);

    // sqlite-vec KNN via MATCH. If the extension isn't loaded the prepare fails
    // and we skip dedup rather than block extraction.
    let mut stmt = match db.prepare(
        "SELECT nv.node_id, nv.distance
         FROM node_vec nv
         JOIN node n ON n.id = nv.node_id
         WHERE nv.embedding MATCH ?1 AND k = 5 AND n.type = ?2
         ORDER BY nv.distance ASC
         LIMIT 1",
    ) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };

    let mut rows = stmt.query(params![query_bytes, kind])?;
    if let Some(row) = rows.next()? {
        let node_id: String = row.get(0)?;
        let dist: f64 = row.get(1)?;
        if (dist as f32) <= max_dist {
            return Ok(Some(node_id));
        }
    }
    Ok(None)
}

pub async fn extract_single_conversation(
    workspace_path: &Path,
    chat_llm: &dyn LlmClient,
    embed_llm: &dyn LlmClient,
    extract_model: &str,
    triage_model: &str,
    embed_model: &str,
    conversation_id: &str,
) -> Result<()> {
    let db_path = workspace_path.join("state").join("app.sqlite");

    {
        let db = Connection::open(&db_path)?;
        db.execute(
            "INSERT INTO extraction_state (conversation_id, status, content_hash, prompt_version, model_used, started_at)
             VALUES (?1, 'processing', '', '1', ?2, ?3)
             ON CONFLICT(conversation_id) DO UPDATE SET
                status='processing',
                model_used=excluded.model_used,
                started_at=excluded.started_at,
                error=NULL",
            params![conversation_id, extract_model, chrono::Utc::now().to_rfc3339()],
        )?;
    }

    let extraction_result = do_extract(
        &db_path,
        chat_llm,
        embed_llm,
        extract_model,
        triage_model,
        embed_model,
        conversation_id,
    )
    .await;

    let db = Connection::open(&db_path)?;
    match &extraction_result {
        Ok(_) => {
            db.execute(
                "UPDATE extraction_state SET status='done', completed_at=?1, error=NULL WHERE conversation_id=?2",
                params![chrono::Utc::now().to_rfc3339(), conversation_id],
            )?;
        }
        Err(e) => {
            db.execute(
                "UPDATE extraction_state SET status='failed', completed_at=?1, error=?2 WHERE conversation_id=?3",
                params![chrono::Utc::now().to_rfc3339(), e.to_string(), conversation_id],
            )?;
        }
    }

    extraction_result
}

struct Proposed {
    kind: String,
    name: String,
    description: Option<String>,
    confidence: f64,
    message_ids: Vec<String>,
    raw: Value,
}

struct Decision {
    proposal: Proposed,
    merge_into: Option<String>,
    embedding: Option<Vec<f32>>,
}

pub async fn approve_parked_payload(
    workspace_path: &Path,
    extraction_id: i64,
    conversation_id: &str,
    kind: &str,
    payload: &str,
    confidence: f64,
    embed_llm: &dyn LlmClient,
    embed_model: &str,
) -> Result<()> {
    let raw: Value = serde_json::from_str(payload)?;
    let proposal = parse_proposal(kind, raw, confidence)?;
    let db_path = workspace_path.join("state").join("app.sqlite");

    let embedding = if proposal.kind != "pattern" {
        let input = match &proposal.description {
            Some(d) if !d.is_empty() => format!("{}: {}", proposal.name, d),
            _ => proposal.name.clone(),
        };
        match embed_llm.embed(embed_model, &input).await {
            Ok(mut emb) => {
                normalize(&mut emb);
                Some(emb)
            }
            Err(_) => None,
        }
    } else {
        None
    };

    let merge_into = if proposal.kind == "pattern" {
        let db = Connection::open(&db_path)?;
        db.query_row(
            "SELECT id FROM node WHERE type='pattern' AND name=?1 LIMIT 1",
            params![&proposal.name],
            |r| r.get(0),
        )
        .optional()?
    } else if let Some(ref emb) = embedding {
        let db = Connection::open(&db_path)?;
        find_merge_candidate(&db, &proposal.kind, emb)?
    } else {
        None
    };

    let mut db = Connection::open(&db_path)?;
    let tx = db.transaction()?;
    upsert_decision(
        &tx,
        conversation_id,
        Decision {
            proposal,
            merge_into,
            embedding,
        },
        &chrono::Utc::now().to_rfc3339(),
        true,
        true,
    )?;
    tx.execute(
        "DELETE FROM parked_extractions WHERE id = ?1",
        params![extraction_id],
    )?;
    tx.commit()?;
    Ok(())
}

async fn do_extract(
    db_path: &Path,
    llm: &dyn LlmClient,
    embed_llm: &dyn LlmClient,
    extract_model: &str,
    triage_model: &str,
    embed_model: &str,
    conversation_id: &str,
) -> Result<()> {
    let t_start = Instant::now();
    // Rough budget: qwen2.5 7b has 32k context. Reserve ~4k for prompt
    // scaffolding and response. Leave ~24k tokens ≈ 96k chars for messages.
    const CHUNK_SIZE_CHARS: usize = 12_000;
    const MAX_MSG_SNIPPET: usize = 2_000;

    let (title, created, chunks) = {
        let db = Connection::open(db_path)?;
        let (title, created): (String, String) = db.query_row(
            "SELECT name, created FROM node WHERE id = ?1 AND type = 'conversation'",
            params![conversation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let messages = load_conversation_user_messages(&db, conversation_id)?;

        let mut chunks = Vec::new();
        let mut current_chunk = String::new();

        for msg in messages.into_iter().filter(|m| m.on_visible_path) {
            let snippet = if msg.text.len() > MAX_MSG_SNIPPET {
                format!("{}…", &msg.text[..MAX_MSG_SNIPPET])
            } else {
                msg.text
            };
            let chunk_str = format!("[{}] user:\n{}\n\n", msg.message_id, snippet);

            if current_chunk.len() + chunk_str.len() > CHUNK_SIZE_CHARS && !current_chunk.is_empty() {
                chunks.push(current_chunk.clone());
                current_chunk.clear();
            }
            current_chunk.push_str(&chunk_str);
        }
        if !current_chunk.is_empty() {
            chunks.push(current_chunk);
        }
        (title, created, chunks)
    };

    if chunks.is_empty() || (chunks.len() == 1 && chunks[0].trim().is_empty()) {
        return Err(anyhow!("No messages to extract from"));
    }

    // Fast path: trivial
    if chunks.len() == 1 && chunks[0].len() < 280 {
        eprintln!("[extract {}] trivial ({} chars), skipping LLM", conversation_id, chunks[0].len());
        return Ok(());
    }

    let total_chars: usize = chunks.iter().map(|c| c.len()).sum();
    agent_log(
        "E",
        "src-tauri/src/ingest/extraction.rs:do_extract:init",
        "extraction_input",
        json!({
            "conversation_id": conversation_id,
            "chunks": chunks.len(),
            "total_chars": total_chars,
            "extract_model": extract_model,
            "triage_model": triage_model,
            "embed_model": embed_model,
        }),
    );

    // Fast path: if the whole conversation fits under SINGLE_PASS_THRESHOLD,
    // skip the map stage entirely — one pass beats map+reduce at this size
    // because the extra round-trips cost more than the prefill savings.
    let master_summary = if total_chars < SINGLE_PASS_THRESHOLD {
        eprintln!(
            "[extract {}] single-pass ({} chars, {} chunks)",
            conversation_id, total_chars, chunks.len()
        );
        chunks.join("")
    } else {
        // STAGE 1: Map. Summarize chunks in parallel with the cheap triage
        // model. Sequential was costing us the entire Ollama parallelism
        // budget; try_join_all lets OLLAMA_NUM_PARALLEL actually kick in.
        eprintln!(
            "[extract {}] map-reduce: {} chunks, {} chars, triage={}",
            conversation_id, chunks.len(), total_chars, triage_model
        );
        let t_map = Instant::now();
        let summarize_futures = chunks.iter().map(|chunk| {
            let prompt = format!(
                "Summarize the core concepts, topics, and entities in this conversation snippet.\n\n\
                 RULES:\n\
                 - Output EXACTLY 3-5 bullet points. No prose.\n\
                 - Keep the [msg_id] tags next to each concept so citations survive.\n\n\
                 SNIPPET:\n{}",
                chunk
            );
            llm.chat(
                triage_model,
                vec![Message {
                    role: "user".to_string(),
                    content: prompt,
                }],
                ChatOpts {
                    temperature: 0.1,
                    json_mode: false,
                    max_tokens: Some(300),
                },
            )
        });
        let responses = try_join_all(summarize_futures).await?;
        agent_log(
            "E",
            "src-tauri/src/ingest/extraction.rs:do_extract:map_done",
            "map_stage_complete",
            json!({
                "conversation_id": conversation_id,
                "chunks": chunks.len(),
                "elapsed_ms": t_map.elapsed().as_millis(),
                "triage_model": triage_model,
                "response_lengths": responses.iter().map(|r| r.message.content.len()).collect::<Vec<_>>(),
            }),
        );
        eprintln!(
            "[extract {}] map done in {:.1}s",
            conversation_id,
            t_map.elapsed().as_secs_f32()
        );
        responses
            .into_iter()
            .map(|r| r.message.content)
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
    };
    
    // STAGE 2: JSON Extraction (Reduce)
    let prompt_body: Option<String> = {
        let db = Connection::open(db_path)?;
        db.query_row(
            "SELECT body FROM prompts WHERE kind='extraction' AND is_active=1 ORDER BY version DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok()
    };

    let mut safe_summary = master_summary;
    if safe_summary.len() > 15000 {
        safe_summary.truncate(15000);
        safe_summary.push_str("\n... [Snippet Truncated to 15000 Chars Limit]");
    }

    let prompt = match prompt_body {
        Some(body) => body
            .replace("{{title}}", &title)
            .replace("{{created}}", &created)
            .replace("{{messages_with_ids}}", &safe_summary),
        None => default_extraction_prompt(&title, &created, &safe_summary),
    };

    let t_reduce = Instant::now();
    let mut parsed: Option<Value> = None;
    let mut last_err: Option<String> = None;
    let mut last_response_len: Option<usize> = None;
    let mut last_response_prefix: Option<String> = None;
    let mut last_response_suffix: Option<String> = None;
    for attempt in 0..3 {
        let p = if attempt == 0 {
            prompt.clone()
        } else {
            format!(
                "{}\n\nReturn ONLY valid JSON, no prose, no markdown.",
                prompt
            )
        };
        agent_log(
            "A",
            "src-tauri/src/ingest/extraction.rs:do_extract:reduce_attempt",
            "reduce_attempt_start",
            json!({
                "conversation_id": conversation_id,
                "attempt": attempt + 1,
                "extract_model": extract_model,
                "prompt_len": p.len(),
                "json_mode": true,
                "max_tokens": 6000,
            }),
        );
        let response = llm
            .chat(
                extract_model,
                vec![Message {
                    role: "user".to_string(),
                    content: p,
                }],
                ChatOpts {
                    temperature: 0.2,
                    json_mode: true,
                    max_tokens: Some(6000),
                },
            )
            .await?;

        let content = &response.message.content;
        last_response_len = Some(content.len());
        last_response_prefix = Some(content.chars().take(200).collect::<String>());
        last_response_suffix = Some(
            content
                .chars()
                .rev()
                .take(200)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>(),
        );
        agent_log(
            "D",
            "src-tauri/src/ingest/extraction.rs:do_extract:reduce_attempt",
            "reduce_attempt_response",
            json!({
                "conversation_id": conversation_id,
                "attempt": attempt + 1,
                "content_len": content.len(),
                "content_prefix": content.chars().take(160).collect::<String>(),
                "content_suffix": content.chars().rev().take(160).collect::<String>().chars().rev().collect::<String>(),
            }),
        );

        match parse_json_response(content) {
            Ok(v) => {
                parsed = Some(v);
                break;
            }
            Err(e) => {
                last_err = Some(e.to_string());
                agent_log(
                    "B",
                    "src-tauri/src/ingest/extraction.rs:do_extract:reduce_attempt",
                    "reduce_attempt_parse_failed",
                    json!({
                        "conversation_id": conversation_id,
                        "attempt": attempt + 1,
                        "error": e.to_string(),
                    }),
                );
            }
        }
    }

    let json_output = parsed.ok_or_else(|| {
        let snippet = match (last_response_len, last_response_prefix, last_response_suffix) {
            (Some(len), Some(pre), Some(suf)) => format!(
                " (response_len={}, prefix={:?}, suffix={:?})",
                len, pre, suf
            ),
            _ => "".to_string(),
        };
        anyhow!(
            "LLM did not return valid JSON after 3 attempts: {}{}",
            last_err.unwrap_or_default(),
            snippet
        )
    })?;
    eprintln!(
        "[extract {}] reduce done in {:.1}s (total {:.1}s)",
        conversation_id,
        t_reduce.elapsed().as_secs_f32(),
        t_start.elapsed().as_secs_f32()
    );

    let sanitized = sanitize_extraction_output(json_output.clone())?;
    eprintln!(
        "[extract {}] chat returned, parsing {} topic / {} entity / {} concept / {} pattern",
        conversation_id,
        sanitized.topics.len(),
        sanitized.entities.len(),
        sanitized.concepts.len(),
        sanitized.prompt_patterns.len()
    );

    let mut proposals: Vec<Proposed> = Vec::new();
    // Convert sanitized output back into the internal proposal representation.
    for item in sanitized.topics {
        proposals.push(parse_proposal("topic", item.raw, item.confidence)?);
    }
    for item in sanitized.entities {
        proposals.push(parse_proposal("entity", item.raw, item.confidence)?);
    }
    for item in sanitized.concepts {
        proposals.push(parse_proposal("concept", item.raw, item.confidence)?);
    }
    for item in sanitized.prompt_patterns {
        proposals.push(parse_proposal("pattern", item.raw, item.confidence)?);
    }

    // Batch embed everything that needs it in one HTTP call, then run the
    // dedup queries. Patterns use exact-name match, so they skip embeds.
    let embed_indexes: Vec<usize> = proposals
        .iter()
        .enumerate()
        .filter(|(_, p)| p.confidence >= PARKED_THRESHOLD && p.kind != "pattern")
        .map(|(i, _)| i)
        .collect();
    let embed_inputs: Vec<String> = embed_indexes
        .iter()
        .map(|&i| {
            let p = &proposals[i];
            match &p.description {
                Some(d) if !d.is_empty() => format!("{}: {}", p.name, d),
                _ => p.name.clone(),
            }
        })
        .collect();

    let mut embeddings: Vec<Option<Vec<f32>>> = vec![None; proposals.len()];
    if !embed_inputs.is_empty() {
        let batch = embed_llm.embed_many(embed_model, &embed_inputs).await?;
        for (slot, mut emb) in embed_indexes.iter().zip(batch.into_iter()) {
            normalize(&mut emb);
            embeddings[*slot] = Some(emb);
        }
    }

    let mut decisions: Vec<Decision> = Vec::with_capacity(proposals.len());
    for (i, p) in proposals.into_iter().enumerate() {
        if p.confidence < PARKED_THRESHOLD {
            decisions.push(Decision {
                proposal: p,
                merge_into: None,
                embedding: None,
            });
            continue;
        }
        if p.kind == "pattern" {
            let db = Connection::open(db_path)?;
            let existing: Option<String> = db
                .query_row(
                    "SELECT id FROM node WHERE type='pattern' AND name=?1 LIMIT 1",
                    params![p.name],
                    |r| r.get(0),
                )
                .ok();
            decisions.push(Decision {
                proposal: p,
                merge_into: existing,
                embedding: None,
            });
            continue;
        }
        let embedding = embeddings[i].take();
        let merge_into = if let Some(ref emb) = embedding {
            let db = Connection::open(db_path)?;
            find_merge_candidate(&db, &p.kind, emb)?
        } else {
            None
        };
        decisions.push(Decision {
            proposal: p,
            merge_into,
            embedding,
        });
    }

    let parked = decisions.iter().filter(|d| d.proposal.confidence < PARKED_THRESHOLD).count();
    let solid = decisions.len().saturating_sub(parked);
    eprintln!(
        "[extract {}] decisions: total={}, solid={}, parked={} (threshold={:.2})",
        conversation_id,
        decisions.len(),
        solid,
        parked,
        PARKED_THRESHOLD
    );

    let mut db = Connection::open(db_path)?;
    let tx = db.transaction()?;
    let now = chrono::Utc::now().to_rfc3339();
    clear_conversation_derivations(&tx, conversation_id)?;

    for d in decisions {
        upsert_decision(&tx, conversation_id, d, &now, false, false)?;
    }

    // Ensure the conversation itself becomes "recent" after extraction so
    // graph views seeded by recent conversations include newly derived nodes.
    let _ = tx.execute(
        "UPDATE node SET updated = ?1 WHERE id = ?2",
        params![&now, conversation_id],
    );

    tx.commit()?;
    eprintln!("[extract {}] commit complete", conversation_id);
    Ok(())
}

fn default_extraction_prompt(title: &str, created: &str, messages: &str) -> String {
    // Tight prompt: less prefill time for the model, same shape of output.
    // Field definitions kept terse because downstream code handles all the
    // thresholding and dedup — the model only needs to emit items + scores.
    format!(
        r#"Extract a knowledge graph from this conversation as JSON.

Fields:
- topics: 1-3 high-level subjects. lowercase-kebab names.
- entities: named things (person|tool|org|book|tech|place).
- concepts: atomic ideas worth remembering. max 15.
- prompt_patterns: reusable question framings. max 3.

Only include items DISCUSSED substantively. Confidence 0.9+=defined, 0.6-0.85=mentioned, <0.6=uncertain. Cite 1-3 message_ids each.

Title: {}
Date: {}
Messages:
{}

JSON only, no prose:
{{"topics":[{{"name":"","confidence":0.0,"message_ids":[]}}],"entities":[{{"name":"","type":"","description":"","confidence":0.0,"message_ids":[]}}],"concepts":[{{"name":"","description":"","confidence":0.0,"message_ids":[]}}],"prompt_patterns":[{{"name":"","description":"","confidence":0.0,"message_ids":[]}}]}}"#,
        title, created, messages
    )
}

#[cfg(test)]
mod tests {
    use super::parse_json_response;

    #[test]
    fn parse_json_response_accepts_fenced_json() {
        let parsed = parse_json_response(
            "Here you go:\n```json\n{\"topics\":[],\"entities\":[],\"concepts\":[],\"prompt_patterns\":[]}\n```",
        )
        .expect("fenced json should parse");
        assert!(parsed.get("topics").is_some());
    }

    #[test]
    fn parse_json_response_extracts_balanced_object_from_prose() {
        let parsed = parse_json_response(
            "Answer first, then JSON: {\"topics\":[],\"entities\":[],\"concepts\":[{\"name\":\"x\"}],\"prompt_patterns\":[]} trailing note",
        )
        .expect("embedded object should parse");
        assert_eq!(parsed["concepts"][0]["name"], "x");
    }

    #[test]
    fn parse_json_response_accepts_top_level_array() {
        let parsed = parse_json_response(
            "Here you go:\n```json\n[{\"name\":\"x\"},{\"name\":\"y\"}]\n```",
        )
        .expect("array json should parse");
        assert!(parsed.as_array().is_some());
        assert_eq!(parsed[0]["name"], "x");
    }
}
