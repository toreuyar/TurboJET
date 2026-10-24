use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::storage::LogStorage;
use crate::types::{LogEntry, LogFilter, LogLevel, LogSearchResult, LogSource, LogStats};

pub struct LogReader {
    storage: LogStorage,
    system_log_path: PathBuf,
}

impl LogReader {
    pub fn new<P: AsRef<Path>>(log_dir: P, db_path: &str) -> Result<Self> {
        let log_dir = PathBuf::from(log_dir.as_ref());
        let system_log_path = log_dir.join("turbojet.jsonl");
        let storage = LogStorage::new(&log_dir, db_path)?;

        Ok(Self {
            storage,
            system_log_path,
        })
    }

    pub fn read_system_logs(&self, filter: &LogFilter) -> Result<Vec<LogEntry>> {
        let mut entries = Vec::new();

        if !self.system_log_path.exists() {
            return Ok(entries);
        }

        let file = File::open(&self.system_log_path).context("Failed to open system log file")?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = line.context("Failed to read log line")?;
            if let Some(entry) = self.parse_tracing_log(&line) {
                if self.matches_filter(&entry, filter) {
                    entries.push(entry);
                }
            }
        }

        // Apply limit and offset
        if let Some(offset) = filter.offset {
            entries = entries.into_iter().skip(offset).collect();
        }
        if let Some(limit) = filter.limit {
            entries.truncate(limit);
        }

