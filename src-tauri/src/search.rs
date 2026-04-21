use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use rusqlite::{Connection, params};
use serde::Serialize;
use tauri::command;

use crate::graph::{GraphData, Link, Node};
use crate::llm::{ChatOpts, LlmClient, Message};

const RRF_K: f64 = 60.0;
const RETRIEVAL_POOL: usize = 30;
const FUSED_TOP: usize = 15;
const GRAPH_NODE_CAP: usize = 25;

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

fn fts_search(conn: &Connection, query: &str, limit: usize) -> Vec<String> {
    let mut ids = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT node_id FROM node_fts WHERE node_fts MATCH ?1 ORDER BY rank LIMIT ?2",
    ) {
        if let Ok(iter) = stmt.query_map(params![query, limit as i64], |r| r.get::<_, String>(0)) {
            for r in iter.flatten() {
                ids.push(r);
            }
        }
    }
    if ids.is_empty() {
        if let Ok(mut stmt) = conn.prepare("SELECT id FROM node WHERE name LIKE ?1 LIMIT ?2") {
            let like = format!("%{}%", query);
            if let Ok(iter) = stmt.query_map(params![like, limit as i64], |r| r.get::<_, String>(0))
            {
                for r in iter.flatten() {
                    ids.push(r);
                }
            }
        }
    }
    ids
}

fn vector_search(conn: &Connection, embedding: &[f32], limit: usize) -> Vec<String> {
    let bytes = vec_bytes(embedding);
    let mut ids = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT node_id FROM node_vec WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance ASC",
    ) {
        if let Ok(iter) = stmt.query_map(params![bytes, limit as i64], |r| r.get::<_, String>(0)) {
            for r in iter.flatten() {
                ids.push(r);
            }
        }
    }
    ids
}

// Spec §9: score(node) = sum over retrievers [1 / (k + rank)], k = 60.
fn rrf_fuse(ranked_lists: &[Vec<String>]) -> Vec<String> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    for list in ranked_lists {
        for (rank, id) in list.iter().enumerate() {
            let contribution = 1.0 / (RRF_K + (rank + 1) as f64);
            *scores.entry(id.clone()).or_insert(0.0) += contribution;
        }
    }
    let mut pairs: Vec<_> = scores.into_iter().collect();
    pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    pairs.into_iter().map(|(id, _)| id).collect()
}

fn fetch_graph_for_nodes(conn: &Connection, base_node_ids: &[String]) -> Result<GraphData, String> {
    if base_node_ids.is_empty() {
        return Ok(GraphData {
            nodes: vec![],
            links: vec![],
        });
    }

    let placeholders: Vec<&str> = base_node_ids.iter().map(|_| "?").collect();
    let in_clause = placeholders.join(", ");

    let query_edges = format!(
        "SELECT src_id, dst_id, type, props FROM edge WHERE src_id IN ({}) OR dst_id IN ({})",
        in_clause, in_clause
    );

    let mut edge_params: Vec<&dyn rusqlite::ToSql> = Vec::new();
    for id in base_node_ids {
        edge_params.push(id as &dyn rusqlite::ToSql);
    }
    for id in base_node_ids {
        edge_params.push(id as &dyn rusqlite::ToSql);
    }

    let mut links = Vec::new();
    let mut expanded: HashSet<String> = base_node_ids.iter().cloned().collect();

    if let Ok(mut stmt) = conn.prepare(&query_edges) {
        let iter = stmt.query_map(rusqlite::params_from_iter(edge_params), |row| {
            let props: Option<String> = row.get(3).ok();
            let count: i64 = props
                .as_ref()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|v| v.get("count").and_then(|c| c.as_i64()))
                .unwrap_or(1);
            Ok(Link {
                source: row.get(0)?,
                target: row.get(1)?,
                value: count,
                edge_type: row.get(2)?,
            })
        });
        if let Ok(iter) = iter {
            for r in iter.flatten() {
                if expanded.len() >= GRAPH_NODE_CAP
                    && !expanded.contains(&r.source)
                    && !expanded.contains(&r.target)
                {
                    continue;
                }
                expanded.insert(r.source.clone());
                expanded.insert(r.target.clone());
                links.push(r);
                if expanded.len() >= GRAPH_NODE_CAP {
                    break;
                }
            }
        }
    }

    let exp_placeholders: Vec<&str> = expanded.iter().map(|_| "?").collect();
    let exp_in_clause = exp_placeholders.join(", ");
    let query_nodes = format!(
        "SELECT id, type, name, props FROM node WHERE id IN ({}) LIMIT ?",
        exp_in_clause
    );

    let mut node_params: Vec<&dyn rusqlite::ToSql> = Vec::new();
    for id in &expanded {
        node_params.push(id as &dyn rusqlite::ToSql);
    }
    let cap = GRAPH_NODE_CAP as i64;
    node_params.push(&cap);

    let mut nodes = Vec::new();
    if let Ok(mut stmt) = conn.prepare(&query_nodes) {
        let iter = stmt.query_map(rusqlite::params_from_iter(node_params), |row| {
            let props_str: Option<String> = row.get(3).unwrap_or(None);
            let props = props_str.and_then(|s| serde_json::from_str(&s).ok());
            Ok(Node {
                id: row.get(0)?,
                group: row.get(1)?,
                name: row.get(2)?,
                props,
            })
        });
        if let Ok(iter) = iter {
            for r in iter.flatten() {
                nodes.push(r);
            }
        }
    }

    let kept_ids: HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    links.retain(|l| kept_ids.contains(&l.source) && kept_ids.contains(&l.target));

    Ok(GraphData { nodes, links })
}

