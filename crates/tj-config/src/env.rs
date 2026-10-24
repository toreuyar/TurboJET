use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

pub struct EnvCascade {
    overrides: HashMap<String, String>,
}

impl Default for EnvCascade {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvCascade {
    pub fn new() -> Self {
        Self {
            overrides: HashMap::new(),
        }
    }

    pub fn load_from_file(&mut self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read .env file: {}", path.display()))?;

        for line in content.lines() {
            let line = line.trim();

            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim();

                let value = if (value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\''))
                {
                    &value[1..value.len() - 1]
                } else {
                    value
                };

                if std::env::var(key).is_err() {
                    self.overrides.insert(key.to_string(), value.to_string());
                }
            }
        }

        Ok(())
    }

    fn get(&self, key: &str) -> Option<String> {
        match std::env::var(key) {
            Ok(v) if !v.is_empty() => Some(v),
            _ => self.overrides.get(key).cloned(),
        }
    }

    /// Whether a key is present in the process environment or the loaded
    /// `.env` cascade, without exposing its value to logs.
    pub fn lookup(&self, key: &str) -> Option<String> {
        self.get(key)
    }

    pub fn resolve<'a>(&self, env_key: &str, config_value: &'a str) -> &'a str {
        match self.get(env_key) {
            Some(v) => {
                tracing::debug!(key = env_key, "Using env override");
                Box::leak(v.into_boxed_str())
            }
            None => config_value,
        }
    }

    pub fn resolve_or<'a>(
        &self,
        env_key: &str,
        config_value: Option<&'a str>,
        default: &'a str,
    ) -> &'a str {
        match self.get(env_key) {
            Some(v) => Box::leak(v.into_boxed_str()),
            None => config_value.unwrap_or(default),
        }
    }
}
