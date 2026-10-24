use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tj_chat::{ChatMessage, ChatProvider, ConversationContext, ConversationId, ConversationType};
use tj_db::Database;

pub struct RunNotifier {
    chat: Arc<dyn ChatProvider>,
    db: Arc<Database>,
    default_stream: String,
    conversation_cache: Mutex<HashMap<String, ConversationId>>,
}

impl RunNotifier {
    pub fn new(chat: Arc<dyn ChatProvider>, db: Arc<Database>, default_stream: String) -> Self {
        tracing::debug!(provider = %chat.provider_id(), default_stream = %default_stream, "RunNotifier created");
        Self {
            chat,
            db,
            default_stream,
            conversation_cache: Mutex::new(HashMap::new()),
        }
    }

    fn resolve_stream(&self, run_id: &str) -> String {
        self.db
            .load_run(run_id)
            .ok()
            .flatten()
            .and_then(|r| r.project_id().map(|pid| pid.as_str().to_string()))
            .and_then(|pid| self.db.load_project(&pid).ok().flatten())
            .map(|p| p.name.to_lowercase().replace(' ', "-"))
            .unwrap_or_else(|| self.default_stream.clone())
    }

    async fn ensure_topic(&self, run_id: &str, stream: &str) -> Option<ConversationId> {
        if let Some(id) = self.conversation_cache.lock().unwrap().get(run_id).cloned() {
            tracing::debug!(run_id = %run_id, "RunNotifier using cached conversation");
            return Some(id);
        }

        let ctx = ConversationContext {
            project_id: String::new(),
            project_slug: stream.to_string(),
            lane_id: String::new(),
            lane_slug: "run".to_string(),
            run_id: run_id.to_string(),
            flow_name: None,
            flow_number: None,
            run_description: None,
            stage_name: None,
            stage_execution_id: None,
        };

        tracing::debug!(
            run_id = %run_id,
            stream = %stream,
            "RunNotifier ensuring Zulip stream/topic exists via create_conversation"
        );

        match self
            .chat
            .create_conversation(&ctx, ConversationType::Run)
            .await
        {
            Ok(conv_id) => {
                tracing::debug!(
                    run_id = %run_id,
                    conversation_id = %conv_id.id,
                    provider_key = %conv_id.provider_key,
                    "RunNotifier created conversation successfully"
                );
                self.conversation_cache
                    .lock()
                    .unwrap()
                    .insert(run_id.to_string(), conv_id.clone());
                Some(conv_id)
            }
            Err(e) => {
                tracing::warn!(
                    run_id = %run_id,
                    stream = %stream,
                    error = %e,
                    "RunNotifier failed to create conversation for run topic"
                );
                None
            }
        }
    }

    async fn send(&self, run_id: &str, message: &str) {
        let stream = self.resolve_stream(run_id);

        let conv = match self.ensure_topic(run_id, &stream).await {
            Some(id) => id,
            None => {
                tracing::warn!(
                    run_id = %run_id,
                    stream = %stream,
                    content_len = message.len(),
                    "RunNotifier dropping message — no conversation available"
                );
                return;
            }
        };

        tracing::debug!(
            run_id = %run_id,
            stream = %stream,
            provider_key = %conv.provider_key,
            content_len = message.len(),
            "RunNotifier sending message to run topic"
        );

        let chat_msg = ChatMessage {
            sender: "TurboJet".into(),
            sender_id: String::new(),
            content: message.to_string(),
            timestamp: chrono::Utc::now(),
        };

        match self.chat.send_message(&conv, &chat_msg).await {
            Ok(()) => {
                tracing::debug!(run_id = %run_id, stream = %stream, "RunNotifier message sent successfully");
            }
            Err(e) => {
                tracing::warn!(run_id = %run_id, stream = %stream, error = %e, "Zulip notification failed");
            }
        }
    }

    pub async fn notify_task_started(&self, run_id: &str, title: &str, branch_name: &str) {
        tracing::debug!(run_id = %run_id, title = %title, branch_name = %branch_name, "notify_task_started");
        self.send(
            run_id,
            &format!("Task started: {}. Branch: {}", title, branch_name),
        )
        .await;
    }

    pub async fn notify_workspace_ready(&self, run_id: &str, flow_name: &str) {
        tracing::debug!(run_id = %run_id, flow_name = %flow_name, "notify_workspace_ready");
        self.send(
            run_id,
            &format!("Workspace ready. Starting {}...", flow_name),
        )
        .await;
    }

    pub async fn notify_flow_completed(&self, run_id: &str, flow_name: &str, summary: &str) {
        tracing::debug!(run_id = %run_id, flow_name = %flow_name, "notify_flow_completed");
        self.send(
            run_id,
            &format!("{} complete. {}. Pushed to branch.", flow_name, summary),
        )
        .await;
    }

    pub async fn notify_draft_pr(&self, run_id: &str, pr_url: &str) {
        tracing::debug!(run_id = %run_id, pr_url = %pr_url, "notify_draft_pr");
        self.send(run_id, &format!("Draft PR opened: {}", pr_url))
            .await;
    }

    pub async fn notify_task_complete(&self, run_id: &str, pr_url: &str) {
        tracing::debug!(run_id = %run_id, pr_url = %pr_url, "notify_task_complete");
        self.send(
            run_id,
            &format!("Task complete. PR ready for review: {}", pr_url),
        )
        .await;
    }

    pub async fn notify_error(&self, run_id: &str, step: &str, error: &str) {
        tracing::debug!(run_id = %run_id, step = %step, "notify_error");
        self.send(
            run_id,
            &format!(
                "Task failed during {}: {}. Remote branch preserved. Workspace cleaned up.",
                step, error
            ),
        )
        .await;
    }

    pub async fn notify_cleanup(&self, run_id: &str) {
        tracing::debug!(run_id = %run_id, "notify_cleanup");
        self.send(run_id, "Workspace cleaned up.").await;
    }

    pub async fn notify_warning(&self, run_id: &str, message: &str) {
        tracing::debug!(run_id = %run_id, message = %message, "notify_warning");
        self.send(run_id, &format!("Warning: {}", message)).await;
    }

    pub async fn notify_gate_reached(&self, run_id: &str, gate_name: &str, summary: &str) {
        tracing::debug!(run_id = %run_id, gate_name = %gate_name, "notify_gate_reached");
        self.send(
            run_id,
            &format!(
                "**Gate reached: {}**\n\n{}\n\nReply `approve` to continue or `reject` to cancel.",
                gate_name, summary
            ),
        )
        .await;
    }
}
