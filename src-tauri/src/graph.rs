use serde::Serialize;
use rusqlite::Connection;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;

#[derive(Serialize)]
pub struct Node {
    pub id: String,
    pub group: String,
    pub name: String,
    pub props: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct Link {
    pub source: String,
    pub target: String,
    pub value: i64,
    pub edge_type: String,
}

#[derive(Serialize)]
pub struct GraphData {
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
}

// #region agent log
fn agent_log(hypothesis_id: &str, location: &str, message: &str, data: serde_json::Value) {
    let payload = serde_json::json!({
        "sessionId": "a5604b",
        "runId": "pre-fix",
        "hypothesisId": hypothesis_id,
        "location": location,
        "message": message,
        "data": data,
        "timestamp": (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)),
    });
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/Users/saurav/projects/apps/chat-analyzer/.cursor/debug-a5604b.log")
    {
        let _ = writeln!(f, "{}", payload.to_string());
    }
}
// #endregion

pub fn get_initial_graph(conn: &Connection) -> Result<GraphData, String> {
    // Seed from recent conversations (the user's primary entry point), then
    // expand to derived nodes (concepts/topics/entities/patterns) connected by edges.
    // This keeps the initial view meaningful and connected even when many
    // extracted nodes share the same `updated` timestamp.
    let seed_conversations = 12usize;
    let neighbor_cap = 120usize;

    let mut conv_stmt = conn
        .prepare(
            "SELECT id FROM node
             WHERE type = 'conversation'
             ORDER BY updated DESC
             LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let conv_ids: Vec<String> = conv_stmt
        .query_map([seed_conversations as i64], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    if conv_ids.is_empty() {
        agent_log("H4", "src-tauri/src/graph.rs:get_initial_graph", "no_conversations_seeded", serde_json::json!({}));
        return Ok(GraphData { nodes: vec![], links: vec![] });
    }

    // Pull edges that touch these conversations.
    let placeholders: Vec<String> = conv_ids.iter().map(|_| "?".to_string()).collect();
    let in_clause = placeholders.join(", ");
    let edge_query = format!(
        "SELECT src_id, dst_id, type
         FROM edge
         WHERE src_id IN ({})
         ORDER BY id DESC
         LIMIT {}",
        in_clause,
        neighbor_cap * 4
    );
    let params: Vec<&dyn rusqlite::ToSql> = conv_ids
        .iter()
        .map(|s| s as &dyn rusqlite::ToSql)
        .collect();

    let mut included: HashSet<String> = conv_ids.iter().cloned().collect();
    let mut links: Vec<Link> = Vec::new();
    let mut newly_found: Vec<String> = Vec::new();
    if let Ok(mut edge_stmt) = conn.prepare(&edge_query) {
        if let Ok(edge_iter) = edge_stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        }) {
            for e in edge_iter.flatten() {
                let (src, dst, typ) = e;
                if !included.contains(&src) {
                    newly_found.push(src.clone());
                }
                if !included.contains(&dst) {
                    newly_found.push(dst.clone());
                }
                links.push(Link {
                    source: src,
                    target: dst,
                    value: 1,
                    edge_type: typ,
                });
            }
        }
    }

    newly_found.sort();
    newly_found.dedup();
    newly_found.truncate(neighbor_cap);
    for id in &newly_found {
        included.insert(id.clone());
    }

    agent_log(
        "H5",
        "src-tauri/src/graph.rs:get_initial_graph",
        "seed_and_neighbor_summary",
        serde_json::json!({
            "seed_conversations": seed_conversations,
            "neighbor_cap": neighbor_cap,
            "seed_count": conv_ids.len(),
            "seed_preview": conv_ids.iter().take(8).cloned().collect::<Vec<_>>(),
            "new_neighbors_count": newly_found.len(),
            "neighbor_preview": newly_found.iter().take(8).cloned().collect::<Vec<_>>(),
        }),
    );

    // Fetch all included nodes.
    let mut all_ids: Vec<String> = included.iter().cloned().collect();
    all_ids.sort();
    let placeholders: Vec<String> = all_ids.iter().map(|_| "?".to_string()).collect();
    let in_clause = placeholders.join(", ");
    let node_query = format!(
        "SELECT id, type, name, props
         FROM node
         WHERE id IN ({})",
        in_clause
    );
    let params: Vec<&dyn rusqlite::ToSql> = all_ids
        .iter()
        .map(|s| s as &dyn rusqlite::ToSql)
        .collect();
    let mut nodes: Vec<Node> = Vec::new();
    if let Ok(mut nstmt) = conn.prepare(&node_query) {
        if let Ok(iter) = nstmt.query_map(rusqlite::params_from_iter(params), |row| {
            let props_str: Option<String> = row.get(3).unwrap_or(None);
            let props = props_str.and_then(|s| serde_json::from_str(&s).ok());
            Ok(Node {
                id: row.get(0)?,
                group: row.get(1)?,
                name: row.get(2)?,
                props,
            })
        }) {
            for n in iter.flatten() {
                nodes.push(n);
            }
        }
    }

    // Filter links so both endpoints are included.
    links.retain(|l| included.contains(&l.source) && included.contains(&l.target));

    agent_log(
        "H6",
        "src-tauri/src/graph.rs:get_initial_graph",
        "graph_result_fingerprint",
        serde_json::json!({
            "node_count": nodes.len(),
            "link_count": links.len(),
            "node_preview": nodes.iter().take(8).map(|n| n.id.clone()).collect::<Vec<_>>(),
            "link_preview": links.iter().take(8).map(|l| format!("{}->{}:{}", l.source, l.target, l.edge_type)).collect::<Vec<_>>(),
        }),
    );

    eprintln!(
        "[graph] initial_graph nodes={} links={} (seed_cap={} neighbor_cap={})",
        nodes.len(),
        links.len(),
        seed_conversations,
        neighbor_cap
    );
    Ok(GraphData { nodes, links })
}
