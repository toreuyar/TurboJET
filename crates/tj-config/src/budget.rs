//! Run-wide execution budgets (TJ-016).
//!
//! D5 accepted six question rounds per stage and twelve per Discovery run.
//! The remaining limits are conservative operational defaults: they exist so a
//! retrying or multi-call model cannot execute without bound, and they are
//! frozen into the run's execution bundle. Cost accounting remains TJ-024.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunBudgetLimits {
    /// D5: user questions asked by one stage execution (persistent lineage).
    #[serde(default = "default_stage_questions")]
    pub question_rounds_per_stage: u32,
    /// D5: user questions across the whole run, including rework/retry.
    #[serde(default = "default_run_questions")]
    pub question_rounds_per_run: u32,
    /// Total model round-trips, including corrections and retry attempts.
    #[serde(default = "default_model_calls")]
    pub model_calls: u32,
    /// Total tool calls; every call in a multi-call response is charged.
    #[serde(default = "default_tool_calls")]
    pub tool_calls: u32,
    /// Malformed/invalid output corrections only. Valid questions are free.
    #[serde(default = "default_output_corrections")]
    pub output_corrections: u32,
    /// Silent-turn nudges (interactive stage discipline), separate from output
    /// corrections so neither budget starves the other.
    #[serde(default = "default_interaction_nudges")]
    pub interaction_nudges: u32,
    /// Active execution time in seconds. Human gate waits and the time between
    /// an interruption and an explicit retry do not consume this budget.
    #[serde(default = "default_active_secs")]
    pub active_secs: u64,
    /// Optional monetary cap in millionths of the provider's configured
    /// currency unit. 0 disables the cap. When usage is not reported by the
    /// provider it remains explicitly unknown and is not charged as zero cost.
    #[serde(default)]
    pub cost_micros: u64,
}

fn default_stage_questions() -> u32 {
    6
}
fn default_run_questions() -> u32 {
    12
}
fn default_model_calls() -> u32 {
    120
}
fn default_tool_calls() -> u32 {
    200
}
fn default_output_corrections() -> u32 {
    24
}
fn default_interaction_nudges() -> u32 {
    12
}
fn default_active_secs() -> u64 {
    4 * 60 * 60
}

impl Default for RunBudgetLimits {
    fn default() -> Self {
        Self {
            question_rounds_per_stage: default_stage_questions(),
            question_rounds_per_run: default_run_questions(),
            model_calls: default_model_calls(),
            tool_calls: default_tool_calls(),
            output_corrections: default_output_corrections(),
            interaction_nudges: default_interaction_nudges(),
            active_secs: default_active_secs(),
            cost_micros: 0,
        }
    }
}

impl RunBudgetLimits {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.question_rounds_per_stage > 0,
            "budgets.question_rounds_per_stage must be positive"
        );
        anyhow::ensure!(
            self.question_rounds_per_run > 0,
            "budgets.question_rounds_per_run must be positive"
        );
        anyhow::ensure!(self.model_calls > 0, "budgets.model_calls must be positive");
        anyhow::ensure!(self.tool_calls > 0, "budgets.tool_calls must be positive");
        anyhow::ensure!(
            self.output_corrections > 0,
            "budgets.output_corrections must be positive"
        );
        anyhow::ensure!(
            self.interaction_nudges > 0,
            "budgets.interaction_nudges must be positive"
        );
        anyhow::ensure!(self.active_secs > 0, "budgets.active_secs must be positive");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_d5_and_are_positive() {
        let limits = RunBudgetLimits::default();
        assert_eq!(limits.question_rounds_per_stage, 6);
        assert_eq!(limits.question_rounds_per_run, 12);
        limits.validate().unwrap();
    }

    #[test]
    fn omitted_budgets_use_defaults_and_unknown_keys_fail() {
        let parsed: RunBudgetLimits = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, RunBudgetLimits::default());
        assert!(
            serde_json::from_str::<RunBudgetLimits>(r#"{"model_calls":0}"#)
                .unwrap()
                .validate()
                .is_err()
        );
        assert!(serde_json::from_str::<RunBudgetLimits>(r#"{"unknown":1}"#).is_err());
    }
}
