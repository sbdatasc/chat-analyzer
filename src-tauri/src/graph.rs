use serde::Serialize;
use rusqlite::Connection;

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

pub fn get_initial_graph(conn: &Connection) -> Result<GraphData, String> {
    let mut stmt = conn.prepare("SELECT id, type, name, props FROM node ORDER BY updated DESC LIMIT 25").map_err(|e| e.to_string())?;
    
    let node_iter = stmt.query_map([], |row| {
        let props_str: Option<String> = row.get(3).unwrap_or(None);
        let props = props_str.and_then(|s| serde_json::from_str(&s).ok());
        Ok(Node {
            id: row.get(0)?,
            group: row.get(1)?,
            name: row.get(2)?,
            props,
        })
    }).map_err(|e| e.to_string())?;

    let mut nodes = Vec::new();
    let mut node_ids = Vec::new();
    for node in node_iter {
        if let Ok(n) = node {
            node_ids.push(n.id.clone());
            nodes.push(n);
        }
    }

    if node_ids.is_empty() {
        return Ok(GraphData { nodes: vec![], links: vec![] });
    }

    // Prepare a dynamic IN clause
    let placeholders: Vec<String> = node_ids.iter().map(|_| "?".to_string()).collect();
    let in_clause = placeholders.join(", ");
    let query = format!("SELECT src_id, dst_id, type FROM edge WHERE src_id IN ({}) AND dst_id IN ({})", in_clause, in_clause);

    // Bind parameters twice
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::new();
    for id in &node_ids {
        params.push(id as &dyn rusqlite::ToSql);
    }
    for id in &node_ids {
        params.push(id as &dyn rusqlite::ToSql);
    }

    let mut links = Vec::new();
    if let Ok(mut edge_stmt) = conn.prepare(&query) {
        if let Ok(edge_iter) = edge_stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok(Link {
                source: row.get(0)?,
                target: row.get(1)?,
                value: 1, // Default visual weight
                edge_type: row.get(2)?,
            })
        }) {
            for link in edge_iter {
                if let Ok(l) = link {
                    links.push(l);
                }
            }
        }
    }

    Ok(GraphData { nodes, links })
}
