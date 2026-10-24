use crate::types::{ChatRequest, ChatResponse, ModelInfo};
use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;

pub type ChatStream = std::pin::Pin<Box<dyn Stream<Item = Result<String>> + Send>>;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;
    fn chat_stream(&self, request: ChatRequest) -> ChatStream;
    fn list_models(&self) -> Vec<ModelInfo>;
    fn provider_id(&self) -> &str;
}
