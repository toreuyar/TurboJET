//! TJ-034 boundary evidence. These are real Landlock+seccomp executions on
//! this host; if the kernel lacks the prerequisite the test reports BLOCKED
//! explicitly instead of silently weakening the sandbox.

use std::path::PathBuf;
use tj_tools::{ShellTool, Tool, ToolContext};

fn context(workspace: &std::path::Path) -> ToolContext {
    ToolContext {
        workspace: workspace.to_path_buf(),
        output_dir: workspace.to_path_buf(),
    }
}

fn require_sandbox() -> bool {
    let status = tj_tools::isolation::LinuxSandbox::status();
    if !status.available {
        eprintln!(
            "BLOCKED (TJ-034 prerequisite): {}",
            tj_tools::isolation::describe_prerequisite()
        );
    }
    status.available
}

async fn shell(workspace: &std::path::Path, command: &str) -> serde_json::Value {
    ShellTool::new()
        .execute(
            serde_json::json!({"command": command, "timeout": 5}),
            &context(workspace),
        )
        .await
}

#[tokio::test]
async fn host_credentials_and_unlisted_paths_are_not_reachable() {
    if !require_sandbox() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let secret = root.path().join("outside-secret");
    std::fs::write(&secret, "do-not-read").unwrap();

    let host = shell(&workspace, "cat /etc/passwd").await;
    assert_eq!(host["success"], false, "host /etc/passwd readable: {host}");

    let outside = shell(&workspace, &format!("cat {}", secret.display())).await;
    assert_eq!(
        outside["success"], false,
        "outside secret readable: {outside}"
    );
}

#[tokio::test]
async fn symlinks_cannot_escape_the_workspace() {
    if !require_sandbox() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let outside = root.path().join("outside.txt");
    std::fs::write(&outside, "escaped").unwrap();
    std::os::unix::fs::symlink(&outside, workspace.join("link")).unwrap();

    let result = shell(&workspace, "cat link").await;
    assert_eq!(
        result["success"], false,
        "symlink escaped boundary: {result}"
    );
}

#[tokio::test]
async fn network_connect_is_denied() {
    if !require_sandbox() {
        return;
    }
    let workspace = tempfile::tempdir().unwrap();
    let result = shell(workspace.path(), "exec 3<>/dev/tcp/127.0.0.1/9").await;
    assert_eq!(result["success"], false, "network was reachable: {result}");
    assert!(
        result.to_string().contains("ermission denied")
            || result.to_string().contains("not permitted")
            || result.to_string().contains("refused"),
        "unexpected network result: {result}"
    );
}

#[tokio::test]
async fn ambient_environment_and_limits_are_bounded() {
    if !require_sandbox() {
        return;
    }
    unsafe {
        std::env::set_var("TJ_LEAK_CANARY", "must-not-appear");
    }
    let workspace = tempfile::tempdir().unwrap();
    let env = shell(workspace.path(), "env").await;
    assert_eq!(env["success"], true, "{env}");
    assert!(!env["stdout"].as_str().unwrap().contains("TJ_LEAK_CANARY"));
    assert!(env["stdout"].as_str().unwrap().contains("HOME="));
    unsafe {
        std::env::remove_var("TJ_LEAK_CANARY");
    }

    let limits = shell(workspace.path(), "ulimit -n; ulimit -c; ulimit -u").await;
    assert_eq!(limits["success"], true, "{limits}");
    assert_eq!(limits["stdout"].as_str().unwrap().trim(), "512\n0\n4096");
}

#[tokio::test]
async fn udp_unix_sockets_and_session_escape_are_denied() {
    if !require_sandbox() {
        return;
    }
    let workspace = tempfile::tempdir().unwrap();
    // Landlock ABI 4 only handles TCP, so UDP and AF_UNIX must be blocked by
    // the seccomp socket-family deny. Both are exercised explicitly rather
    // than inferred from the TCP test.
    let udp = shell(workspace.path(), "exec 3<>/dev/udp/127.0.0.1/9").await;
    assert_eq!(udp["success"], false, "UDP was reachable: {udp}");
    let unix = shell(
        workspace.path(),
        "python3 -c 'import socket; socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)'",
    )
    .await;
    assert_eq!(unix["success"], false, "AF_UNIX socket was allowed: {unix}");
    assert!(
        unix.to_string().contains("PermissionError")
            || unix.to_string().contains("Operation not permitted")
            || unix.to_string().contains("Permission denied"),
        "unexpected AF_UNIX result: {unix}"
    );

    // setsid() is a process-group escape primitive; seccomp denies it so the
    // supervisor's group kill cannot be evaded this way.
    let setsid = shell(workspace.path(), "python3 -c 'import os; os.setsid()'").await;
    assert_eq!(
        setsid["success"], false,
        "setsid escaped the process group: {setsid}"
    );
    assert!(
        setsid.to_string().contains("Operation not permitted")
            || setsid.to_string().contains("PermissionError"),
        "unexpected setsid result: {setsid}"
    );
}

#[tokio::test]
async fn oversized_stream_output_fails_instead_of_growing_without_bound() {
    if !require_sandbox() {
        return;
    }
    let workspace = tempfile::tempdir().unwrap();
    let result = shell(workspace.path(), "head -c 2000000 /dev/zero").await;
    assert_eq!(result["success"], false, "{result}");
    assert!(
        result["error"]
            .as_str()
            .unwrap_or_default()
            .contains("exceeded"),
        "expected stream cap error: {result}"
    );
}

#[tokio::test]
async fn missing_workspace_fails_closed() {
    if !require_sandbox() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let missing: PathBuf = root.path().join("missing");
    let result = shell(&missing, "echo should-not-run").await;
    assert_eq!(result["success"], false);
    assert!(
        result["error"]
            .as_str()
            .unwrap_or_default()
            .contains("isolation"),
        "{result}"
    );
}

#[tokio::test]
async fn background_job_writes_inside_workspace() {
    if !require_sandbox() {
        return;
    }
    let workspace = tempfile::tempdir().unwrap();
    let result = shell(
        workspace.path(),
        "sleep 2 & echo $! > child; echo ready > started; wait",
    )
    .await;
    assert_eq!(result["success"], true, "{result}");
    assert!(workspace.path().join("started").exists());
    assert!(workspace.path().join("child").exists());
}
