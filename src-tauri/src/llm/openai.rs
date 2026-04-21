use super::{LlmClient, Message, ChatOpts, ChatResponse, HealthStatus};
use async_trait::async_trait;
use serde_json::{json, Value};
use anyhow::{Result, anyhow};
use std::time::Duration;
use std::fs::OpenOptions;
use std::io::Write;

pub struct OpenAiCompatibleClient {
    pub base_url: String,
    pub api_key: String,
    pub client: reqwest::Client,
    /// True when the endpoint looks like Ollama. Gates Ollama-only request fields
    /// (e.g. `keep_alive`) so strict providers like Gemini don't 400 on them.
    pub is_ollama: bool,
}

impl OpenAiCompatibleClient {
    pub fn new(base_url: String, api_key: String) -> Self {
        // Strip trailing slash so `format!("{}/models", base_url)` never produces
        // a double slash. Gemini's OpenAI-compat endpoint 404s on `.../openai//models`.
        let base_url = base_url.trim_end_matches('/').to_string();
        let is_ollama = base_url.contains("localhost")
            || base_url.contains("127.0.0.1")
            || base_url.contains(":11434");
        Self {
            base_url,
            api_key,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            is_ollama,
        }
    }
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

fn coerce_message_content(content_val: &Value) -> String {
    if let Some(s) = content_val.as_str() {
        return s.to_string();
    }
    if let Some(parts) = content_val.as_array() {
        // Some OpenAI-compatible providers (notably Gemini) may return structured content
        // parts instead of a single string.
        let mut out = String::new();
        for part in parts {
            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                out.push_str(t);
            } else if let Some(t) = part.get("content").and_then(|v| v.as_str()) {
                out.push_str(t);
            }
        }
        return out;
    }
    if let Some(obj) = content_val.as_object() {
        // Fallback: try a couple of common shapes.
        if let Some(s) = obj
            .get("text")
            .and_then(|v| v.as_str())
            .or_else(|| obj.get("content").and_then(|v| v.as_str()))
        {
            return s.to_string();
        }
    }

    // Last resort: recursively collect any string leaves. This is intentionally
    // forgiving for providers that vary OpenAI-compat response shapes.
    fn collect_strings(v: &Value, out: &mut String, depth: usize) {
        if depth > 10 {
            return;
        }
        match v {
            Value::String(s) => out.push_str(s),
            Value::Array(a) => {
                for item in a {
                    collect_strings(item, out, depth + 1);
                }
            }
            Value::Object(m) => {
                // Prefer common “text-like” keys first to avoid concatenating unrelated strings.
                for k in ["text", "content", "value", "output_text"] {
                    if let Some(v2) = m.get(k) {
                        collect_strings(v2, out, depth + 1);
                    }
                }
                for (k, v2) in m {
                    if ["text", "content", "value", "output_text"].contains(&k.as_str()) {
                        continue;
                    }
                    // Skip common metadata keys seen in LLM responses.
                    if ["role", "type", "name", "id", "model", "index", "finish_reason"].contains(&k.as_str()) {
                        continue;
                    }
                    collect_strings(v2, out, depth + 1);
                }
            }
            _ => {}
        }
    }

    let mut out = String::new();
    collect_strings(content_val, &mut out, 0);
    out
}

fn coerce_message_from_openai_choice(choice: &Value) -> String {
    // Standard chat-completions shape: choices[].message.content
    let message = &choice["message"];
    if !message.is_null() {
        // First preference: explicit content.
        let content = coerce_message_content(&message["content"]);
        if !content.trim().is_empty() {
            return content;
        }

        // Some providers omit `content` but include text in other nested fields
        // (e.g. `parts`). Try coercing the entire message object.
        let msg_fallback = coerce_message_content(message);
        if !msg_fallback.trim().is_empty() {
            return msg_fallback;
        }

        // Some providers return structured tool/function calls with arguments instead.
        if let Some(arr) = message.get("tool_calls").and_then(|v| v.as_array()) {
            let mut out = String::new();
            for tc in arr {
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                {
                    out.push_str(args);
                }
            }
            if !out.trim().is_empty() {
                return out;
            }
        }
        if let Some(args) = message
            .get("function_call")
            .and_then(|f| f.get("arguments"))
            .and_then(|v| v.as_str())
        {
            if !args.trim().is_empty() {
                return args.to_string();
            }
        }
    }

    // Legacy/non-chat compat: choices[].text
    if let Some(s) = choice.get("text").and_then(|v| v.as_str()) {
        return s.to_string();
    }

    String::new()
}

