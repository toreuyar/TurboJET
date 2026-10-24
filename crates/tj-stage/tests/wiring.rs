//! T-5 wiring test: `Stage::new` attaches the blocking question channel to
//! the runner for interactive stages that have a chat session, and leaves it
//! absent otherwise (chat-less and non-interactive stages keep the T-3/T-4
//! not-wired/nudge behavior).

#[allow(dead_code)]
mod common;

use std::sync::Arc;

use tj_config::loader::ConfigLoader;
use tj_config::smg::StageDef;
use tj_stage::{Stage, StageConfig};

use common::{ScriptedSession, insert_run_and_flow, test_db};

/// Minimal config: one OpenAI provider whose API key is read from an
/// environment variable that always exists in the test environment (no
/// network calls happen — the client is constructed, never asked).
fn config_loader(dir: &std::path::Path) -> ConfigLoader {
    let config_dir = dir.join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("fixture.json"),
        r#"{"providers": [{"id": "test", "type": "open_ai", "api_key_env": "PATH", "default_model": "test-model"}]}"#,
    )
    .unwrap();
    let agent_dir = config_dir.join("stages").join("stage-a");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("agent.json"),
        r#"{"id": "stage-a", "name": "Stage A", "prompt": "prompt.md"}"#,
    )
    .unwrap();
    std::fs::write(agent_dir.join("prompt.md"), "Ask the user for the goal.").unwrap();
    ConfigLoader::from_file(&config_dir.join("fixture.json")).unwrap()
}

fn stage_def(interactive: bool) -> StageDef {
    serde_json::from_value(serde_json::json!({
        "agent": "config/stages/stage-a/agent.json",
        "interactive": interactive
    }))
    .unwrap()
}

async fn build_stage(
    dir: &std::path::Path,
    interactive: bool,
    session: Option<Arc<dyn tj_chat::ChatSession>>,
) -> Stage {
    let loader = config_loader(dir);
    let config = StageConfig::resolve(
        "run_w".to_string(),
        "job_w".to_string(),
        "flow_w".to_string(),
        "flow-a".to_string(),
        "stage-a".to_string(),
        None,
        0,
        stage_def(interactive),
        serde_json::json!({}),
        "test job".to_string(),
        None,
        interactive,
        &loader,
    )
    .unwrap();
    let db = test_db();
    insert_run_and_flow(&db, "run_w", "flow_w");
    Stage::new(config, db, session, &loader).await.unwrap()
}

#[tokio::test]
async fn interactive_stage_with_session_wires_question_channel() {
    let dir = std::env::temp_dir().join(format!("tjstage-wiring-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let session: Arc<dyn tj_chat::ChatSession> = ScriptedSession::new();
    let stage = build_stage(&dir, true, Some(session)).await;
    assert!(
        stage.question_channel().is_some(),
        "interactive stage must carry the blocking question channel"
    );
}

#[tokio::test]
async fn interactive_stage_without_session_has_no_channel() {
    let dir = std::env::temp_dir().join(format!("tjstage-wiring-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let stage = build_stage(&dir, true, None).await;
    assert!(
        stage.question_channel().is_none(),
        "chat-less interactive stage falls back to the nudge paths"
    );
}

#[tokio::test]
async fn non_interactive_stage_has_no_channel() {
    let dir = std::env::temp_dir().join(format!("tjstage-wiring-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let session: Arc<dyn tj_chat::ChatSession> = ScriptedSession::new();
    let stage = build_stage(&dir, false, Some(session)).await;
    assert!(
        stage.question_channel().is_none(),
        "non-interactive stages never declare the question tool"
    );
}
