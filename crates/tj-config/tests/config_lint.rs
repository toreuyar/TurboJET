//! Static config lint arms over the shipped workflow packages.
//!
//! This complements `smg_route_schemas_test.rs` (route keys == schema verdict
//! enum, schema + `max_output_retries` per stage, package coverage) and
//! `agent_schemas_test.rs` (package schema wiring into the execution bundle).
//! Pure file reads — deterministic, no network, no clock.
//!
//! The old shared agent/schema trees no longer exist. Workflow contracts are
//! read from self-contained packages under `workflows/`
//! (and the optional ignored local SDLC package when present). Only `*.json` /
//! `*.md` files are inspected; runtime dot-directories and `.env` credentials
//! are never read.

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(path: &Path) -> Value {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("invalid JSON in {}: {e}", path.display()))
}

/// Every lintable text file under `root`, recursing but skipping
/// dot-directories (runtime artifacts, secrets) and reading only
/// `*.json` / `*.md` files.
fn config_text_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        {
            let path = entry.expect("dir entry readable").path();
            let name = path
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .to_string();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "json" || e == "md") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Shipped package directories. The ignored local SDLC package is included only
/// when it exists; the default suite never reads it.
fn package_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let workflows = repo_root().join("workflows");
    if workflows.is_dir() {
        for entry in std::fs::read_dir(&workflows).expect("workflows dir readable") {
            let path = entry.expect("workflows entry readable").path();
            if path.is_dir() && path.join("workflow.smg.json").is_file() {
                roots.push(path);
            }
        }
    }
    let local = repo_root().join("config/local/full-sdlc.package");
    if local.join("workflow.smg.json").is_file() {
        roots.push(local);
    }
    roots.sort();
    roots
}

fn lint_files() -> Vec<PathBuf> {
    let mut paths = config_text_files(&repo_root().join("config"));
    for package in package_roots() {
        paths.extend(config_text_files(&package));
    }
    paths.sort();
    paths
}

fn lint_violations(tokens: &[&str]) -> Vec<String> {
    let mut hits = Vec::new();
    for path in lint_files() {
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        for (line_no, line) in raw.lines().enumerate() {
            for token in tokens {
                if line.contains(token) {
                    hits.push(format!(
                        "{}:{}: contains {token:?}",
                        path.display(),
                        line_no + 1
                    ));
                }
            }
        }
    }
    hits
}

/// AC-11/FR-21 static arm: the marker protocol is retired — no config file
/// may teach it, not even inside a negating sentence (this is a literal
/// grep, so prompts must avoid the token entirely).
#[test]
fn no_config_file_contains_the_retired_completion_marker() {
    let hits = lint_violations(&["::TURBOJET:DONE::"]);
    assert!(
        hits.is_empty(),
        "retired completion marker still present under config/ or workflows/:\n{}",
        hits.join("\n")
    );
}

/// AC-7/FR-21 static arm: `needs-clarification` is no longer part of any
/// stage's verdict space (D-3) — no config file may reference it.
#[test]
fn no_config_file_references_needs_clarification() {
    let hits = lint_violations(&["needs-clarification"]);
    assert!(
        hits.is_empty(),
        "retired needs-clarification verdict still referenced under config/ or workflows/:\n{}",
        hits.join("\n")
    );
}

/// True when `line` uses `"status"` as a JSON object key (allowing
/// whitespace before the colon) — the machine-read routing key must be
/// `verdict` (FR-18). Hardens the `set status to` phrase check in
/// `smg_route_schemas_test.rs` beyond that exact phrasing.
fn line_uses_status_as_json_key(line: &str) -> bool {
    let needle = "\"status\"";
    let mut from = 0;
    while let Some(idx) = line[from..].find(needle) {
        let after = from + idx + needle.len();
        if line[after..].trim_start().starts_with(':') {
            return true;
        }
        from = after;
    }
    false
}

/// AC-14 prompt arm: no prompt instructs emitting a `status` JSON key —
/// routing reads `verdict`.
#[test]
fn no_prompt_instructs_a_status_json_key() {
    let mut hits = Vec::new();
    for package in package_roots() {
        let agents = package.join("agents");
        if !agents.is_dir() {
            continue;
        }
        for path in config_text_files(&agents) {
            if path.file_name().is_none_or(|n| n != "prompt.md") {
                continue;
            }
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
            for (line_no, line) in raw.lines().enumerate() {
                if line_uses_status_as_json_key(line) {
                    hits.push(format!(
                        "{}:{}: {}",
                        path.display(),
                        line_no + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "prompts still present a \"status\" JSON key (routing key is verdict):\n{}",
        hits.join("\n")
    );
}

/// Resolve a package ADC schema the same way import does: package-root-relative
/// first, then ADC-directory-relative. The result must stay inside the package.
fn resolve_schema(package: &Path, adc_path: &Path, reference: &str) -> PathBuf {
    let relative = Path::new(reference);
    assert!(
        !relative.is_absolute(),
        "{}: schema '{}' must be package-relative",
        adc_path.display(),
        reference
    );
    let candidates = [
        package.join(relative),
        adc_path.parent().expect("ADC parent").join(relative),
    ];
    for candidate in candidates {
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!(
        "{}: schema '{}' does not resolve inside {}",
        adc_path.display(),
        reference,
        package.display()
    )
}

/// Orphan-schema detection: every `schemas/*.json` in a package must be bound
/// by at least one agent.json `"schema"` or `output.status.schema` path.
#[test]
fn every_schema_file_is_bound_by_at_least_one_package_agent() {
    let mut hits = Vec::new();
    for package in package_roots() {
        let mut bound: BTreeSet<PathBuf> = BTreeSet::new();
        let agents = package.join("agents");
        if !agents.is_dir() {
            continue;
        }
        for adc_path in config_text_files(&agents) {
            if adc_path.file_name().is_none_or(|n| n != "agent.json") {
                continue;
            }
            let adc = read_json(&adc_path);
            if let Some(schema) = adc["schema"].as_str().filter(|s| !s.trim().is_empty()) {
                bound.insert(resolve_schema(&package, &adc_path, schema));
            }
            if let Some(status) = adc
                .pointer("/output/status/schema")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
            {
                bound.insert(resolve_schema(&package, &adc_path, status));
            }
        }

        let schemas_dir = package.join("schemas");
        if !schemas_dir.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&schemas_dir).expect("package schemas dir") {
            let path = entry.expect("schemas entry readable").path();
            if !path.extension().is_some_and(|e| e == "json") {
                continue;
            }
            if !bound.contains(&path) {
                hits.push(format!(
                    "{} is not bound by any agent.json in {}",
                    path.display(),
                    package.display()
                ));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "orphan package schemas: {}\nBind them to an agent.json, delete them, or document an explicit exception",
        hits.join("; ")
    );
}