/// True for models that burn output tokens on hidden reasoning before producing
/// visible content. These need a much larger `max_tokens` than the task itself
/// would suggest — otherwise the entire budget is consumed thinking and the
/// response comes back empty (HTTP 200, `finish_reason: MAX_TOKENS`, no text).
fn model_is_thinking(name: &str) -> bool {
    let n = name.to_lowercase();
    // OpenAI-compat model IDs may include the `models/` prefix via Gemini;
    // strip it so prefix matches work.
    let n = n.strip_prefix("models/").unwrap_or(&n);
    if n.contains("thinking") || n.contains("reasoning") {
        return true;
    }
    // Gemini 2.5 Pro is a thinking model by default; flash variants aren't
    // unless the name contains `-thinking`.
    if n.starts_with("gemini-2.5-pro") || n == "gemini-2.5-pro" {
        return true;
    }
    // OpenAI o-series reasoning models.
    if n == "o1" || n == "o3" || n == "o4" || n.starts_with("o1-") || n.starts_with("o3-") || n.starts_with("o4-") {
        return true;
    }
    false
}

// Spec §14: 3 attempts with 1s/3s/9s exponential backoff. The 9s wait would
// precede a 4th attempt, so we sleep 1s before attempt 2 and 3s before attempt 3.
async fn with_retry<F, Fut, T>(mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let backoffs = [Duration::from_secs(1), Duration::from_secs(3)];
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..3usize {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = Some(e);
                if attempt < backoffs.len() {
                    tokio::time::sleep(backoffs[attempt]).await;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("retry exhausted")))
}

async fn post_json(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: &Value,
) -> Result<Value> {
    let resp = client
        .post(url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        let snippet: String = text.chars().take(300).collect();
        agent_log(
            "C",
            "src-tauri/src/llm/openai.rs:post_json",
            "http_error",
            json!({
                "url": url,
                "status": status.as_u16(),
                "body_snippet": snippet,
            }),
        );
        return Err(anyhow!("HTTP {}: {}", status, text));
    }
    let json: Value = serde_json::from_str(&text)
        .map_err(|e| anyhow!("Invalid JSON from endpoint: {}", e))?;
    Ok(json)
}

#[async_trait]
impl LlmClient for OpenAiCompatibleClient {
    async fn chat(&self, model: &str, messages: Vec<Message>, opts: ChatOpts) -> Result<ChatResponse> {
        let mut body = json!({
            "model": model,
            "messages": messages,
            "temperature": opts.temperature,
        });
        // Ollama-specific: keep the model resident between calls so concurrent
        // workers don't pay repeated cold-start costs. Gemini's strict schema
        // rejects unknown fields with 400, so only send this to Ollama.
        if self.is_ollama {
            body.as_object_mut()
                .unwrap()
                .insert("keep_alive".to_string(), json!("10m"));
        }
        if opts.json_mode {
            body.as_object_mut().unwrap().insert(
                "response_format".to_string(),
                json!({ "type": "json_object" }),
            );
        }
        if let Some(limit) = opts.max_tokens {
            // Thinking / reasoning models (Gemini 2.5-pro, OpenAI o-series,
            // any *-thinking / *-reasoning variant) consume output tokens on
            // hidden chain-of-thought BEFORE producing the visible answer.
            // A 900-token cap is entirely eaten by thinking, and the OpenAI-
            // compat layer returns HTTP 200 with content_len=0. Guarantee
            // these models a large enough budget that visible output actually
            // makes it out. Non-thinking models ignore the larger cap (they
            // stop when done), so this is safe.
            let adjusted = if !self.is_ollama && model_is_thinking(model) {
                limit.max(8000)
            } else {
                limit
            };
            body.as_object_mut()
                .unwrap()
                .insert("max_tokens".to_string(), json!(adjusted));
        }

        let url = format!("{}/chat/completions", self.base_url);
        agent_log(
            "A",
            "src-tauri/src/llm/openai.rs:chat",
            "chat_request",
            json!({
                "base_url": self.base_url,
                "url": url,
                "is_ollama": self.is_ollama,
                "model": model,
                "json_mode": opts.json_mode,
                "max_tokens": opts.max_tokens,
                "temperature": opts.temperature,
                "message_count": body.get("messages").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
                "sent_keep_alive": self.is_ollama,
                "sent_response_format": opts.json_mode,
            }),
        );
        let resp = with_retry(|| async {
            post_json(&self.client, &url, &self.api_key, &body).await
        })
        .await?;

        let role = resp["choices"][0]["message"]["role"]
            .as_str()
            .unwrap_or("assistant")
            .to_string();
        let content = coerce_message_from_openai_choice(&resp["choices"][0]);
        agent_log(
            "B",
            "src-tauri/src/llm/openai.rs:chat",
            "chat_response_summary",
            json!({
                "url": url,
                "model": model,
                "content_len": content.len(),
                "content_prefix": content.chars().take(160).collect::<String>(),
            }),
        );

        Ok(ChatResponse {
            message: Message { role, content },
        })
    }

