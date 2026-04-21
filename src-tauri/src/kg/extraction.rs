use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractItem {
    pub name: String,
    #[serde(default)]
    pub item_type: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub message_ids: Vec<String>,
    #[serde(default)]
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractOutput {
    pub topics: Vec<ExtractItem>,
    pub entities: Vec<ExtractItem>,
    pub concepts: Vec<ExtractItem>,
    pub prompt_patterns: Vec<ExtractItem>,
}

fn coerce_items(arr: &[Value]) -> Vec<ExtractItem> {
    arr.iter()
        .filter_map(|v| v.as_object().cloned().map(Value::Object))
        .map(|raw| {
            let name = raw
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            let item_type = raw
                .get("type")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let description = raw
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let confidence = raw.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let message_ids = raw
                .get("message_ids")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            ExtractItem {
                name,
                item_type,
                description,
                confidence,
                message_ids,
                raw: raw.clone(),
            }
        })
        .filter(|i| !i.name.trim().is_empty())
        .collect()
}

fn get_array<'a>(obj: &'a serde_json::Map<String, Value>, keys: &[&str]) -> &'a [Value] {
    for k in keys {
        if let Some(Value::Array(a)) = obj.get(*k) {
            return a;
        }
    }
    &[]
}

/// Sanitize + validate the LLM extraction output into a stable shape.
///
/// This mirrors the "schema-first" approach from Understand-Anything: accept
/// common variants/aliases, coerce missing collections to empty, and never
/// crash on partial output.
pub fn sanitize_extraction_output(v: Value) -> Result<ExtractOutput> {
    match v {
        Value::Object(obj) => {
            // Key aliases: tolerate camelCase, and "patterns" naming drift.
            let topics = coerce_items(get_array(&obj, &["topics", "topic"]));
            let entities = coerce_items(get_array(&obj, &["entities", "entity"]));
            let concepts = coerce_items(get_array(&obj, &["concepts", "concept"]));
            let prompt_patterns = coerce_items(get_array(
                &obj,
                &[
                    "prompt_patterns",
                    "promptPatterns",
                    "patterns",
                    "prompt_pattern",
                ],
            ));

            Ok(ExtractOutput {
                topics,
                entities,
                concepts,
                prompt_patterns,
            })
        }
        // Some providers may emit a bare array; treat it as "concepts" by
        // default rather than failing the whole run.
        Value::Array(arr) => Ok(ExtractOutput {
            topics: vec![],
            entities: vec![],
            concepts: coerce_items(&arr),
            prompt_patterns: vec![],
        }),
        other => Err(anyhow!("invalid extraction JSON root (expected object/array), got {}", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_extraction_output;
    use serde_json::json;

    #[test]
    fn sanitizes_missing_keys_to_empty() {
        let out = sanitize_extraction_output(json!({"topics":[{"name":"x"}]})).unwrap();
        assert_eq!(out.topics.len(), 1);
        assert_eq!(out.entities.len(), 0);
        assert_eq!(out.concepts.len(), 0);
        assert_eq!(out.prompt_patterns.len(), 0);
    }

    #[test]
    fn accepts_alias_keys() {
        let out = sanitize_extraction_output(json!({"promptPatterns":[{"name":"p"}]})).unwrap();
        assert_eq!(out.prompt_patterns.len(), 1);
    }

    #[test]
    fn accepts_root_array_as_concepts() {
        let out = sanitize_extraction_output(json!([{"name":"c"}])).unwrap();
        assert_eq!(out.concepts.len(), 1);
    }
}

