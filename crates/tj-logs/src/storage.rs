use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::types::{ContainerLogInfo, LogEntry};

pub struct LogStorage {
    pub log_dir: PathBuf,
    pub db: Mutex<rusqlite::Connection>,
}

impl LogStorage {
    pub fn new<P: AsRef<Path>>(log_dir: P, db_path: &str) -> Result<Self> {
        let log_dir = PathBuf::from(log_dir.as_ref());
        std::fs::create_dir_all(&log_dir).context("Failed to create log directory")?;

        // Create container logs subdirectory
        let container_logs_dir = log_dir.join("containers");
        std::fs::create_dir_all(&container_logs_dir)
            .context("Failed to create container logs directory")?;

        // Create stage logs subdirectory
        let stage_logs_dir = log_dir.join("stages");
        std::fs::create_dir_all(&stage_logs_dir)
            .context("Failed to create stage logs directory")?;

        let db =
            Mutex::new(rusqlite::Connection::open(db_path).context("Failed to open log database")?);

        let storage = Self { log_dir, db };
        storage.migrate()?;
        Ok(storage)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self
            .db
            .lock()
            .map_err(|e| anyhow::anyhow!("DB mutex poisoned: {}", e))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS stage_logs (
                id TEXT PRIMARY KEY,
                stage_id TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                level TEXT NOT NULL,
                source TEXT NOT NULL,
                component TEXT NOT NULL,
                message TEXT NOT NULL,
                flow_id TEXT,
                run_id TEXT,
                container_id TEXT,
                details TEXT,
                raw TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_stage_logs_stage_id ON stage_logs(stage_id);
            CREATE INDEX IF NOT EXISTS idx_stage_logs_timestamp ON stage_logs(timestamp);
            CREATE INDEX IF NOT EXISTS idx_stage_logs_level ON stage_logs(level);
            CREATE INDEX IF NOT EXISTS idx_stage_logs_source ON stage_logs(source);
            CREATE INDEX IF NOT EXISTS idx_stage_logs_run_id ON stage_logs(run_id);
            CREATE TABLE IF NOT EXISTS container_logs (
                stage_id TEXT PRIMARY KEY,
                container_id TEXT NOT NULL,
                log_path TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                line_count INTEGER NOT NULL,
                captured_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_container_logs_stage_id ON container_logs(stage_id);",
        )
        .context("Failed to create log tables")?;

        tracing::debug!("Log storage schema migrated");
        Ok(())
    }

    pub fn store_log_entry(&self, entry: &LogEntry) -> Result<()> {
        let conn = self
            .db
            .lock()
            .map_err(|e| anyhow::anyhow!("DB mutex poisoned: {}", e))?;

        let details_json = entry
            .details
            .as_ref()
            .map(serde_json::to_string)
            .transpose();

        conn.execute(
            "INSERT OR REPLACE INTO stage_logs (
                id, stage_id, timestamp, level, source, component, message,
                flow_id, run_id, container_id, details, raw, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                entry.id,
                entry.stage_id,
                entry.timestamp.to_rfc3339(),
                entry.level.as_str(),
                entry.source.as_str(),
                entry.component,
                entry.message,
                entry.flow_id,
                entry.run_id,
                entry.container_id,
                details_json.ok().flatten(),
                entry.raw,
                Utc::now().to_rfc3339(),
            ],
        )
        .context("Failed to store log entry")?;

        Ok(())
    }

    pub fn store_container_log_info(&self, info: &ContainerLogInfo) -> Result<()> {
        let conn = self
            .db
            .lock()
            .map_err(|e| anyhow::anyhow!("DB mutex poisoned: {}", e))?;

        conn.execute(
            "INSERT OR REPLACE INTO container_logs (
                stage_id, container_id, log_path, size_bytes, line_count, captured_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                info.stage_id,
                info.container_id,
                info.log_path,
                info.size_bytes,
                info.line_count,
                info.captured_at.to_rfc3339(),
            ],
        )
        .context("Failed to store container log info")?;

        Ok(())
    }

    pub fn get_log_path(&self, log_type: &str, identifier: &str) -> PathBuf {
        match log_type {
            "container" => self
                .log_dir
                .join("containers")
                .join(format!("{}.jsonl", identifier)),
            "stage" => self
                .log_dir
                .join("stages")
                .join(format!("{}.jsonl", identifier)),
            "aggregate" => self.log_dir.join("containers").join("aggregate.jsonl"),
            "errors" => self.log_dir.join("stages").join("errors.jsonl"),
            _ => self.log_dir.join(format!("{}.jsonl", identifier)),
        }
    }

    pub fn read_container_log_info(&self, stage_id: &str) -> Result<Option<ContainerLogInfo>> {
        let conn = self
            .db
            .lock()
            .map_err(|e| anyhow::anyhow!("DB mutex poisoned: {}", e))?;

        let mut stmt = conn
            .prepare(
                "SELECT stage_id, container_id, log_path, size_bytes, line_count, captured_at
             FROM container_logs WHERE stage_id = ?1",
            )
            .context("Failed to prepare container log query")?;

        let result = stmt.query_row(params![stage_id], |row| {
            Ok(ContainerLogInfo {
                stage_id: row.get(0)?,
                container_id: row.get(1)?,
                log_path: row.get(2)?,
                size_bytes: row.get(3)?,
                line_count: row.get(4)?,
                captured_at: {
                    let timestamp: String = row.get(5)?;
                    chrono::DateTime::parse_from_rfc3339(&timestamp)
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .unwrap_or_else(|_| Utc::now())
                },
            })
        });

        match result {
            Ok(info) => Ok(Some(info)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("Failed to query container log info"),
        }
    }

    pub fn append_to_file(&self, path: &Path, content: &str) -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .context("Failed to open log file for appending")?;

        use std::io::Write;
        writeln!(file, "{}", content).context("Failed to write to log file")?;

        Ok(())
    }
}