    async fn embed(&self, model: &str, input: &str) -> Result<Vec<f32>> {
        let body = json!({ "model": model, "input": input });
        let url = format!("{}/embeddings", self.base_url);

        let resp = with_retry(|| async {
            post_json(&self.client, &url, &self.api_key, &body).await
        })
        .await?;

        let embeddings_val = resp["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| anyhow!("Invalid embedding response format"))?;

        let mut vec = Vec::with_capacity(embeddings_val.len());
        for v in embeddings_val {
            vec.push(v.as_f64().unwrap_or(0.0) as f32);
        }

        Ok(vec)
    }

    async fn embed_many(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        // Ollama's OpenAI-compatible endpoint accepts `input` as an array and
        // returns one embedding per item in order. A single round-trip replaces
        // N sequential calls.
        let body = json!({ "model": model, "input": inputs });
        let url = format!("{}/embeddings", self.base_url);

        let resp = with_retry(|| async {
            post_json(&self.client, &url, &self.api_key, &body).await
        })
        .await?;

        let data = resp["data"]
            .as_array()
            .ok_or_else(|| anyhow!("Invalid batch embedding response"))?;

        let mut out = Vec::with_capacity(data.len());
        for item in data {
            let arr = item["embedding"]
                .as_array()
                .ok_or_else(|| anyhow!("Missing embedding in batch response"))?;
            let mut v = Vec::with_capacity(arr.len());
            for x in arr {
                v.push(x.as_f64().unwrap_or(0.0) as f32);
            }
            out.push(v);
        }
        Ok(out)
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        let url = format!("{}/models", self.base_url);
        let http = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|e| anyhow!("network error reaching {}: {}", url, e))?;

        let status = http.status();
        let text = http.text().await.unwrap_or_default();
        if !status.is_success() {
            // Truncate so a 2 MB HTML error page doesn't flood the UI.
            let snippet: String = text.chars().take(300).collect();
            return Err(anyhow!("HTTP {} from {}: {}", status, url, snippet));
        }

        let resp: Value = serde_json::from_str(&text)
            .map_err(|e| anyhow!("non-JSON response from {}: {} (body: {})", url, e, text.chars().take(200).collect::<String>()))?;

        let data = resp["data"]
            .as_array()
            .ok_or_else(|| anyhow!("unexpected models response shape (no `data` array): {}", text.chars().take(200).collect::<String>()))?;

        let mut models = Vec::new();
        for model in data {
            if let Some(id) = model["id"].as_str() {
                models.push(id.to_string());
            }
        }

        Ok(models)
    }

