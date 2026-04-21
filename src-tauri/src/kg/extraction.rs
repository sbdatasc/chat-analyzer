use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// Allowed subtypes per the wiki taxonomy. Items with a subtype outside this
// list get normalized to None so we don't persist noise (e.g. a hallucinated
// "computer program" subtype on an entity would be dropped to NULL and the
// user can relabel manually).
const ENTITY_SUBTYPES: &[&str] = &[
    "person",
    "organization",
    "tool",
    "product",
    "standard",
    "framework",
    "technology",
    "place",
];
const SOURCE_SUBTYPES: &[&str] = &["book", "article", "paper", "course", "talk"];
const CONCEPT_SUBTYPES: &[&str] = &["pattern"];

fn validate_subtype(kind: &str, raw: Option<&str>) -> Option<String> {
    let s = raw?.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    let allowed: &[&str] = match kind {
        "entity" => ENTITY_SUBTYPES,
        "source" => SOURCE_SUBTYPES,
        "concept" => CONCEPT_SUBTYPES,
        _ => return None,
    };
    if allowed.iter().any(|x| *x == s) {
        Some(s)
    } else {
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractItem {
    pub name: String,
    /// Wiki-taxonomy subtype (person/tool/book/etc). Empty / unknown → None.
    #[serde(default)]
    pub subtype: Option<String>,
    /// Legacy field (pre-taxonomy prompts emitted a generic `type` on
    /// entities). Kept because the extraction pipeline round-trips raw JSON
    /// through parked_extractions; new code should prefer `subtype`.
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
    pub sources: Vec<ExtractItem>,
}

fn coerce_items(kind: &str, arr: &[Value]) -> Vec<ExtractItem> {
    arr.iter()
        .filter_map(|v| v.as_object().cloned().map(Value::Object))
        .map(|raw| {
            let name = raw
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            // Accept `subtype` (new, preferred) or `type` (legacy entity
            // shape). Both get validated against the kind's whitelist.
            let raw_subtype = raw
                .get("subtype")
                .and_then(|v| v.as_str())
                .or_else(|| raw.get("type").and_then(|v| v.as_str()));
            let subtype = validate_subtype(kind, raw_subtype);
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
                subtype,
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
            // Key aliases: tolerate camelCase and naming drift.
            let topics = coerce_items("topic", get_array(&obj, &["topics", "topic"]));
            let entities = coerce_items("entity", get_array(&obj, &["entities", "entity"]));
            let sources = coerce_items("source", get_array(&obj, &["sources", "source"]));

            // `concepts` absorbs legacy `prompt_patterns` / `patterns` arrays —
            // they become concepts with subtype='pattern' per the wiki
            // taxonomy (rule #8: patterns are a concept subtype by default).
            let mut concepts = coerce_items("concept", get_array(&obj, &["concepts", "concept"]));
            let legacy_patterns = coerce_items(
                "concept",
                get_array(
                    &obj,
                    &[
                        "prompt_patterns",
                        "promptPatterns",
                        "patterns",
                        "prompt_pattern",
                    ],
                ),
            );
            for mut p in legacy_patterns {
                // Force the subtype even if the legacy item didn't mark it —
                // they came out of a "patterns" bucket, so that's their kind.
                p.subtype = Some("pattern".to_string());
                concepts.push(p);
            }

            Ok(ExtractOutput {
                topics,
                entities,
                concepts,
                sources,
            })
        }
        // Some providers may emit a bare array; treat it as "concepts" by
        // default rather than failing the whole run.
        Value::Array(arr) => Ok(ExtractOutput {
            topics: vec![],
            entities: vec![],
            concepts: coerce_items("concept", &arr),
            sources: vec![],
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
        assert_eq!(out.sources.len(), 0);
    }

    #[test]
    fn legacy_prompt_patterns_fold_into_concepts_with_subtype() {
        let out = sanitize_extraction_output(
            json!({"promptPatterns":[{"name":"p"}]}),
        )
        .unwrap();
        assert_eq!(out.concepts.len(), 1);
        assert_eq!(out.concepts[0].subtype.as_deref(), Some("pattern"));
    }

    #[test]
    fn accepts_root_array_as_concepts() {
        let out = sanitize_extraction_output(json!([{"name":"c"}])).unwrap();
        assert_eq!(out.concepts.len(), 1);
    }

    #[test]
    fn parses_entity_subtype_from_taxonomy_whitelist() {
        let out = sanitize_extraction_output(
            json!({"entities":[{"name":"ada","subtype":"person"},{"name":"weirdthing","subtype":"alien"}]}),
        )
        .unwrap();
        assert_eq!(out.entities[0].subtype.as_deref(), Some("person"));
        // Unknown subtype drops to None so we never persist noise.
        assert_eq!(out.entities[1].subtype, None);
    }

    #[test]
    fn parses_source_subtype_from_taxonomy_whitelist() {
        let out = sanitize_extraction_output(
            json!({"sources":[{"name":"ssg","subtype":"book"},{"name":"kant","subtype":"treatise"}]}),
        )
        .unwrap();
        assert_eq!(out.sources[0].subtype.as_deref(), Some("book"));
        assert_eq!(out.sources[1].subtype, None);
    }

    #[test]
    fn legacy_entity_type_field_is_read_as_subtype() {
        // Pre-taxonomy prompts emitted `type: "person"` on entities. The
        // sanitizer must still understand that shape.
        let out = sanitize_extraction_output(
            json!({"entities":[{"name":"ada","type":"person"}]}),
        )
        .unwrap();
        assert_eq!(out.entities[0].subtype.as_deref(), Some("person"));
    }
}

