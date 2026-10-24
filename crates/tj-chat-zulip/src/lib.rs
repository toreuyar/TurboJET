use async_trait::async_trait;
use chrono::DateTime;
use futures::StreamExt;
use futures::stream::Stream;
use reqwest::{Client, Method, RequestBuilder};
use serde::{Deserialize, Serialize};
use std::error::Error as StdError;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tj_chat::{
    ChatError, ChatMessage, ChatProvider, ChatResult, ChatSession, ConversationContext,
    ConversationId, ConversationType,
};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZulipConfig {
    pub server: String,
    pub bot_email: String,
    pub api_key: String,
    pub default_stream: String,
}

pub struct ZulipChat {
    config: ZulipConfig,
    client: Client,
}

#[derive(Debug, Deserialize)]
struct ZulipResponse {
    result: String,
    #[serde(default)]
    msg: String,
}

#[derive(Debug, Deserialize)]
struct ZulipMessagesResponse {
    result: String,
    #[serde(default)]
    messages: Vec<ZulipMessageData>,
    #[serde(default)]
    msg: String,
}

#[derive(Debug, Deserialize)]
struct ZulipStreamIdResponse {
    result: String,
    #[serde(default)]
    stream_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ZulipMessageData {
    sender_full_name: String,
    sender_email: String,
    content: String,
    timestamp: u64,
}

impl ZulipChat {
    pub fn new(config: ZulipConfig) -> Self {
        let client = Client::builder()
            .build()
            .expect("failed to build HTTP client");

        Self { config, client }
    }

    fn api_url(&self, path: &str) -> String {
        format!(
            "{}/api/v1/{}",
            self.config.server.trim_end_matches('/'),
            path
        )
    }