    async fn health(&self) -> Result<HealthStatus> {
        let mut status = HealthStatus::default();

        // §14 probe 1: list models. Capture the error so the UI can show
        // *why* the endpoint is offline instead of a silent "Offline" label.
        let models = match self.list_models().await {
            Ok(m) => {
                status.models = !m.is_empty();
                if !status.models {
                    status.models_error = Some("Provider returned an empty model list.".to_string());
                }
                Some(m)
            }
            Err(e) => {
                status.models_error = Some(e.to_string());
                None
            }
        };

        let chat_model = models
            .as_ref()
            .and_then(|m| m.iter().find(|s| !s.contains("embed")).cloned())
            .unwrap_or_else(|| "default".to_string());

        // §14 probe 2: chat completions with json_mode.
        let chat_res = self
            .chat(
                &chat_model,
                vec![Message {
                    role: "user".to_string(),
                    content: "ping".to_string(),
                }],
                ChatOpts {
                    temperature: 0.0,
                    json_mode: true,
                    max_tokens: Some(20),
                },
            )
            .await;

        match chat_res {
            Ok(_) => {
                status.chat = true;
                status.json_mode = true;
            }
            Err(json_err) => {
                let chat_res_no_json = self
                    .chat(
                        &chat_model,
                        vec![Message {
                            role: "user".to_string(),
                            content: "ping".to_string(),
                        }],
                        ChatOpts {
                            temperature: 0.0,
                            json_mode: false,
                            max_tokens: Some(20),
                        },
                    )
                    .await;
                match chat_res_no_json {
                    Ok(_) => {
                        status.chat = true;
                        status.json_mode = false;
                    }
                    Err(plain_err) => {
                        // Report the json_mode error since that's the primary
                        // mode extraction relies on; fall back to the plain
                        // error only if json_err was just the json_mode rejection.
                        status.chat_error = Some(format!(
                            "json_mode: {}; plain: {}",
                            json_err, plain_err
                        ));
                    }
                }
            }
        }

        // §14 probe 3: embeddings. Pick the first model in the endpoint's
        // own /models list that looks like an embedding model. If the endpoint
        // doesn't advertise one, skip the probe rather than hardcoding a
        // model name — an honest "not reachable" beats a false signal.
        let embed_model = models
            .as_ref()
            .and_then(|m| m.iter().find(|s| s.to_lowercase().contains("embed")).cloned());

        match embed_model {
            None => {
                status.embeddings_error = Some(
                    "Endpoint didn't advertise any embedding model (nothing containing 'embed' in /models). Check Settings → LLM Routing (Embedding) for the exact model your server is serving.".to_string(),
                );
            }
            Some(name) => match self.embed(&name, "ping").await {
                Ok(_) => {
                    status.embeddings = true;
                }
                Err(e) => {
                    status.embeddings_error = Some(e.to_string());
                }
            },
        }

        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::{coerce_message_content, coerce_message_from_openai_choice};
    use serde_json::json;

    #[test]
    fn coerce_message_content_accepts_string() {
        let v = json!("hello");
        assert_eq!(coerce_message_content(&v), "hello");
    }

    #[test]
    fn coerce_message_content_accepts_array_parts() {
        let v = json!([
            { "type": "text", "text": "Hello " },
            { "type": "text", "text": "world" }
        ]);
        assert_eq!(coerce_message_content(&v), "Hello world");
    }

    #[test]
    fn coerce_message_content_accepts_object_text() {
        let v = json!({ "text": "hi" });
        assert_eq!(coerce_message_content(&v), "hi");
    }

    #[test]
    fn coerce_message_content_accepts_object_content() {
        let v = json!({ "content": "yo" });
        assert_eq!(coerce_message_content(&v), "yo");
    }

    #[test]
    fn coerce_message_content_recurses_into_nested_shapes() {
        let v = json!({
            "parts": [
                { "text": { "value": "Hello" } },
                { "text": " " },
                { "nested": { "output_text": "world" } }
            ]
        });
        assert_eq!(coerce_message_content(&v), "Hello world");
    }

    #[test]
    fn coerce_message_from_choice_prefers_message_content() {
        let choice = json!({
            "message": { "content": "hi" }
        });
        assert_eq!(coerce_message_from_openai_choice(&choice), "hi");
    }

    #[test]
    fn coerce_message_from_choice_reads_tool_calls_arguments() {
        let choice = json!({
            "message": {
                "content": "",
                "tool_calls": [
                    { "type": "function", "function": { "name": "emit", "arguments": "{\"topics\":[]}" } }
                ]
            }
        });
        assert_eq!(coerce_message_from_openai_choice(&choice), "{\"topics\":[]}");
    }

    #[test]
    fn coerce_message_from_choice_falls_back_to_message_parts() {
        let choice = json!({
            "message": {
                "role": "assistant",
                "parts": [
                    { "text": "Hello" },
                    { "text": " world" }
                ]
            }
        });
        assert_eq!(coerce_message_from_openai_choice(&choice), "Hello world");
    }

    #[test]
    fn coerce_message_from_choice_reads_function_call_arguments() {
        let choice = json!({
            "message": {
                "content": "",
                "function_call": { "name": "emit", "arguments": "{\"entities\":[]}" }
            }
        });
        assert_eq!(coerce_message_from_openai_choice(&choice), "{\"entities\":[]}");
    }

    #[test]
    fn coerce_message_from_choice_falls_back_to_text() {
        let choice = json!({ "text": "legacy" });
        assert_eq!(coerce_message_from_openai_choice(&choice), "legacy");
    }
}
