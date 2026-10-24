use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationId {
    pub id: String,
    pub provider_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationContext {
    pub project_id: String,
    pub project_slug: String,
    pub lane_id: String,
    pub lane_slug: String,
    pub run_id: String,
    pub flow_name: Option<String>,
    pub flow_number: Option<u64>,
    pub run_description: Option<String>,
    pub stage_name: Option<String>,
    pub stage_execution_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub sender: String,
    pub sender_id: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationType {
    General,
    DirectMessage,
    Flow,
    Run,
    Stage,
}

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("conversation not found: {0}")]
    ConversationNotFound(String),
    #[error("provider error: {0}")]
    ProviderError(String),
    #[error("authentication failed: {0}")]
    AuthError(String),
    #[error("rate limited")]
    RateLimited,
    #[error("network error: {0}")]
    NetworkError(String),
    #[error("{0}")]
    Other(String),
}

pub type ChatResult<T> = Result<T, ChatError>;

/// A chat session -- the only thing consumers need.
/// Dumb interface: send and receive. Nothing else.
pub trait ChatSession: Send + Sync {
    /// Send a message. Works from both sync and async contexts.
    /// The session knows whether to send as DM or topic message.
    fn send(&self, message: &str) -> Pin<Box<dyn Future<Output = ChatResult<()>> + Send + '_>>;

    /// Block waiting for the next user message. For interactive stages.
    /// Returns the message content.
    fn receive(&self) -> Pin<Box<dyn Future<Output = ChatResult<String>> + Send + '_>>;
}

#[async_trait]
pub trait ChatProvider: Send + Sync {
    fn provider_id(&self) -> &str;

    async fn create_conversation(
        &self,
        ctx: &ConversationContext,
        kind: ConversationType,
    ) -> ChatResult<ConversationId>;

    async fn send_message(&self, conv: &ConversationId, message: &ChatMessage) -> ChatResult<()>;

    fn listen(
        &self,
        conv: &ConversationId,
    ) -> Pin<Box<dyn Stream<Item = ChatResult<ChatMessage>> + Send>>;

    async fn get_history(
        &self,
        conv: &ConversationId,
        limit: usize,
    ) -> ChatResult<Vec<ChatMessage>>;

    async fn close_conversation(&self, conv: &ConversationId) -> ChatResult<()>;

    /// Create a DM session with a user (for Chief).
    #[allow(clippy::type_complexity)]
    fn create_dm_session(
        &self,
        _user_identifier: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = ChatResult<Arc<dyn ChatSession>>> + Send + '_>>
    {
        Box::pin(async { Err(ChatError::Other("create_dm_session not implemented".into())) })
    }

    /// Create a flow session on a project channel (for stage agents).
    #[allow(clippy::type_complexity)]
    fn create_flow_session(
        &self,
        _ctx: &ConversationContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ChatResult<Arc<dyn ChatSession>>> + Send + '_>>
    {
        Box::pin(async {
            Err(ChatError::Other(
                "create_flow_session not implemented".into(),
            ))
        })
    }
}

use std::sync::Mutex as StdMutex;

/// Bridge letting webui users feed input into a waiting stage and observe
/// what the stage asked. Registered per run while a flow has a chat session.
pub struct WebInputBridge {
    queue: StdMutex<std::collections::VecDeque<(u64, String)>>,
    notify: tokio::sync::Notify,
    seq: std::sync::atomic::AtomicU64,
    transcript: StdMutex<Vec<String>>,
}

impl WebInputBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: StdMutex::new(std::collections::VecDeque::new()),
            notify: tokio::sync::Notify::new(),
            seq: std::sync::atomic::AtomicU64::new(1),
            transcript: StdMutex::new(Vec::new()),
        })
    }

    /// Queue a user message for the waiting stage. Returns the message's
    /// sequence number (used to cancel it while still queued).
    pub fn send_user_input(&self, text: &str) -> u64 {
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.queue
            .lock()
            .unwrap()
            .push_back((seq, text.to_string()));
        self.notify.notify_one();
        seq
    }

    /// Cancel a queued (not yet consumed) message. True when removed.
    pub fn cancel_input(&self, seq: u64) -> bool {
        let mut q = self.queue.lock().unwrap();
        let before = q.len();
        q.retain(|(s, _)| *s != seq);
        q.len() != before
    }

    /// Messages queued but not yet consumed by the stage, oldest first.
    pub fn queued(&self) -> Vec<(u64, String)> {
        self.queue.lock().unwrap().iter().cloned().collect()
    }

    /// Record a stage-originated message (question/output) for webui display.
    pub fn record_stage_message(&self, message: &str) {
        let mut t = self.transcript.lock().unwrap();
        if t.len() >= 100 {
            t.remove(0);
        }
        t.push(message.to_string());
    }

    pub fn transcript(&self) -> Vec<String> {
        self.transcript.lock().unwrap().clone()
    }

    /// Next queued user input, waiting for one to arrive. Consumed in FIFO
    /// order; the queue lives for the bridge's whole life so several sessions
    /// (flow resume) can share one bridge.
    async fn next_input(&self) -> Option<String> {
        loop {
            let popped = self.queue.lock().unwrap().pop_front().map(|(_, t)| t);
            if let Some(text) = popped {
                return Some(text);
            }
            self.notify.notified().await;
        }
    }
}