    /// Clone the inner state to produce a new `ZulipChat` handle.
    pub fn clone_inner(&self) -> Self {
        Self {
            config: self.config.clone(),
            client: self.client.clone(),
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

impl ZulipChat {
    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, self.api_url(path))
            .basic_auth(&self.config.bot_email, Some(&self.config.api_key))
    }

    fn stream_name(ctx: &ConversationContext) -> String {
        ctx.project_slug.clone()
    }

    fn topic_name(ctx: &ConversationContext, kind: ConversationType) -> String {
        // Zulip's default `max_topic_length` is 60 chars. If we send a longer topic,
        // Zulip silently truncates the stored topic but our `register` narrow is exact —
        // we end up listening on a topic that no message actually has, so user replies
        // never reach us. Keep all topics short and stable.
        const MAX_TOPIC: usize = 55;
        let raw = match kind {
            ConversationType::Flow => {
                let flow_name = ctx.flow_name.as_deref().unwrap_or("unknown");
                let number = ctx.flow_number.unwrap_or(0);
                format!("{} {}", capitalize(flow_name), number)
            }
            ConversationType::Run => ctx.run_id.clone(),
            ConversationType::Stage => {
                format!(
                    "{} #{} / {}",
                    ctx.lane_slug,
                    ctx.run_id,
                    ctx.stage_name.as_deref().unwrap_or("unknown")
                )
            }
            ConversationType::General => ctx.project_slug.clone(),
            ConversationType::DirectMessage => String::new(),
        };
        if raw.chars().count() > MAX_TOPIC {
            raw.chars().take(MAX_TOPIC).collect()
        } else {
            raw
        }
    }

    fn provider_key(ctx: &ConversationContext, kind: ConversationType) -> String {
        if kind == ConversationType::DirectMessage {
            return "dm::".to_string();
        }
        format!(
            "{}::{}",
            Self::stream_name(ctx),
            Self::topic_name(ctx, kind)
        )
    }

    async fn ensure_stream_exists(&self, stream_name: &str) -> ChatResult<()> {
        info!(stream = stream_name, "ZULIP HTTP --> GET /get_stream_id");
        let t0 = std::time::Instant::now();

        let check = self
            .request(Method::GET, "get_stream_id")
            .query(&[("stream", stream_name)])
            .send()
            .await
            .map_err(|e| {
                error!(stream = stream_name, elapsed_ms = %t0.elapsed().as_millis(), error = %e, "ZULIP HTTP !! GET /get_stream_id network error");
                ChatError::NetworkError(e.to_string())
            })?;

        let status = check.status();
        let resp: ZulipStreamIdResponse = check
            .json()
            .await
            .map_err(|e| {
                error!(stream = stream_name, status = %status, error = %e, "ZULIP HTTP <-- GET /get_stream_id body parse error");
                ChatError::NetworkError(e.to_string())
            })?;

        info!(stream = stream_name, status = %status, elapsed_ms = %t0.elapsed().as_millis(), result = %resp.result, "ZULIP HTTP <-- GET /get_stream_id");

        if resp.result == "success" && resp.stream_id.is_some() {
            info!(stream = stream_name, stream_id = ?resp.stream_id, "stream already exists");
            return Ok(());
        }

        info!(
            stream = stream_name,
            "ZULIP HTTP --> POST /channels/create (stream not found)"
        );
        let t1 = std::time::Instant::now();

        let create = self
            .request(Method::POST, "channels/create")
            .form(&[
                ("name", stream_name),
                ("description", "TurboJet project channel"),
                ("subscribers", "[]"),
            ])
            .send()
            .await
            .map_err(|e| ChatError::NetworkError(e.to_string()))?;

        let status = create.status();
        let resp: ZulipResponse = create
            .json()
            .await
            .map_err(|e| ChatError::NetworkError(e.to_string()))?;

        info!(stream = stream_name, status = %status, elapsed_ms = %t1.elapsed().as_millis(), result = %resp.result, "ZULIP HTTP <-- POST /channels/create");

        if resp.result == "success" {
            info!(stream = stream_name, "created Zulip stream");
            self.subscribe_bot_to_stream(stream_name).await?;
            Ok(())
        } else {
            warn!(stream = stream_name, msg = %resp.msg, "channels/create failed, falling back to subscribe");
            self.subscribe_bot_to_stream(stream_name).await
        }
    }

    async fn subscribe_bot_to_stream(&self, stream_name: &str) -> ChatResult<()> {
        debug!(stream = stream_name, "subscribe_bot_to_stream entered");
        info!(
            stream = stream_name,
            "ZULIP HTTP --> POST /users/me/subscriptions"
        );
        let t0 = std::time::Instant::now();

        let resp = self
            .request(Method::POST, "users/me/subscriptions")
            .form(&[(
                "subscriptions",
                format!("[{{\"name\": \"{}\"}}]", stream_name),
            )])
            .send()
            .await
            .map_err(|e| ChatError::NetworkError(e.to_string()))?;

        let status = resp.status();
        let result: ZulipResponse = resp
            .json()
            .await
            .map_err(|e| ChatError::NetworkError(e.to_string()))?;

        info!(stream = stream_name, status = %status, elapsed_ms = %t0.elapsed().as_millis(), result = %result.result, "ZULIP HTTP <-- POST /users/me/subscriptions");

        if result.result == "success" {
            info!(stream = stream_name, "bot subscribed to stream");
            Ok(())
        } else {
            warn!(stream = stream_name, msg = %result.msg, "subscribe failed");
            Err(ChatError::ProviderError(format!(
                "subscribe failed: {}",
                result.msg
            )))
        }
    }

    fn message_to_chat(msg: &ZulipMessageData) -> ChatMessage {
        let timestamp = DateTime::from_timestamp(msg.timestamp as i64, 0).unwrap_or_default();
        ChatMessage {
            sender: msg.sender_full_name.clone(),
            sender_id: msg.sender_email.clone(),
            content: msg.content.clone(),
            timestamp,
        }
    }
}

// ---------------------------------------------------------------------------
// Zulip ChatSession implementations
// ---------------------------------------------------------------------------

/// A flow session backed by a Zulip stream/topic conversation.
pub struct ZulipFlowSession {
    zulip: ZulipChat,
    conv_id: ConversationId,
    rx: Mutex<mpsc::Receiver<String>>,
}

impl ZulipFlowSession {
    pub fn new(zulip: ZulipChat, conv_id: ConversationId) -> Self {
        let (tx, rx) = mpsc::channel::<String>(32);

        // Spawn a background listener that feeds inbound messages into the channel.
        let listener_conv = conv_id.clone();
        let listener_zulip = zulip.clone_inner();
        tokio::spawn(async move {
            loop {
                let mut stream = listener_zulip.listen(&listener_conv);
                while let Some(result) = StreamExt::next(&mut stream).await {
                    match result {
                        Ok(msg) => {
                            if tx.send(msg.content).await.is_err() {
                                // Receiver dropped; session is gone.
                                return;
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "Flow session listener error");
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }
                warn!("Flow session listener stream ended, restarting in 10s");
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        });

        Self {
            zulip,
            conv_id,
            rx: Mutex::new(rx),
        }
    }
}

impl ChatSession for ZulipFlowSession {
    fn send(&self, message: &str) -> Pin<Box<dyn Future<Output = ChatResult<()>> + Send + '_>> {
        let preview: String = message.chars().take(100).collect();
        debug!(
            provider_key = %self.conv_id.provider_key,
            content_len = message.len(),
            preview = %preview,
            "ZulipFlowSession::send entered"
        );

        let zulip = self.zulip.clone_inner();
        let conv_id = self.conv_id.clone();
        let msg = message.to_string();

        Box::pin(async move {
            let chat_msg = ChatMessage {
                sender: "TurboJet".into(),
                sender_id: String::new(),
                content: msg,
                timestamp: chrono::Utc::now(),
            };
            let result = zulip.send_message(&conv_id, &chat_msg).await;

            match &result {
                Ok(()) => {
                    debug!(provider_key = %conv_id.provider_key, "ZulipFlowSession::send succeeded");
                }
                Err(e) => {
                    error!(provider_key = %conv_id.provider_key, error = %e, "ZulipFlowSession::send failed");
                    let mut source = StdError::source(&e);
                    let mut depth = 0u32;
                    while let Some(cause) = source {
                        error!(depth, cause = %cause, "Error chain");
                        source = StdError::source(cause);
                        depth += 1;
                    }
                }
            }

            result
        })
    }

    fn receive(&self) -> Pin<Box<dyn Future<Output = ChatResult<String>> + Send + '_>> {
        Box::pin(async {
            let mut rx = self.rx.lock().await;
            rx.recv()
                .await
                .ok_or_else(|| ChatError::Other("receive channel closed".into()))
        })
    }
}

/// A DM session backed by Zulip private messages.
/// Internal message carrying both content and sender email.
struct DmInbound {
    content: String,
    sender_email: String,
}

pub struct ZulipDmSession {
    zulip: ZulipChat,
    user_email: String,
    rx: Mutex<mpsc::Receiver<DmInbound>>,
    /// Tracks the last sender's email for replying in no-filter (chief) mode.
    last_sender: Arc<Mutex<String>>,
}

impl ZulipDmSession {
    pub fn new(zulip: ZulipChat, user_email: String) -> Self {
        let (tx, rx) = mpsc::channel::<DmInbound>(32);

        let is_no_filter = user_email.is_empty() || user_email == "dm::";

        // Spawn a background listener for DMs from this user (or all users in no-filter mode).
        let listener_zulip = zulip.clone_inner();
        let email_for_filter = user_email.clone();
        let dm_conv = ConversationId {
            id: format!("dm-listen-{}", uuid::Uuid::new_v4()),
            provider_key: "dm::".to_string(),
        };
        tokio::spawn(async move {
            debug!(
                email_filter = %email_for_filter,
                is_no_filter = is_no_filter,
                "DM session background listener starting"
            );
            loop {
                let mut stream = listener_zulip.listen(&dm_conv);
                while let Some(result) = StreamExt::next(&mut stream).await {
                    match result {
                        Ok(msg) => {
                            let matches = is_no_filter || msg.sender_id == email_for_filter;
                            if matches {
                                debug!(
                                    sender = %msg.sender_id,
                                    is_no_filter = is_no_filter,
                                    "DM session forwarding inbound message"
                                );
                                if tx
                                    .send(DmInbound {
                                        content: msg.content,
                                        sender_email: msg.sender_id,
                                    })
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "DM session listener error");
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }
                warn!("DM session listener stream ended, restarting in 10s");
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        });

        Self {
            zulip,
            user_email,
            rx: Mutex::new(rx),
            last_sender: Arc::new(Mutex::new(String::new())),
        }
    }
}

impl ChatSession for ZulipDmSession {
    fn send(&self, message: &str) -> Pin<Box<dyn Future<Output = ChatResult<()>> + Send + '_>> {
        let zulip = self.zulip.clone_inner();
        let user_email = self.user_email.clone();
        let msg = message.to_string();
        let last_sender = self.last_sender.clone();

        Box::pin(async move {
            // In no-filter (chief) mode, reply to whoever last messaged us.
            let recipient = if user_email.is_empty() || user_email == "dm::" {
                let sender = last_sender.lock().await;
                if sender.is_empty() {
                    return Err(ChatError::Other(
                        "Cannot reply: no sender address known yet".into(),
                    ));
                }
                sender.clone()
            } else {
                user_email.clone()
            };

            debug!(recipient = %recipient, "ZulipDmSession::send replying to");
            let conv_id = ConversationId {
                id: String::new(),
                provider_key: "dm::".to_string(),
            };
            let chat_msg = ChatMessage {
                sender: "TurboJet".into(),
                sender_id: recipient,
                content: msg,
                timestamp: chrono::Utc::now(),
            };
            zulip.send_message(&conv_id, &chat_msg).await
        })
    }

    fn receive(&self) -> Pin<Box<dyn Future<Output = ChatResult<String>> + Send + '_>> {
        Box::pin(async {
            let mut rx = self.rx.lock().await;
            let inbound = rx
                .recv()
                .await
                .ok_or_else(|| ChatError::Other("receive channel closed".into()))?;

            // Track the sender for reply routing in no-filter mode.
            let is_no_filter = self.user_email.is_empty() || self.user_email == "dm::";
            if is_no_filter {
                let mut last = self.last_sender.lock().await;
                debug!(sender = %inbound.sender_email, "DM session recording last sender");
                *last = inbound.sender_email;
            }

            Ok(inbound.content)
        })
    }
}

#[async_trait]
impl ChatProvider for ZulipChat {
    fn provider_id(&self) -> &str {
        "zulip"
    }

