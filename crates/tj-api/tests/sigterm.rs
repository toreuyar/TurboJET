//! Process-level SIGTERM smoke: the real tj-api binary starts, accepts a
//! signal, runs the graceful shutdown path and exits successfully. Disposable
//! state and a synthetic workflow only; no repository config asset, production
//! database, deployment or paid call is used.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn sigterm_quiesces_and_exits_the_real_api_process() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let tree = tj_test_support::SyntheticTree::new();
    let port = free_port();
    let home = tree.root().join("home");
    std::fs::create_dir_all(&home).unwrap();
    tree.update_config(|config| {
        config["listen_addr"] = serde_json::json!(format!("127.0.0.1:{port}"));
        config["database_path"] = serde_json::json!(home.join("state.db").to_string_lossy());
        config["workspace_base"] = serde_json::json!(home.join("workspaces").to_string_lossy());
    })
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_tj-api"))
        .arg(tree.config_path())
        .env("TURBOJET_HOME", &home)
        .env("TURBOJET_WEBUI_PATH", repo.join("webui/dist/index.html"))
        .env(
            "TURBOJET_MASTER_KEY",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .env("TJ_SYNTHETIC_API_KEY", "unused-local-key")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn tj-api");

    // Wait for the listener rather than a fixed sleep.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("tj-api exited before listening: {status}");
        }
        assert!(Instant::now() < deadline, "tj-api did not listen on {port}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let pid = child.id();
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "could not send SIGTERM");

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "tj-api did not exit cleanly after SIGTERM: {status}"
            );
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("tj-api did not quiesce after SIGTERM");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
