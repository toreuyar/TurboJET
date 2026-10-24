//! Shared test harness for T-5 question-channel tests: a scripted
//! [`ChatSession`] mock and small tj-db fixtures.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tj_chat::ChatSession;
use tj_db::{Database, FlowId, FlowRecord, StageLog, StageRecord, StageStatus};
use tj_runner::{QuestionOption, QuestionRequest};
use tokio::sync::mpsc;

/// Chat session whose `send` records messages and whose `receive` pops from a
/// controllable reply queue — the test decides when (and what) the user answers.
pub struct ScriptedSession {
    sent: Mutex<Vec<String>>,
    reply_tx: mpsc::UnboundedSender<String>,
    reply_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>,
    fail_receive: std::sync::atomic::AtomicBool,
}

impl ScriptedSession {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            sent: Mutex::new(Vec::new()),
            reply_tx: tx,
            reply_rx: tokio::sync::Mutex::new(rx),
            fail_receive: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }

    pub fn reply(&self, text: &str) {
        self.reply_tx.send(text.to_string()).unwrap();
    }

    pub fn fail_receive(&self, fail: bool) {
        self.fail_receive
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }
}

impl ChatSession for ScriptedSession {
    fn send(
        &self,
        message: &str,
    ) -> Pin<Box<dyn Future<Output = tj_chat::ChatResult<()>> + Send + '_>> {
        self.sent.lock().unwrap().push(message.to_string());
        Box::pin(async { Ok(()) })
    }

    fn receive(&self) -> Pin<Box<dyn Future<Output = tj_chat::ChatResult<String>> + Send + '_>> {
        let fail = self.fail_receive.load(std::sync::atomic::Ordering::Relaxed);
        Box::pin(async move {
            if fail {
                return Err(tj_chat::ChatError::Other("channel gone".into()));
            }
            self.reply_rx
                .lock()
                .await
                .recv()
                .await
                .ok_or_else(|| tj_chat::ChatError::Other("reply channel closed".into()))
        })
    }
}

/// Fresh SQLite database in a throwaway directory (tj-db test pattern).
pub fn test_db() -> Arc<Database> {
    let dir = std::env::temp_dir().join(format!("tjstage-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    Arc::new(Database::open(&dir.join("state.db").to_string_lossy()).expect("test db"))
}

/// Insert the run + flow rows a stage row's foreign keys point at.
pub fn insert_run_and_flow(db: &Database, run_id: &str, flow_id: &str) {
    db.save_run(&tj_db::PipelineRun::new(
        run_id.to_string(),
        None,
        None,
        "test run".to_string(),
        tj_db::RunStatus::Running,
        None,
        None,
        None,
        None,
    ))
    .unwrap();
    db.save_flow(&FlowRecord::new(
        flow_id.to_string(),
        tj_db::RunId(run_id.to_string()),
        "flow-a".to_string(),
        Some(1),
        None,
        None,
    ))
    .unwrap();
}

/// Insert a run row + flow row + a Running stage row, as the executor would.
pub fn insert_running_stage(db: &Database, run_id: &str, flow_id: &str, stage_id: &str) {
    insert_run_and_flow(db, run_id, flow_id);
    let record = StageRecord::new(
        stage_id.to_string(),
        FlowId(flow_id.to_string()),
        "stage-a".to_string(),
        StageStatus::Running,
        0,
        None,
    );
    db.save_stage(&record).unwrap();
}

pub fn sample_request() -> QuestionRequest {
    QuestionRequest {
        question: "Which database should we use?".to_string(),
        options: vec![
            QuestionOption {
                id: "opt-a".to_string(),
                label: "Option A".to_string(),
            },
            QuestionOption {
                id: "opt-b".to_string(),
                label: "Option B".to_string(),
            },
            QuestionOption {
                id: "opt-c".to_string(),
                label: "Option C".to_string(),
            },
        ],
        multi_select: false,
        free_form: true,
    }
}

pub fn logs_for(db: &Database, run_id: &str) -> Vec<StageLog> {
    db.list_stage_logs(run_id, 0, 200).unwrap()
}

/// Poll the DB until the stage row reaches `target` (or panic after ~2s).
pub async fn wait_for_status(db: &Database, stage_id: &str, target: StageStatus) {
    for _ in 0..200 {
        if let Some(row) = db.load_stage(stage_id).unwrap() {
            if *row.status() == target {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("stage {} never reached status {:?}", stage_id, target);
}