    async fn create_conversation(
        &self,
        ctx: &ConversationContext,
        kind: ConversationType,
    ) -> ChatResult<ConversationId> {
        let provider_key = Self::provider_key(ctx, kind);

        debug!(
            provider_key = %provider_key,
            kind = ?kind,
            project_slug = %ctx.project_slug,
            lane_slug = %ctx.lane_slug,
            run_id = %ctx.run_id,
            flow_name = ?ctx.flow_name,
            flow_number = ?ctx.flow_number,
            "ZulipChat::create_conversation entered"
        );

        if kind != ConversationType::DirectMessage {
            let stream_name = Self::stream_name(ctx);
            debug!(stream = %stream_name, "Ensuring stream exists");
            self.ensure_stream_exists(&stream_name).await?;
            debug!(stream = %stream_name, "Subscribing bot to stream");
            self.subscribe_bot_to_stream(&stream_name).await?;
            debug!(stream = %stream_name, "Stream setup complete");
        }

        let id = format!("zulip-{}", uuid::Uuid::new_v4());

        info!(
            provider_key = %provider_key,
            kind = ?kind,
            conversation_id = %id,
            "created conversation"
        );

        Ok(ConversationId { id, provider_key })
    }

    async fn send_message(&self, conv: &ConversationId, message: &ChatMessage) -> ChatResult<()> {
        if conv.provider_key.starts_with("dm::") {
            let recipient = if message.sender_id.is_empty() {
                return Err(ChatError::Other(
                    "DM reply requires sender_id (email)".into(),
                ));
            } else {
                &message.sender_id
            };

            info!(to = %recipient, bytes = message.content.len(), "ZULIP HTTP --> POST /messages (private)");
            let t0 = std::time::Instant::now();

            let resp = self
                .request(Method::POST, "messages")
                .form(&[
                    ("type", "private"),
                    ("to", recipient),
                    ("content", &message.content),
                ])
                .send()
                .await
                .map_err(|e| {
                    error!(to = %recipient, elapsed_ms = %t0.elapsed().as_millis(), error = %e, "ZULIP HTTP !! POST /messages (private) network error");
                    ChatError::NetworkError(e.to_string())
                })?;

            let status = resp.status();
            let result: ZulipResponse = resp
                .json()
                .await
                .map_err(|e| ChatError::NetworkError(e.to_string()))?;

            info!(to = %recipient, status = %status, elapsed_ms = %t0.elapsed().as_millis(), result = %result.result, "ZULIP HTTP <-- POST /messages (private)");

            if result.result == "success" {
                debug!(to = %recipient, "DM message sent successfully");
                Ok(())
            } else {
                warn!(to = %recipient, msg = %result.msg, "DM send failed");
                Err(ChatError::ProviderError(format!(
                    "DM send failed: {}",
                    result.msg
                )))
            }
        } else {
            let parts: Vec<&str> = conv.provider_key.splitn(2, "::").collect();
            let (stream, topic) = match parts.as_slice() {
                [s, t] => (*s, *t),
                _ => return Err(ChatError::Other("invalid provider_key format".into())),
            };

            info!(
                stream = stream,
                topic = topic,
                bytes = message.content.len(),
                "ZULIP HTTP --> POST /messages (stream)"
            );
            let t0 = std::time::Instant::now();

            let resp = self
                .request(Method::POST, "messages")
                .form(&[
                    ("type", "stream"),
                    ("to", stream),
                    ("topic", topic),
                    ("content", &message.content),
                ])
                .send()
                .await
                .map_err(|e| {
                    error!(stream = stream, topic = topic, elapsed_ms = %t0.elapsed().as_millis(), error = %e, "ZULIP HTTP !! POST /messages (stream) network error");
                    ChatError::NetworkError(e.to_string())
                })?;

            let status = resp.status();
            let result: ZulipResponse = resp
                .json()
                .await
                .map_err(|e| ChatError::NetworkError(e.to_string()))?;

            info!(stream = stream, topic = topic, status = %status, elapsed_ms = %t0.elapsed().as_millis(), result = %result.result, "ZULIP HTTP <-- POST /messages (stream)");

            if result.result == "success" {
                debug!(
                    stream = stream,
                    topic = topic,
                    "Stream message sent successfully"
                );
                Ok(())
            } else {
                warn!(stream = stream, topic = topic, msg = %result.msg, "message send failed");
                Err(ChatError::ProviderError(format!(
                    "send failed: {}",
                    result.msg
                )))
            }
        }
    }

