pub mod openai;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use anyhow::Result;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ChatOpts {
    pub temperature: f32,
    pub json_mode: bool,
    /// Hard cap on generated tokens. Speeds up extraction by letting the model
    /// stop as soon as the JSON closes instead of drifting to its default
    /// context limit.
    pub max_tokens: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ChatResponse {
    pub message: Message,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct HealthStatus {
    pub chat: bool,
    pub embeddings: bool,
    pub models: bool,
    pub json_mode: bool,
    /// Failure detail from the /models probe. Present when `models == false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_error: Option<String>,
    /// Failure detail from the /chat/completions probe. Present when `chat == false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_error: Option<String>,
    /// Failure detail from the /embeddings probe. Present when `embeddings == false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embeddings_error: Option<String>,
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(&self, model: &str, messages: Vec<Message>, opts: ChatOpts) -> Result<ChatResponse>;
    async fn embed(&self, model: &str, input: &str) -> Result<Vec<f32>>;
    async fn embed_many(&self, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>>;
    async fn list_models(&self) -> Result<Vec<String>>;
    async fn health(&self) -> Result<HealthStatus>;
}
