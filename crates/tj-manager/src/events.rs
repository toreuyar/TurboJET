pub use tj_flow::FlowOutcome;

#[derive(Debug)]
pub enum ManagerEvent {
    RunRequested {
        run_id: String,
        trace_id: String,
    },
    /// Startup reconciliation for durable effects accepted by the previous
    /// process. Only pure routing effects are resumed automatically.
    ReconcilePendingEffects {
        run_id: String,
    },
    /// Explicit retry after an interrupted run. Starts a new attempt.
    RetryRequested {
        run_id: String,
        flow_name: String,
        attempt_offset: u32,
        trace_id: String,
    },
    FlowDone {
        attempt_id: String,
        run_id: String,
        flow_name: String,
        outcome: FlowOutcome,
        trace_id: String,
    },
    GateReached {
        run_id: String,
        flow_name: String,
        stage_name: String,
        summary: String,
        trace_id: String,
    },
    ResumeFlow {
        run_id: String,
        completed_stage_id: String,
        trace_id: String,
    },
    WatchdogTick {
        run_id: String,
        generation: String,
    },
    UserInput {
        message: String,
    },
    GateDecision {
        decision: tj_db::GateDecision,
        expected_state_version: i64,
        reply: tokio::sync::oneshot::Sender<Result<tj_db::GateDecisionReceipt, String>>,
    },
    /// A human recorded the exact Engineering task selection. This is the
    /// only event that may start the declared selection target after a plan gate.
    TaskSelected {
        run_id: String,
        selection_id: String,
    },
    RunDeleted {
        run_id: String,
    },
    CancelRun {
        run_id: String,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Shutdown {
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    ProjectDeleted {
        project_id: String,
    },
    SecretUpdated {
        project_id: String,
        key: String,
    },
}
