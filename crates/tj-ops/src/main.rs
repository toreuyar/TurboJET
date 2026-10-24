//! Operator CLI: backup/restore/verify/package. It only acts on explicit
//! paths, never installs into system locations or talks to a production
//! service. See `docs/runbook-self-hosted.md`.

use anyhow::{Context, Result};
use std::path::PathBuf;
use tj_ops::{create_backup, create_package, restore_backup, verify_backup};

fn usage() -> &'static str {
    "usage:\n\
     tj-ops backup --db <state.db> --out <dir> [--artifacts <dir>]\n\
     tj-ops verify --backup <dir> [--no-key-check]\n\
     tj-ops restore --backup <dir> --home <dir> [--force] [--no-key-check]\n\
     tj-ops package --out <dir> --binary <tj-api> --webui <index.html> [--config <dir>] [--workflows <dir>] [--version <v>]\n"
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(String::as_str).unwrap_or("");
    match command {
        "backup" => {
            let db = arg_value(&args, "--db").context("--db is required")?;
            let out = arg_value(&args, "--out").context("--out is required")?;
            let artifacts = arg_value(&args, "--artifacts").map(PathBuf::from);
            let manifest = create_backup(
                &PathBuf::from(db),
                artifacts.as_deref(),
                &PathBuf::from(out),
            )?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        "verify" => {
            let backup = arg_value(&args, "--backup").context("--backup is required")?;
            let manifest = verify_backup(&PathBuf::from(backup), !flag(&args, "--no-key-check"))?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        "restore" => {
            let backup = arg_value(&args, "--backup").context("--backup is required")?;
            let home = arg_value(&args, "--home").context("--home is required")?;
            let manifest = restore_backup(
                &PathBuf::from(backup),
                &PathBuf::from(home),
                flag(&args, "--force"),
                !flag(&args, "--no-key-check"),
            )?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        "package" => {
            let out = arg_value(&args, "--out").context("--out is required")?;
            let binary = arg_value(&args, "--binary").context("--binary is required")?;
            let webui = arg_value(&args, "--webui").context("--webui is required")?;
            let config = arg_value(&args, "--config").map(PathBuf::from);
            let workflows = arg_value(&args, "--workflows").map(PathBuf::from);
            let version = arg_value(&args, "--version")
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
            let manifest = create_package(
                &PathBuf::from(out),
                &PathBuf::from(binary),
                &PathBuf::from(webui),
                config.as_deref(),
                workflows.as_deref(),
                &version,
            )?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        "help" | "--help" | "-h" => println!("{}", usage()),
        _ => {
            eprint!("{}", usage());
            std::process::exit(2);
        }
    }
    Ok(())
}
