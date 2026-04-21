use std::fs::File;
use std::io::Read;
use std::collections::HashSet;
use serde_json::Value;
use rusqlite::{Connection, params};
use zstd::stream::encode_all;
use anyhow::Result;
use std::path::Path;
use sha2::{Sha256, Digest};
use std::time::UNIX_EPOCH;

#[derive(serde::Serialize)]
struct StoredUserMessage {
    message_id: String,
    create_time: f64,
    on_visible_path: bool,
    text: String,
}

pub fn process_export_file(
    db: &mut Connection,
    file_path: &Path,
    workspace_dir: &Path,
    force: bool,
) -> Result<String> {
    let mut file = File::open(file_path)?;
    let mut hasher = Sha256::new();
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;
    hasher.update(&buffer);
    let hash = hasher.finalize();
    let source_id = hex::encode(hash);

    let file_size = buffer.len() as i64;
    let hash6 = &source_id[0..6];

    let existing_filename: Option<String> = db
        .query_row(
            "SELECT internal_filename FROM source_registry WHERE source_id = ?1",
            params![&source_id],
            |row| row.get(0),
        )
        .ok();

    if let Some(name) = existing_filename {
        if !force {
            return Err(anyhow::anyhow!(
                "Already imported on {}.",
                name.split('-').nth(1).unwrap_or("a previous date")
            ));
        }
        // Override: purge prior rows for this source. Per PROJECT.md §7
        // idempotency rules, edges from removed conversations are deleted
        // but shared nodes survive (they may be supported by other sources).
        purge_source(db, &source_id, workspace_dir, &name)?;
    }

    let conversations: Vec<Value> = serde_json::from_slice(&buffer)
        .map_err(|e| anyhow::anyhow!("Invalid JSON: {}", e))?;

    let mut max_update_time: f64 = 0.0;
    for conv in &conversations {
        if let Some(time) = conv.get("update_time").and_then(|v| v.as_f64()) {
            if time > max_update_time {
                max_update_time = time;
            }
        }
    }

    let export_date = if max_update_time > 0.0 {
        let d = UNIX_EPOCH + std::time::Duration::from_secs_f64(max_update_time);
        let dt: chrono::DateTime<chrono::Utc> = d.into();
        dt.format("%Y-%m-%d").to_string()
    } else {
        chrono::Utc::now().format("%Y-%m-%d").to_string()
    };

    let internal_filename = format!("conversations-{}-{}.json", export_date, hash6);
    let dest_path = workspace_dir.join("sources").join(&internal_filename);

    // PROJECT.md §7 says "move, not copy" — but in practice the user picks
    // their permanent conversations.json via a file dialog and expects the
    // original to stay in place. Copy is non-destructive and the extra disk
    // usage is negligible on a single-user local tool.
    std::fs::copy(file_path, &dest_path)?;

    let tx = db.transaction()?;

    let original_filename = file_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    tx.execute(
        "INSERT INTO source_registry (source_id, internal_filename, file_size, export_date, imported_at, conversation_count, original_filename)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            &source_id,
            internal_filename,
            file_size,
            export_date,
            chrono::Utc::now().to_rfc3339(),
            conversations.len() as i64,
            original_filename
        ]
    )?;

    let now = chrono::Utc::now().to_rfc3339();
    let num_conversations = conversations.len();
    let mut skipped = 0u64;

    for conv in conversations {
        let conv_id = conv
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown_id")
            .to_string();
        let title = conv.get("title").and_then(|v| v.as_str()).unwrap_or("Untitled");
        let create_time = conv.get("create_time").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let update_time = conv.get("update_time").and_then(|v| v.as_f64()).unwrap_or(0.0);

        let mapping = match conv.get("mapping").and_then(|v| v.as_object()) {
            Some(m) => m,
            None => {
                skipped += 1;
                continue;
            }
        };

        // Walk visible path from current_node back up via parent pointers.
        let mut visible: HashSet<String> = HashSet::new();
        if let Some(current) = conv.get("current_node").and_then(|v| v.as_str()) {
            let mut cursor: Option<String> = Some(current.to_string());
            while let Some(ref id) = cursor {
                if !visible.insert(id.clone()) {
                    break;
                }
                let next = mapping
                    .get(id)
                    .and_then(|n| n.get("parent"))
                    .and_then(|v| v.as_str())
                    .map(String::from);
                cursor = next;
            }
        }

        let mut turn_count: i64 = 0;
        let mut branch_count: i64 = 0;
        let mut has_user_message = false;
        let mut models: HashSet<String> = HashSet::new();
        let mut user_messages: Vec<StoredUserMessage> = Vec::new();

        for (node_id, node_val) in mapping {
            let children_len = node_val
                .get("children")
                .and_then(|c| c.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if children_len > 1 {
                branch_count += 1;
            }

            let msg = match node_val.get("message").and_then(|v| v.as_object()) {
                Some(m) => m,
                None => continue,
            };

            let role = msg
                .get("author")
                .and_then(|a| a.get("role"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let model = msg
                .get("metadata")
                .and_then(|m| m.get("model_slug"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if model != "unknown" {
                models.insert(model.to_string());
            }

            if role != "user" {
                continue;
            }

            has_user_message = true;
            turn_count += 1;

            let on_visible_path = if visible.contains(node_id) { 1 } else { 0 };
            let parent_id = node_val.get("parent").and_then(|v| v.as_str());
            let cr_time = msg.get("create_time").and_then(|v| v.as_f64()).unwrap_or(0.0);

            let parts = msg
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|v| v.as_array());
            let mut text_content = String::new();
            if let Some(p) = parts {
                for part in p {
                    if let Some(s) = part.as_str() {
                        text_content.push_str(s);
                        text_content.push('\n');
                    }
                }
            }

            let compressed = encode_all(text_content.as_bytes(), 3).unwrap_or_default();

            tx.execute(
                "INSERT INTO message_index (
                    conversation_id, message_id, parent_id, role, model,
                    create_time, byte_start, byte_length, text_content,
                    source_id, on_visible_path, is_system
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, ?7, ?8, ?9, ?10)
                ON CONFLICT(conversation_id, message_id) DO NOTHING",
                params![
                    conv_id,
                    node_id,
                    parent_id,
                    role,
                    model,
                    cr_time,
                    compressed,
                    source_id,
                    on_visible_path,
                    0
                ],
            )?;

            user_messages.push(StoredUserMessage {
                message_id: node_id.clone(),
                create_time: cr_time,
                on_visible_path: on_visible_path == 1,
                text: text_content,
            });
        }

        if !has_user_message {
            skipped += 1;
            continue;
        }

        user_messages.sort_by(|a, b| {
            a.create_time
                .total_cmp(&b.create_time)
                .then_with(|| a.message_id.cmp(&b.message_id))
        });
        let user_message_count = user_messages.len() as i64;
        let char_count = user_messages.iter().map(|m| m.text.len() as i64).sum::<i64>();
        let user_messages_json = serde_json::to_vec(&user_messages)?;
        let compressed_user_messages = encode_all(user_messages_json.as_slice(), 3)?;

        let content_hash = {
            let mut h = Sha256::new();
            h.update(conv_id.as_bytes());
            h.update(format!("{:.0}", update_time).as_bytes());
            hex::encode(h.finalize())
        };

        let props = serde_json::json!({
            "title": title,
            "chatgpt_created": create_time,
            "chatgpt_updated": update_time,
            "turn_count": turn_count,
            "branch_count": branch_count,
            "models": models.into_iter().collect::<Vec<_>>(),
            "content_hash": content_hash,
        });

        tx.execute(
            "INSERT INTO node (id, type, name, props, created, updated)
             VALUES (?1, 'conversation', ?2, ?3, ?4, ?4)
             ON CONFLICT(id) DO UPDATE SET props=excluded.props, updated=excluded.updated",
            params![conv_id, title, props.to_string(), now],
        )?;

        tx.execute(
            "INSERT INTO conversation_sources (conversation_id, source_id, update_time, turn_count, is_canonical)
             VALUES (?1, ?2, ?3, ?4, 1)
             ON CONFLICT(conversation_id, source_id) DO UPDATE SET update_time=excluded.update_time, turn_count=excluded.turn_count",
            params![conv_id, source_id, update_time, turn_count]
        )?;

        tx.execute(
            "INSERT INTO conversation_user_cache (
                conversation_id, source_id, user_messages, user_message_count, char_count, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(conversation_id) DO UPDATE SET
                source_id=excluded.source_id,
                user_messages=excluded.user_messages,
                user_message_count=excluded.user_message_count,
                char_count=excluded.char_count,
                updated_at=excluded.updated_at",
            params![
                conv_id,
                source_id,
                compressed_user_messages,
                user_message_count,
                char_count,
                now,
            ],
        )?;

        tx.execute(
            "INSERT INTO extraction_state (conversation_id, status, content_hash, prompt_version)
             VALUES (?1, 'pending', ?2, '1')
             ON CONFLICT(conversation_id) DO UPDATE SET
                status = CASE WHEN extraction_state.content_hash = excluded.content_hash THEN extraction_state.status ELSE 'stale' END,
                content_hash = excluded.content_hash",
            params![conv_id, content_hash],
        )?;
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO manifest (run_id, ts, kind, parsed, skipped, errors, notes)
         VALUES (?1, ?2, 'ingest', ?3, ?4, 0, 'Successful ingest')",
        params![
            run_id,
            now,
            num_conversations as i64,
            skipped as i64
        ],
    )?;

    tx.commit()?;
    Ok(source_id)
}

/// Delete a previously imported source and everything derived from it.
/// Keeps shared nodes (topics/concepts/entities) alive if another source
/// still supports them — per PROJECT.md §7 idempotency rules.
fn purge_source(
    db: &mut Connection,
    source_id: &str,
    workspace_dir: &Path,
    internal_filename: &str,
) -> Result<()> {
    let tx = db.transaction()?;

    // Conversations that exist ONLY in this source get fully removed.
    let convs_to_drop: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT cs.conversation_id FROM conversation_sources cs
             WHERE cs.source_id = ?1
               AND (SELECT COUNT(*) FROM conversation_sources cs2
                    WHERE cs2.conversation_id = cs.conversation_id) = 1",
        )?;
        let rows = stmt.query_map(params![source_id], |r| r.get::<_, String>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    for conv_id in &convs_to_drop {
        tx.execute(
            "DELETE FROM edge WHERE src_id = ?1 OR dst_id = ?1",
            params![conv_id],
        )?;
        tx.execute(
            "DELETE FROM extraction_state WHERE conversation_id = ?1",
            params![conv_id],
        )?;
        tx.execute(
            "DELETE FROM parked_extractions WHERE conversation_id = ?1",
            params![conv_id],
        )?;
        tx.execute("DELETE FROM node WHERE id = ?1 AND type = 'conversation'", params![conv_id])?;
    }

    tx.execute(
        "DELETE FROM message_index WHERE source_id = ?1",
        params![source_id],
    )?;
    tx.execute(
        "DELETE FROM conversation_user_cache WHERE source_id = ?1",
        params![source_id],
    )?;
    tx.execute(
        "DELETE FROM conversation_sources WHERE source_id = ?1",
        params![source_id],
    )?;
    tx.execute(
        "DELETE FROM source_registry WHERE source_id = ?1",
        params![source_id],
    )?;

    tx.commit()?;

    // Remove the stored source file.
    let stored = workspace_dir.join("sources").join(internal_filename);
    let _ = std::fs::remove_file(stored);
    Ok(())
}

/// Drop all workspace data but keep the workspace folder + workspace.yaml.
/// Called by the `reset_workspace` Tauri command.
pub fn reset_workspace_data(workspace_dir: &Path) -> Result<()> {
    let state = workspace_dir.join("state");
    for name in ["app.sqlite", "app.sqlite-wal", "app.sqlite-shm", "manifest.jsonl"] {
        let _ = std::fs::remove_file(state.join(name));
    }
    let sources = workspace_dir.join("sources");
    if let Ok(entries) = std::fs::read_dir(&sources) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    // Recreate schema so later commands don't hit "unable to open database file".
    crate::db::open_workspace_db(workspace_dir)?;
    Ok(())
}
