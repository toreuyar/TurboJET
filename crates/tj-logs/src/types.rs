use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

impl std::str::FromStr for LogLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "DEBUG" => Ok(LogLevel::Debug),
            "INFO" => Ok(LogLevel::Info),
            "WARN" => Ok(LogLevel::Warn),
            "ERROR" => Ok(LogLevel::Error),
            other => Err(format!("Unknown LogLevel: {}", other)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LogSource {
    TurboJet,
    Docker,
    Stage,
    Manager,
    Tool,
}

impl LogSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogSource::TurboJet => "turbojet",
            LogSource::Docker => "docker",
            LogSource::Stage => "stage",
            LogSource::Manager => "manager",
            LogSource::Tool => "tool",
        }
    }
}

impl std::str::FromStr for LogSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "turbojet" => Ok(LogSource::TurboJet),
            "docker" => Ok(LogSource::Docker),
            "stage" => Ok(LogSource::Stage),
            "manager" => Ok(LogSource::Manager),
            "tool" => Ok(LogSource::Tool),
            other => Err(format!("Unknown LogSource: {}", other)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub source: LogSource,
    pub component: String,
    pub message: String,
    pub stage_id: Option<String>,
    pub flow_id: Option<String>,
    pub run_id: Option<String>,
    pub container_id: Option<String>,
    pub details: Option<HashMap<String, serde_json::Value>>,
    pub raw: Option<String>, // Raw log line for reference
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LogFilter {
    pub level: Option<LogLevel>,
    pub source: Option<LogSource>,
    pub component: Option<String>,
    pub stage_id: Option<String>,
    pub flow_id: Option<String>,
    pub run_id: Option<String>,
    pub container_id: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub search_query: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogStats {
    pub total_entries: usize,
    pub error_count: usize,
    pub warning_count: usize,
    pub source_counts: HashMap<String, usize>,
    pub time_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerLogInfo {
    pub stage_id: String,
    pub container_id: String,
    pub log_path: String,
    pub size_bytes: u64,
    pub line_count: usize,
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSearchResult {
    pub entries: Vec<LogEntry>,
    pub total_matches: usize,
    pub stats: LogStats,
}