#[command]
pub async fn search_nodes(
    query: String,
    workspace_path: String,
    base_url: Option<String>,
    api_key: Option<String>,
    embed_model: Option<String>,
) -> Result<GraphData, String> {
    let conn = crate::db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;

    if query.trim().is_empty() {
        return crate::graph::get_initial_graph(&conn).map_err(|e| e.to_string());
    }

    let fts_results = fts_search(&conn, &query, RETRIEVAL_POOL);

    // Embeddings must use an endpoint that serves the workspace's vec dim
    // (768 / nomic-embed-text). Callers of this fn are expected to supply
    // the *embedding* endpoint specifically — not a cloud chat endpoint,
    // which wouldn't serve nomic-embed-text.
    let vector_results: Vec<String> = match (base_url, embed_model) {
        (Some(url), Some(model)) => {
            let client = crate::llm::openai::OpenAiCompatibleClient::new(
                url,
                api_key.unwrap_or_default(),
            );
            if let Ok(mut emb) = client.embed(&model, &query).await {
                normalize(&mut emb);
                vector_search(&conn, &emb, RETRIEVAL_POOL)
            } else {
                vec![]
            }
        }
        _ => vec![],
    };

    let fused = rrf_fuse(&[fts_results, vector_results]);
    let top: Vec<String> = fused.into_iter().take(FUSED_TOP).collect();

    if top.is_empty() {
        // Fallback: fresh workspace may have conversations ingested but nothing
        // in node_fts (extraction hasn't run). Return recent conversations so
        // ask_question has something to ground on.
        let mut stmt = conn
            .prepare(
                "SELECT id FROM node WHERE type = 'conversation' ORDER BY updated DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let ids: Vec<String> = stmt
            .query_map(params![FUSED_TOP as i64], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        if ids.is_empty() {
            return Ok(GraphData {
                nodes: vec![],
                links: vec![],
            });
        }
        return fetch_graph_for_nodes(&conn, &ids);
    }

    fetch_graph_for_nodes(&conn, &top)
}

#[command]
pub async fn lens_territory(workspace_path: String) -> Result<GraphData, String> {
    let conn = crate::db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT t.id, COUNT(DISTINCT e.src_id) AS conv_count
             FROM node t
             JOIN edge e ON e.dst_id = t.id AND e.type = 'belongs_to'
             JOIN node c ON c.id = e.src_id AND c.type = 'conversation'
             WHERE t.type = 'topic'
             GROUP BY t.id
             ORDER BY conv_count DESC
             LIMIT 20",
        )
        .map_err(|e| e.to_string())?;

    let ids: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    fetch_graph_for_nodes(&conn, &ids)
}

#[derive(Serialize)]
pub struct DriftCell {
    pub topic: String,
    pub month: String,
    pub vol: i64,
}

#[command]
pub async fn lens_drift_data(workspace_path: String) -> Result<Vec<DriftCell>, String> {
    let conn = crate::db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT t.name AS topic,
                    strftime('%Y-%m', datetime(json_extract(c.props, '$.chatgpt_created'), 'unixepoch')) AS month,
                    COUNT(DISTINCT c.id) AS vol
             FROM node t
             JOIN edge e ON e.dst_id = t.id AND e.type = 'belongs_to'
             JOIN node c ON c.id = e.src_id AND c.type = 'conversation'
             WHERE t.type = 'topic'
               AND json_extract(c.props, '$.chatgpt_created') >= strftime('%s', 'now', '-12 months')
             GROUP BY t.id, month
             ORDER BY month, topic",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([], |r| {
            Ok(DriftCell {
                topic: r.get(0)?,
                month: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                vol: r.get(2)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    for r in rows.flatten() {
        out.push(r);
    }
    Ok(out)
}

#[command]
pub async fn lens_drift(workspace_path: String) -> Result<GraphData, String> {
    // Visual fallback returns the same topic territory so the graph panel
    // isn't empty when the heatmap isn't rendered.
    lens_territory(workspace_path).await
}

#[command]
pub async fn lens_bridges(workspace_path: String) -> Result<GraphData, String> {
    let conn = crate::db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;

    // §11 Bridges: concept c "bridges" topics it reaches via 2-hop
    // concept <-discusses- conversation -belongs_to-> topic. Flag concepts
    // that reach 3+ distinct topics.
    let mut stmt = conn
        .prepare(
            "SELECT c.id, COUNT(DISTINCT t.id) AS topic_count
             FROM node c
             JOIN edge e1 ON e1.dst_id = c.id AND e1.type = 'discusses'
             JOIN edge e2 ON e2.src_id = e1.src_id AND e2.type = 'belongs_to'
             JOIN node t ON t.id = e2.dst_id AND t.type = 'topic'
             WHERE c.type = 'concept'
             GROUP BY c.id
             HAVING topic_count >= 3
             ORDER BY topic_count DESC
             LIMIT 10",
        )
        .map_err(|e| e.to_string())?;

    let ids: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    fetch_graph_for_nodes(&conn, &ids)
}

#[command]
pub async fn lens_orphans(workspace_path: String) -> Result<GraphData, String> {
    let conn = crate::db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT c.id
             FROM node c
             JOIN edge e ON e.dst_id = c.id AND e.type = 'discusses'
             WHERE c.type = 'concept'
               AND c.created < datetime('now', '-90 days')
             GROUP BY c.id
             HAVING COUNT(DISTINCT e.src_id) = 1
             ORDER BY c.created DESC
             LIMIT 20",
        )
        .map_err(|e| e.to_string())?;

    let ids: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    fetch_graph_for_nodes(&conn, &ids)
}

#[command]
pub async fn orphan_promote(workspace_path: String, node_id: String) -> Result<(), String> {
    let conn = crate::db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE node SET props = json_set(coalesce(props, '{}'), '$.orphan_state', 'promoted') WHERE id = ?1",
        params![node_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub async fn orphan_archive(workspace_path: String, node_id: String) -> Result<(), String> {
    let conn = crate::db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE node SET props = json_set(coalesce(props, '{}'), '$.orphan_state', 'archived') WHERE id = ?1",
        params![node_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Serialize)]
pub struct PathResult {
    pub path: Vec<String>,
    pub strength: i64,
    pub graph: GraphData,
}

#[command]
pub async fn lens_path(
    workspace_path: String,
    from_name: Option<String>,
    to_name: Option<String>,
) -> Result<PathResult, String> {
    let conn = crate::db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;

    let resolve = |name: &str| -> Option<String> {
        conn.query_row(
            "SELECT id FROM node WHERE name = ?1 ORDER BY type LIMIT 1",
            params![name],
            |r| r.get::<_, String>(0),
        )
        .ok()
    };

    let from = from_name.as_deref().and_then(resolve);
    let to = to_name.as_deref().and_then(resolve);

    let (from_id, to_id) = match (from, to) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            return Ok(PathResult {
                path: vec![],
                strength: 0,
                graph: GraphData {
                    nodes: vec![],
                    links: vec![],
                },
            });
        }
    };

    // BFS up to depth 3 from `from_id` looking for `to_id`. Undirected.
    const MAX_DEPTH: usize = 3;
    let mut visited: HashMap<String, Option<String>> = HashMap::new();
    visited.insert(from_id.clone(), None);
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((from_id.clone(), 0));

    let mut found = false;
    while let Some((cur, depth)) = queue.pop_front() {
        if cur == to_id {
            found = true;
            break;
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let mut stmt = conn
            .prepare("SELECT dst_id FROM edge WHERE src_id = ?1 UNION SELECT src_id FROM edge WHERE dst_id = ?1")
            .map_err(|e| e.to_string())?;
        let neighbors: Vec<String> = stmt
            .query_map(params![cur], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        for n in neighbors {
            if !visited.contains_key(&n) {
                visited.insert(n.clone(), Some(cur.clone()));
                queue.push_back((n, depth + 1));
            }
        }
    }

    if !found {
        return Ok(PathResult {
            path: vec![],
            strength: 0,
            graph: GraphData {
                nodes: vec![],
                links: vec![],
            },
        });
    }

    let mut path = Vec::new();
    let mut cursor = Some(to_id.clone());
    while let Some(id) = cursor {
        path.push(id.clone());
        cursor = visited.get(&id).cloned().flatten();
    }
    path.reverse();

    // Path strength = min conversation backing across edges.
    let mut strength: i64 = i64::MAX;
    for pair in path.windows(2) {
        let count: i64 = conn
            .query_row(
                "SELECT coalesce(json_extract(props, '$.count'), 1)
                 FROM edge
                 WHERE (src_id = ?1 AND dst_id = ?2) OR (src_id = ?2 AND dst_id = ?1)
                 LIMIT 1",
                params![pair[0], pair[1]],
                |r| r.get(0),
            )
            .unwrap_or(1);
        if count < strength {
            strength = count;
        }
    }
    if strength == i64::MAX {
        strength = 0;
    }

    let graph = fetch_graph_for_nodes(&conn, &path)?;

    Ok(PathResult {
        path,
        strength,
        graph,
    })
}

#[derive(Serialize)]
pub struct ChatAnswer {
    pub text: String,
    pub graph: GraphData,
}

#[derive(serde::Deserialize)]
pub struct SessionState {
    pub active_frame: Vec<String>,
    pub recent_turns: Vec<TurnRecord>,
    pub summary: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct TurnRecord {
    pub role: String,
    pub text: String,
}

#[command]
pub async fn ask_question(
    query: String,
    workspace_path: String,
    base_url: String,
    api_key: String,
    model: String,
    embed_model: Option<String>,
    // Separate embedding endpoint. See `extract_conversation` for the same
    // rationale: the chat endpoint may be a cloud provider that doesn't serve
    // the workspace's embedding model, so we route embeddings independently.
    embed_base_url: Option<String>,
    embed_api_key: Option<String>,
    session: Option<SessionState>,
) -> Result<ChatAnswer, String> {
    let client = crate::llm::openai::OpenAiCompatibleClient::new(base_url.clone(), api_key.clone());

    let graph = search_nodes(
        query.clone(),
        workspace_path.clone(),
        Some(embed_base_url.clone().unwrap_or_else(|| "http://localhost:11434/v1".to_string())),
        Some(embed_api_key.clone().unwrap_or_default()),
        embed_model.clone(),
    )
    .await?;

    if graph.nodes.is_empty() {
        return Ok(ChatAnswer {
            text: "No matches in the archive.".to_string(),
            graph,
        });
    }

    let conn = crate::db::open_workspace_db(Path::new(&workspace_path))
        .map_err(|e| e.to_string())?;

    let mut conv_ids: Vec<String> = graph
        .nodes
        .iter()
        .filter(|n| n.group == "conversation")
        .map(|n| n.id.clone())
        .collect();

    if conv_ids.is_empty() {
        for link in &graph.links {
            if let Ok(ty) = conn.query_row(
                "SELECT type FROM node WHERE id = ?1",
                params![&link.source],
                |r| r.get::<_, String>(0),
            ) {
                if ty == "conversation" && !conv_ids.contains(&link.source) {
                    conv_ids.push(link.source.clone());
                }
            }
        }
    }

    conv_ids.truncate(3);
    if conv_ids.is_empty() {
        return Ok(ChatAnswer {
            text: "The archive does not show this.".to_string(),
            graph,
        });
    }

    let mut context_chunks = String::new();
    let mut chunk_id = 1;
    let mut citation_map: Vec<String> = Vec::new();

    for cid in &conv_ids {
        let mut stmt = conn
            .prepare(
                "SELECT message_id, text_content FROM message_index
                 WHERE conversation_id = ?1 AND is_system = 0
                 ORDER BY create_time ASC LIMIT 15",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query(params![cid]).map_err(|e| e.to_string())?;

        context_chunks.push_str(&format!("Conversation {}:\n", cid));
        while let Ok(Some(row)) = rows.next() {
            let msg_id: String = row.get(0).unwrap_or_default();
            let blob: Vec<u8> = row.get(1).unwrap_or_default();
            if let Ok(decompressed) = zstd::stream::decode_all(&blob[..]) {
                if let Ok(text) = String::from_utf8(decompressed) {
                    let snippet = if text.len() > 800 {
                        format!("{}...", &text[0..800])
                    } else {
                        text
                    };
                    context_chunks.push_str(&format!("[{}] {}\n", chunk_id, snippet));
                    citation_map.push(format!("{}:{}", cid, msg_id));
                    chunk_id += 1;
                }
            }
        }
        context_chunks.push('\n');
    }

    if citation_map.is_empty() {
        return Ok(ChatAnswer {
            text: "The archive does not show this.".to_string(),
            graph,
        });
    }

    let active_frame_names: Vec<String> = if let Some(ref s) = session {
        let mut names = Vec::new();
        for id in &s.active_frame {
            if let Ok(n) = conn.query_row(
                "SELECT name FROM node WHERE id = ?1",
                params![id],
                |r| r.get::<_, String>(0),
            ) {
                names.push(n);
            }
        }
        names
    } else {
        Vec::new()
    };

    let active_frame_block = if active_frame_names.is_empty() {
        "(none)".to_string()
    } else {
        active_frame_names.join(", ")
    };

    let recent_turns_block = match &session {
        Some(s) if !s.recent_turns.is_empty() => s
            .recent_turns
            .iter()
            .map(|t| format!("{}: {}", t.role, t.text))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => "(none)".to_string(),
    };

    let session_summary_block = session
        .as_ref()
        .and_then(|s| s.summary.clone())
        .unwrap_or_else(|| "(none)".to_string());

    let prompt = format!(
        r#"ROLE
You answer questions about the user's own past ChatGPT conversations using ONLY
the CONTEXT provided. You do not use outside knowledge.

ACTIVE FRAME (what the user is currently looking at):
{}

SESSION SUMMARY (older turns compressed):
{}

LAST 5 TURNS (verbatim):
{}

CONTEXT (ranked, fetched for this question):
{}

QUESTION: {}

RULES
- Use ACTIVE FRAME to resolve pronouns ("it", "that one").
- Every claim gets a [^n] marker matching CONTEXT source IDs.
- If CONTEXT does not support a claim, say "the archive does not show this".
- Executive tone. Answer first. Under 200 words unless synthesis.

ANSWER:"#,
        active_frame_block, session_summary_block, recent_turns_block, context_chunks, query
    );

    let response = client
        .chat(
            &model,
            vec![Message {
                role: "user".to_string(),
                content: prompt,
            }],
            ChatOpts {
                temperature: 0.2,
                json_mode: false,
                max_tokens: Some(700),
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    Ok(ChatAnswer {
        text: response.message.content,
        graph,
    })
}
