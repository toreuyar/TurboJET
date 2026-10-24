use anyhow::Result;
use std::io;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::main]
async fn main() -> Result<()> {
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config/turbojet.json".into());

    let config_raw = std::fs::read_to_string(&config_path)?;
    let config_json: serde_json::Value = serde_json::from_str(&config_raw)?;
    let log_level = config_json
        .get("log_level")
        .and_then(|v| v.as_str())
        .unwrap_or(if cfg!(debug_assertions) {
            "debug"
        } else {
            "info"
        });

    let turbojet_home = if std::env::var("TURBOJET_HOME").is_err() {
        let home = dirs::home_dir()
            .map(|h| h.join(".turbojet"))
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp/turbojet"));
        unsafe {
            std::env::set_var("TURBOJET_HOME", &home);
        }
        home
    } else {
        std::path::PathBuf::from(std::env::var("TURBOJET_HOME").unwrap())
    };

    let log_dir = turbojet_home.join("logs");
    std::fs::create_dir_all(&log_dir)?;

    let file_appender = tracing_appender::rolling::daily(&log_dir, "turbojet.jsonl");
    let (file_writer, _guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| format!(
            "turbojet={lvl},tj_manager={lvl},tj_api={lvl},tj_flow={lvl},tj_stage={lvl},tj_tool={lvl},tj_db={lvl},tj_agent={lvl},tj_provider={lvl},tj_runner={lvl},tj_config={lvl},tj_chat={lvl},tj_chat_zulip={lvl},tj_git={lvl},tower_http=warn",
            lvl = log_level
        ).parse().unwrap());

    // Color scheme by responsibility and depth for white-on-black terminals:
    // 🔵 API/HTTP (tj_api) - Blue - Surface layer
    // 🟢 Orchestration (tj_manager) - Green - High level coordination
    // 🟡 Pipeline (tj_flow) - Yellow - Flow coordination
    // 🟣 Execution (tj_stage, tj_tool, tj_runner) - Magenta/Orange - Work execution
    // 🔷 Infrastructure (tj_db, tj_chat, tj_config, tj_git) - Cyan - Support systems
    // 🔶 Providers (tj_provider) - Red - External integrations

    let console_layer = fmt::layer()
        .with_writer(io::stdout)
        .with_target(true) // Show colored module names
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .with_ansi(true)
        .with_level(true) // Show log levels with colors
        .compact();

    let file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(file_writer)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::filter::filter_fn(
            tj_api::logging::diagnostic_event_allowed,
        ))
        .with(console_layer)
        .with(file_layer)
        .init();

    std::mem::forget(_guard);

    tracing::info!("Starting TurboJet v2");
    tracing::info!(log_dir = %log_dir.display(), "Log directory");
    tracing::debug!("Config: {}", config_path);
    tracing::debug!("Log level: {}", log_level);

    // Feature 3: Log full resolved config with secrets redacted
    {
        fn redact_secrets(val: &mut serde_json::Value) {
            match val {
                serde_json::Value::Object(map) => {
                    for (key, v) in map.iter_mut() {
                        let key_lower = key.to_lowercase();
                        if key_lower.contains("key")
                            || key_lower.contains("secret")
                            || key_lower.contains("password")
                            || key_lower.contains("token")
                        {
                            *v = serde_json::Value::String("***REDACTED***".into());
                        } else {
                            redact_secrets(v);
                        }
                    }
                }
                serde_json::Value::Array(arr) => {
                    for v in arr.iter_mut() {
                        redact_secrets(v);
                    }
                }
                _ => {}
            }
        }
        let mut config_for_log = config_json.clone();
        redact_secrets(&mut config_for_log);
        tracing::debug!(config = %config_for_log, "Loaded configuration");
    }

    tj_api::run(&config_path).await
}