        Ok(entries)
    }

    pub fn read_container_logs(&self, stage_id: &str) -> Result<Vec<LogEntry>> {
        let container_info = self
            .storage
            .read_container_log_info(stage_id)?
            .context("Container log info not found")?;

        let log_path = PathBuf::from(&container_info.log_path);
        if !log_path.exists() {
            return Ok(Vec::new());
        }

        let mut entries = Vec::new();
        let file = File::open(&log_path).context("Failed to open container log file")?;
        let reader = BufReader::new(file);

        for (index, line) in reader.lines().enumerate() {
            let line = line.context("Failed to read log line")?;
            let entry = LogEntry {
                id: format!("{}_{}", stage_id, index),
                timestamp: Utc::now(),
                level: LogLevel::Info,
                source: LogSource::Stage,
                component: "container".to_string(),
                message: line.clone(),
                stage_id: Some(stage_id.to_string()),
                container_id: Some(container_info.container_id.clone()),
                raw: Some(line),
                ..Default::default()
            };
            entries.push(entry);
        }

        Ok(entries)
    }

    fn row_to_log_entry(row: &rusqlite::Row) -> rusqlite::Result<LogEntry> {
        Ok(LogEntry {
            id: row.get(0)?,
            stage_id: row.get(1)?,
            timestamp: {
                let ts: String = row.get(2)?;
                DateTime::parse_from_rfc3339(&ts)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now())
            },
            level: row.get::<_, String>(3)?.parse().unwrap_or(LogLevel::Info),
            source: row
                .get::<_, String>(4)?
                .parse()
                .unwrap_or(LogSource::TurboJet),
            component: row.get(5)?,
            message: row.get(6)?,
            flow_id: row.get(7)?,
            run_id: row.get(8)?,
            container_id: row.get(9)?,
            details: row
                .get::<_, Option<String>>(10)?
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .unwrap_or(None),
            raw: row.get(11)?,
        })
    }

    pub fn read_stage_logs(&self, stage_id: &str, filter: &LogFilter) -> Result<Vec<LogEntry>> {
        let conn = self
            .storage
            .db
            .lock()
            .map_err(|e| anyhow::anyhow!("DB mutex poisoned: {}", e))?;

        let mut query = String::from(
            "SELECT id, stage_id, timestamp, level, source, component, message,
                    flow_id, run_id, container_id, details, raw
             FROM stage_logs WHERE stage_id = ?1",
        );

        if let Some(level) = &filter.level {
            query.push_str(&format!(" AND level = '{}'", level.as_str()));
        }
        if let Some(source) = &filter.source {
            query.push_str(&format!(" AND source = '{}'", source.as_str()));
        }
        if let Some(since) = &filter.since {
            query.push_str(&format!(" AND timestamp >= '{}'", since.to_rfc3339()));
        }
        if let Some(until) = &filter.until {
            query.push_str(&format!(" AND timestamp <= '{}'", until.to_rfc3339()));
        }

        query.push_str(" ORDER BY timestamp DESC");

        if let Some(limit) = filter.limit {
            query.push_str(&format!(" LIMIT {}", limit));
        }
        if let Some(offset) = filter.offset {
            query.push_str(&format!(" OFFSET {}", offset));
        }

        let mut stmt = conn
            .prepare(&query)
            .context("Failed to prepare stage logs query")?;

        let mut entries = Vec::new();
        let rows = stmt
            .query_map([stage_id], Self::row_to_log_entry)
            .context("Failed to execute stage logs query")?;

        for row in rows {
            entries.push(row.context("Failed to parse log entry")?);
        }

        Ok(entries)
    }

    pub fn search_logs(&self, query: &str, filter: &LogFilter) -> Result<LogSearchResult> {
        let regex = Regex::new(query).context("Failed to compile search regex")?;

        let mut all_entries = Vec::new();

        // Search system logs
        if filter.source.is_none() || filter.source == Some(LogSource::TurboJet) {
            all_entries.extend(self.read_system_logs(filter)?);
        }

        // Search stage logs from database
        if filter.source.is_none() || filter.source != Some(LogSource::TurboJet) {
            let conn = self
                .storage
                .db
                .lock()
                .map_err(|e| anyhow::anyhow!("DB mutex poisoned: {}", e))?;

            let mut stmt = conn
                .prepare(
                    "SELECT id, stage_id, timestamp, level, source, component, message,
                        flow_id, run_id, container_id, details, raw
                 FROM stage_logs",
                )
                .context("Failed to prepare search query")?;

            let rows = stmt
                .query_map([], Self::row_to_log_entry)
                .context("Failed to execute search query")?;

            for row in rows {
                all_entries.push(row.context("Failed to parse log entry")?);
            }
        }

        // Filter by search query
        let mut entries: Vec<LogEntry> = all_entries
            .into_iter()
            .filter(|entry| {
                regex.is_match(&entry.message)
                    || entry.raw.as_ref().is_some_and(|r| regex.is_match(r))
            })
            .filter(|entry| self.matches_filter(entry, filter))
            .collect();

        let total_matches = entries.len();
        let stats = self.calculate_stats(&entries);

        // Apply pagination
        if let Some(offset) = filter.offset {
            entries = entries.into_iter().skip(offset).collect();
        }
        if let Some(limit) = filter.limit {
            entries.truncate(limit);
        }

        Ok(LogSearchResult {
            entries,
            total_matches,
            stats,
        })
    }

    pub fn stream_logs(&self, filter: LogFilter) -> Result<impl futures::Stream<Item = LogEntry>> {
        let entries = self.read_system_logs(&filter)?;
        Ok(futures::stream::iter(entries))
    }

    fn parse_tracing_log(&self, line: &str) -> Option<LogEntry> {
        // Try to parse as JSON (tracing JSON format)
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            return Some(LogEntry {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp: value
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now),
                level: value
                    .get("level")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(LogLevel::Info),
                source: LogSource::TurboJet,
                component: value
                    .get("target")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                message: value
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                raw: Some(line.to_string()),
                ..Default::default()
            });
        }

        // Fallback: parse as plain text log
        Some(LogEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            level: LogLevel::Info,
            source: LogSource::TurboJet,
            component: "unknown".to_string(),
            message: line.to_string(),
            raw: Some(line.to_string()),
            ..Default::default()
        })
    }

    fn matches_filter(&self, entry: &LogEntry, filter: &LogFilter) -> bool {
        if let Some(level) = &filter.level {
            if entry.level != *level {
                return false;
            }
        }

        if let Some(source) = &filter.source {
            if entry.source != *source {
                return false;
            }
        }

        if let Some(component) = &filter.component {
            if !entry.component.contains(component) {
                return false;
            }
        }

        if let Some(stage_id) = &filter.stage_id {
            if entry.stage_id.as_ref() != Some(stage_id) {
                return false;
            }
        }

        if let Some(flow_id) = &filter.flow_id {
            if entry.flow_id.as_ref() != Some(flow_id) {
                return false;
            }
        }

        if let Some(run_id) = &filter.run_id {
            if entry.run_id.as_ref() != Some(run_id) {
                return false;
            }
        }

        if let Some(container_id) = &filter.container_id {
            if entry.container_id.as_ref() != Some(container_id) {
                return false;
            }
        }

        if let Some(since) = &filter.since {
            if entry.timestamp < *since {
                return false;
            }
        }

        if let Some(until) = &filter.until {
            if entry.timestamp > *until {
                return false;
            }
        }

        true
    }

    fn calculate_stats(&self, entries: &[LogEntry]) -> LogStats {
        let mut source_counts: HashMap<String, usize> = HashMap::new();
        let mut error_count = 0;
        let mut warning_count = 0;

        for entry in entries {
            *source_counts
                .entry(entry.source.as_str().to_string())
                .or_insert(0) += 1;
            if entry.level == LogLevel::Error {
                error_count += 1;
            } else if entry.level == LogLevel::Warn {
                warning_count += 1;
            }
        }

        let time_range = if entries.len() > 1 {
            let timestamps: Vec<_> = entries.iter().map(|e| e.timestamp).collect();
            let min = timestamps.iter().min().unwrap();
            let max = timestamps.iter().max().unwrap();
            Some((*min, *max))
        } else {
            None
        };

        LogStats {
            total_entries: entries.len(),
            error_count,
            warning_count,
            source_counts,
            time_range,
        }
    }
}

impl Default for LogEntry {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            level: LogLevel::Info,
            source: LogSource::TurboJet,
            component: "unknown".to_string(),
            message: String::new(),
            stage_id: None,
            flow_id: None,
            run_id: None,
            container_id: None,
            details: None,
            raw: None,
        }
    }
}