/// The bridge alone is a chat session: stages in chat-less deployments wait
/// for webui input instead of running unattended.
impl ChatSession for WebInputBridge {
    fn send(&self, message: &str) -> Pin<Box<dyn Future<Output = ChatResult<()>> + Send + '_>> {
        self.record_stage_message(message);
        Box::pin(async { Ok(()) })
    }

    fn receive(&self) -> Pin<Box<dyn Future<Output = ChatResult<String>> + Send + '_>> {
        Box::pin(async move {
            self.next_input()
                .await
                .ok_or_else(|| ChatError::Other("input channel closed".into()))
        })
    }
}

/// Wraps a provider chat session so a waiting stage can receive input from
/// either the chat provider (Zulip) or the webui bridge, whichever answers first.
/// The bridge receiver lives for the whole session (behind an async mutex) so it
/// can answer many waits and queued input is never dropped.
pub struct HybridSession {
    inner: Arc<dyn ChatSession>,
    bridge: Arc<WebInputBridge>,
}

impl HybridSession {
    pub fn new(inner: Arc<dyn ChatSession>, bridge: Arc<WebInputBridge>) -> Self {
        Self { inner, bridge }
    }
}

impl ChatSession for HybridSession {
    fn send(&self, message: &str) -> Pin<Box<dyn Future<Output = ChatResult<()>> + Send + '_>> {
        self.bridge.record_stage_message(message);
        self.inner.send(message)
    }

    fn receive(&self) -> Pin<Box<dyn Future<Output = ChatResult<String>> + Send + '_>> {
        Box::pin(async move {
            let mut provider_box = self.inner.receive();
            let bridge_input = self.bridge.next_input();
            tokio::select! {
                r = &mut provider_box => r,
                // bridge side never closes while the run lives; None means the
                // receiver is gone entirely — fall back to the provider.
                m = bridge_input => match m {
                    Some(text) => Ok(text),
                    None => provider_box.await,
                },
            }
        })
    }
}

#[cfg(test)]
mod bridge_tests {
    use super::*;

    #[tokio::test]
    async fn bridge_only_session_roundtrip_and_queued_input() {
        let bridge = WebInputBridge::new();

        // A stage asks, then blocks waiting for an answer.
        let waiter_bridge = bridge.clone();
        let waiter = tokio::spawn(async move {
            waiter_bridge
                .send("What is your favorite color?")
                .await
                .unwrap();
            waiter_bridge.receive().await.unwrap()
        });

        // Question is visible to the webui while waiting.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(bridge.transcript(), vec!["What is your favorite color?"]);

        // Answers queued before the receive even starts must not be lost.
        let seq1 = bridge.send_user_input("teal");
        let seq2 = bridge.send_user_input("teal again");
        assert_ne!(seq1, seq2);
        assert_eq!(bridge.queued().len(), 2);
        assert_eq!(waiter.await.unwrap(), "teal");

        // The second wait consumes the queued message without a new send.
        let again = tokio::spawn({
            let bridge = bridge.clone();
            async move { bridge.receive().await.unwrap() }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(again.await.unwrap(), "teal again");
    }

    #[tokio::test]
    async fn queued_input_can_be_cancelled_before_consumption() {
        let bridge = WebInputBridge::new();
        let seq = bridge.send_user_input("take this back");
        assert!(
            bridge
                .queued()
                .iter()
                .any(|(s, t)| *s == seq && t == "take this back")
        );

        assert!(bridge.cancel_input(seq));
        assert!(!bridge.cancel_input(seq)); // already gone
        assert!(bridge.queued().is_empty());

        // A waiter after cancellation keeps waiting — nothing stale delivered.
        let waiter = tokio::spawn({
            let bridge = bridge.clone();
            async move { bridge.receive().await.unwrap() }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());
        bridge.send_user_input("real answer");
        assert_eq!(waiter.await.unwrap(), "real answer");
    }

    #[tokio::test]
    async fn hybrid_session_prefers_first_of_bridge_or_provider() {
        struct SlowProvider;
        impl ChatSession for SlowProvider {
            fn send(&self, _m: &str) -> Pin<Box<dyn Future<Output = ChatResult<()>> + Send + '_>> {
                Box::pin(async { Ok(()) })
            }
            fn receive(&self) -> Pin<Box<dyn Future<Output = ChatResult<String>> + Send + '_>> {
                Box::pin(async {
                    // Simulates a provider taking a moment — the bridge wins if
                    // input arrives while the provider is still thinking.
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    Ok("from-provider".into())
                })
            }
        }

        let bridge = WebInputBridge::new();
        let hybrid = HybridSession::new(Arc::new(SlowProvider), bridge.clone());

        // Bridge input arrives before the provider answers — it wins.
        tokio::spawn({
            let bridge = bridge.clone();
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                bridge.send_user_input("from-bridge");
            }
        });
        let got = hybrid.receive().await.unwrap();
        assert_eq!(got, "from-bridge");

        // No bridge input pending — provider answers.
        let got = hybrid.receive().await.unwrap();
        assert_eq!(got, "from-provider");

        // Hybrid send records to the bridge transcript for webui display.
        hybrid.send("hello stage").await.unwrap();
        assert_eq!(bridge.transcript(), vec!["hello stage"]);
    }
}