    fn listen(
        &self,
        conv: &ConversationId,
    ) -> Pin<Box<dyn Stream<Item = ChatResult<ChatMessage>> + Send>> {
        let is_dm = conv.provider_key == "dm::";

        let (stream_name, topic_name) = if is_dm {
            (String::new(), String::new())
        } else {
            let parts: Vec<String> = conv
                .provider_key
                .splitn(2, "::")
                .map(String::from)
                .collect();

            match parts.as_slice() {
                [s, t] => (s.clone(), t.clone()),
                _ => {
                    return Box::pin(futures::stream::once(async {
                        Err(ChatError::Other("invalid provider_key".into()))
                    }));
                }
            }
        };

        let client = self.client.clone();
        let api_url = self.api_url("events");
        let bot_email = self.config.bot_email.clone();
        let api_key = self.config.api_key.clone();

        let listener_id = uuid::Uuid::new_v4().to_string();
        let listener_tag = listener_id.chars().take(8).collect::<String>();

        Box::pin(async_stream::stream! {
            let build_narrow = || -> String {
                let narrow = if is_dm {
                    vec![vec!["is".to_string(), "private".to_string()]]
                } else if topic_name.is_empty() || topic_name == stream_name {
                    vec![vec!["channel".to_string(), stream_name.clone()]]
                } else {
                    vec![
                        vec!["channel".to_string(), stream_name.clone()],
                        vec!["topic".to_string(), topic_name.clone()],
                    ]
                };
                serde_json::to_string(&narrow).unwrap_or_default()
            };

            let register_queue = |narrow_json: String| {
                let client = client.clone();
                let api_url = api_url.clone();
                let bot_email = bot_email.clone();
                let api_key = api_key.clone();
                let listener_tag = listener_tag.clone();
                async move {
                    let register_url = api_url.replace("events", "register");
                    info!(
                        listener = %listener_tag,
                        url = %register_url,
                        narrow = %narrow_json,
                        "ZULIP HTTP --> POST /register (event queue)"
                    );
                    let t0 = std::time::Instant::now();
                    let resp = client
                        .post(&register_url)
                        .basic_auth(&bot_email, Some(&api_key))
                        .form(&[
                            ("event_types", "[\"message\"]"),
                            ("narrow", narrow_json.as_str()),
                            ("idle_queue_timeout", "3600"),
                        ])
                        .timeout(std::time::Duration::from_secs(30))
                        .send()
                        .await;
                    match resp {
                        Ok(r) => {
                            let status = r.status();
                            let val: serde_json::Value = r.json().await.unwrap_or_default();
                            let qid = val["queue_id"].as_str().unwrap_or_default().to_string();
                            let eid = val["last_event_id"].as_i64().unwrap_or(-1);
                            info!(
                                listener = %listener_tag,
                                status = %status,
                                elapsed_ms = %t0.elapsed().as_millis(),
                                queue_id = %qid,
                                last_event_id = %eid,
                                "ZULIP HTTP <-- POST /register"
                            );
                            if qid.is_empty() {
                                error!(listener = %listener_tag, response = ?val, "register returned no queue_id");
                                return Err(ChatError::ProviderError(format!(
                                    "no queue_id in register response: {}",
                                    val
                                )));
                            }
                            Ok((qid, eid))
                        }
                        Err(e) => {
                            error!(
                                listener = %listener_tag,
                                elapsed_ms = %t0.elapsed().as_millis(),
                                error = %e,
                                "ZULIP HTTP !! POST /register failed"
                            );
                            Err(ChatError::NetworkError(e.to_string()))
                        }
                    }
                }
            };

            let mut queue_id: String;
            let mut last_event_id: i64;

            match register_queue(build_narrow()).await {
                Ok((qid, eid)) => {
                    queue_id = qid;
                    last_event_id = eid;
                    info!(
                        listener = %listener_tag,
                        queue_id = %queue_id,
                        last_event_id = %last_event_id,
                        is_dm = is_dm,
                        stream = %stream_name,
                        topic = %topic_name,
                        "Zulip listener registered, entering long-poll loop"
                    );
                }
                Err(e) => {
                    yield Err(e);
                    return;
                }
            }

            let events_url = api_url.clone();
            let mut poll_count: u64 = 0;
            let mut consecutive_fast_polls: u32 = 0;
            let mut last_poll_start = std::time::Instant::now();

            loop {
                poll_count += 1;
                let poll_start = std::time::Instant::now();
                let inter_poll_ms = poll_start.duration_since(last_poll_start).as_millis();
                last_poll_start = poll_start;

                // Spam-detection guard: if many polls in a row complete in <1s, something is wrong
                // (typically last_event_id not advancing). Back off so we don't hammer Zulip.
                if inter_poll_ms < 1000 {
                    consecutive_fast_polls = consecutive_fast_polls.saturating_add(1);
                } else {
                    consecutive_fast_polls = 0;
                }
                if consecutive_fast_polls >= 5 {
                    warn!(
                        listener = %listener_tag,
                        poll = poll_count,
                        consecutive_fast_polls = consecutive_fast_polls,
                        queue_id = %queue_id,
                        last_event_id = %last_event_id,
                        "SPAM GUARD: many fast polls in a row — sleeping 30s to avoid hammering Zulip"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    consecutive_fast_polls = 0;
                }

                info!(
                    listener = %listener_tag,
                    poll = poll_count,
                    queue_id = %queue_id,
                    last_event_id = %last_event_id,
                    inter_poll_ms = inter_poll_ms,
                    "ZULIP HTTP --> GET /events (long-poll)"
                );

                let resp = client
                    .get(&events_url)
                    .query(&[
                        ("queue_id", queue_id.as_str()),
                        ("last_event_id", &last_event_id.to_string()),
                    ])
                    .basic_auth(&bot_email, Some(&api_key))
                    .timeout(std::time::Duration::from_secs(300))
                    .send()
                    .await;

                let elapsed_ms = poll_start.elapsed().as_millis();

                match resp {
                    Ok(r) => {
                        let status = r.status();
                        let body: serde_json::Value = match r.json().await {
                            Ok(v) => v,
                            Err(e) => {
                                error!(
                                    listener = %listener_tag,
                                    poll = poll_count,
                                    status = %status,
                                    elapsed_ms = elapsed_ms,
                                    error = %e,
                                    "ZULIP HTTP <-- GET /events: body parse failed, sleeping 5s"
                                );
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                continue;
                            }
                        };

                        if body["result"].as_str() == Some("error") {
                            let code = body["code"].as_str().unwrap_or("unknown").to_string();
                            let msg = body["msg"].as_str().unwrap_or("unknown error").to_string();
                            warn!(
                                listener = %listener_tag,
                                poll = poll_count,
                                status = %status,
                                elapsed_ms = elapsed_ms,
                                code = %code,
                                msg = %msg,
                                "ZULIP HTTP <-- GET /events: API error"
                            );

                            if code == "BAD_EVENT_QUEUE_ID" {
                                warn!(
                                    listener = %listener_tag,
                                    old_queue_id = %queue_id,
                                    "event queue expired, re-registering in 5s"
                                );
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                                match register_queue(build_narrow()).await {
                                    Ok((qid, eid)) => {
                                        info!(
                                            listener = %listener_tag,
                                            old_queue_id = %queue_id,
                                            new_queue_id = %qid,
                                            new_last_event_id = %eid,
                                            "re-registered event queue, resuming long-poll"
                                        );
                                        queue_id = qid;
                                        last_event_id = eid;
                                    }
                                    Err(e) => {
                                        error!(
                                            listener = %listener_tag,
                                            error = %e,
                                            "re-register failed, yielding error and ending listener (caller should restart)"
                                        );
                                        yield Err(e);
                                        return;
                                    }
                                }
                                continue;
                            }

                            let is_rate_limit = msg.contains("rate limit") || code == "RATE_LIMIT_HIT";
                            if is_rate_limit {
                                let retry_after = body["retry-after"].as_f64()
                                    .or_else(|| body["retry_after"].as_f64())
                                    .unwrap_or(60.0);
                                let sleep_ms = ((retry_after.max(1.0)) * 1000.0) as u64;
                                warn!(
                                    listener = %listener_tag,
                                    retry_after_s = retry_after,
                                    "Zulip rate limited — backing off"
                                );
                                tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
                            } else {
                                error!(
                                    listener = %listener_tag,
                                    error = %msg,
                                    "Zulip events API error, sleeping 5s"
                                );
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            }
                            continue;
                        }

                        let events = body["events"].as_array().cloned().unwrap_or_default();

                        // ALWAYS advance last_event_id to the maximum id we received, regardless
                        // of whether we actually yielded any messages. Failing to do so causes
                        // Zulip to immediately re-deliver the same events on the next poll,
                        // producing a tight loop that hammers the server.
                        let prev_last_event_id = last_event_id;
                        let mut max_seen: i64 = last_event_id;
                        for event in &events {
                            if let Some(id) = event["id"].as_i64() {
                                if id > max_seen {
                                    max_seen = id;
                                }
                            } else if let Some(id) = event["id"].as_u64() {
                                let id = id as i64;
                                if id > max_seen {
                                    max_seen = id;
                                }
                            }
                        }
                        last_event_id = max_seen;

                        let message_count = events.iter().filter(|e| e["type"].as_str() == Some("message")).count();
                        let heartbeat_count = events.iter().filter(|e| e["type"].as_str() == Some("heartbeat")).count();
                        let other_count = events.len() - message_count - heartbeat_count;

                        if events.is_empty() {
                            debug!(
                                listener = %listener_tag,
                                poll = poll_count,
                                status = %status,
                                elapsed_ms = elapsed_ms,
                                "ZULIP HTTP <-- GET /events: 0 events"
                            );
                        } else if message_count == 0 && other_count == 0 {
                            debug!(
                                listener = %listener_tag,
                                poll = poll_count,
                                status = %status,
                                elapsed_ms = elapsed_ms,
                                heartbeats = heartbeat_count,
                                last_event_id_after = last_event_id,
                                "ZULIP HTTP <-- GET /events: heartbeat only"
                            );
                        } else {
                            info!(
                                listener = %listener_tag,
                                poll = poll_count,
                                status = %status,
                                elapsed_ms = elapsed_ms,
                                messages = message_count,
                                heartbeats = heartbeat_count,
                                other = other_count,
                                last_event_id_before = prev_last_event_id,
                                last_event_id_after = last_event_id,
                                "ZULIP HTTP <-- GET /events: received events"
                            );
                        }

                        for event in &events {
                            let event_type = event["type"].as_str().unwrap_or("");
                            let event_id = event["id"].as_i64()
                                .or_else(|| event["id"].as_u64().map(|v| v as i64))
                                .unwrap_or(-1);

                            if event_type != "message" {
                                debug!(
                                    listener = %listener_tag,
                                    event_id = event_id,
                                    event_type = event_type,
                                    "skipping non-message event"
                                );
                                continue;
                            }

                            let msg = match event.get("message") {
                                Some(m) => m,
                                None => {
                                    debug!(
                                        listener = %listener_tag,
                                        event_id = event_id,
                                        "message event has no 'message' payload, skipping"
                                    );
                                    continue;
                                }
                            };

                            let sender_full_name = msg["sender_full_name"].as_str().unwrap_or("unknown");
                            let sender_email = msg["sender_email"].as_str().unwrap_or("");
                            let msg_stream = msg["display_recipient"].as_str().unwrap_or("");
                            let msg_topic = msg["subject"].as_str().unwrap_or("");
                            let msg_type = msg["type"].as_str().unwrap_or("");

                            info!(
                                listener = %listener_tag,
                                event_id = event_id,
                                sender = %sender_email,
                                msg_type = %msg_type,
                                msg_stream = %msg_stream,
                                msg_topic = %msg_topic,
                                "received message event (narrow context — verify these match the listener's narrow)"
                            );

                            if sender_email == bot_email.as_str() {
                                debug!(
                                    listener = %listener_tag,
                                    event_id = event_id,
                                    sender = %sender_email,
                                    "skipping bot's own message (last_event_id already advanced)"
                                );
                                continue;
                            }

                            let content = msg["content"].as_str().unwrap_or("");
                            let timestamp = msg["timestamp"].as_u64().unwrap_or(0);
                            let ts = DateTime::from_timestamp(timestamp as i64, 0).unwrap_or_default();

                            info!(
                                listener = %listener_tag,
                                event_id = event_id,
                                sender = %sender_email,
                                preview = %content.chars().take(80).collect::<String>(),
                                "yielding inbound message"
                            );

                            yield Ok(ChatMessage {
                                sender: sender_full_name.to_string(),
                                sender_id: sender_email.to_string(),
                                content: content.to_string(),
                                timestamp: ts,
                            });
                        }
                    }
                    Err(e) => {
                        if e.is_timeout() {
                            debug!(
                                listener = %listener_tag,
                                poll = poll_count,
                                elapsed_ms = elapsed_ms,
                                "ZULIP HTTP !! GET /events: client timeout (300s), re-polling"
                            );
                            continue;
                        }
                        error!(
                            listener = %listener_tag,
                            poll = poll_count,
                            elapsed_ms = elapsed_ms,
                            error = %e,
                            "ZULIP HTTP !! GET /events: connection error, sleeping 5s"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        })
    }

    async fn get_history(
        &self,
        conv: &ConversationId,
        limit: usize,
    ) -> ChatResult<Vec<ChatMessage>> {
        let parts: Vec<&str> = conv.provider_key.splitn(2, "::").collect();
        let (stream, topic) = match parts.as_slice() {
            [s, t] => (*s, *t),
            _ => return Err(ChatError::Other("invalid provider_key".into())),
        };

        let narrow = serde_json::json!([["channel", stream], ["topic", topic]]).to_string();

        info!(
            stream = stream,
            topic = topic,
            limit = limit,
            "ZULIP HTTP --> GET /messages (history)"
        );
        let t0 = std::time::Instant::now();

        let resp = self
            .request(Method::GET, "messages")
            .query(&[
                ("narrow", narrow.as_str()),
                ("limit", limit.to_string().as_str()),
                ("anchor", "newest"),
            ])
            .send()
            .await
            .map_err(|e| {
                error!(stream = stream, topic = topic, elapsed_ms = %t0.elapsed().as_millis(), error = %e, "ZULIP HTTP !! GET /messages (history) network error");
                ChatError::NetworkError(e.to_string())
            })?;

        let status = resp.status();
        let result: ZulipMessagesResponse = resp
            .json()
            .await
            .map_err(|e| ChatError::NetworkError(e.to_string()))?;

        info!(stream = stream, topic = topic, status = %status, elapsed_ms = %t0.elapsed().as_millis(), result = %result.result, count = result.messages.len(), "ZULIP HTTP <-- GET /messages (history)");

        if result.result == "success" {
            Ok(result
                .messages
                .iter()
                .filter(|m| m.sender_full_name != self.config.bot_email)
                .map(Self::message_to_chat)
                .collect())
        } else {
            warn!(stream = stream, topic = topic, msg = %result.msg, "history request failed");
            Err(ChatError::ProviderError(result.msg))
        }
    }

    async fn close_conversation(&self, _conv: &ConversationId) -> ChatResult<()> {
        Ok(())
    }

    fn create_dm_session(
        &self,
        user_identifier: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = ChatResult<Arc<dyn ChatSession>>> + Send + '_>>
    {
        let zulip = self.clone_inner();
        let user_email = user_identifier.to_string();
        Box::pin(async move {
            let session = ZulipDmSession::new(zulip, user_email);
            Ok(Arc::new(session) as Arc<dyn ChatSession>)
        })
    }

    fn create_flow_session(
        &self,
        ctx: &ConversationContext,
    ) -> Pin<Box<dyn std::future::Future<Output = ChatResult<Arc<dyn ChatSession>>> + Send + '_>>
    {
        let zulip = self.clone_inner();
        let ctx = ctx.clone();
        Box::pin(async move {
            debug!(
                ctx.project_id = %ctx.project_id,
                ctx.project_slug = %ctx.project_slug,
                ctx.lane_id = %ctx.lane_id,
                ctx.lane_slug = %ctx.lane_slug,
                ctx.run_id = %ctx.run_id,
                ctx.flow_name = ?ctx.flow_name,
                ctx.flow_number = ?ctx.flow_number,
                "ZulipChat::create_flow_session entered"
            );

            let stream_name = Self::stream_name(&ctx);
            let topic_name = Self::topic_name(&ctx, ConversationType::Run);
            debug!(stream = %stream_name, topic = %topic_name, "Computed Zulip stream/topic for flow session");

            debug!(stream = %stream_name, topic = %topic_name, "Calling create_conversation for Run type");
            let conv_id = match zulip.create_conversation(&ctx, ConversationType::Run).await {
                Ok(id) => {
                    debug!(conversation_id = %id.id, provider_key = %id.provider_key, "create_conversation succeeded");
                    id
                }
                Err(e) => {
                    error!(stream = %stream_name, topic = %topic_name, error = %e, "create_conversation failed for flow session");
                    return Err(e);
                }
            };

            debug!(
                conversation_id = %conv_id.id,
                provider_key = %conv_id.provider_key,
                stream = %stream_name,
                topic = %topic_name,
                "Creating ZulipFlowSession"
            );
            let session = ZulipFlowSession::new(zulip, conv_id);

            debug!(stream = %stream_name, topic = %topic_name, "ZulipFlowSession created successfully");
            Ok(Arc::new(session) as Arc<dyn ChatSession>)
        })
    }
}
