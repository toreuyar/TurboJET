//! Execution-scoped shell supervision. Dropping a tool future requests process
//! shutdown; the flow owner can then await reaping before deleting its workspace.
use anyhow::{Context, Result};
use std::{
    future::Future,
    process::{Output, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, sync::oneshot, task::JoinHandle};

tokio::task_local! {
    static PROCESSES: ProcessSupervisor;
}

#[derive(Clone, Default)]
pub struct ProcessSupervisor {
    tasks: Arc<Mutex<Vec<JoinHandle<Result<()>>>>>,
}

impl ProcessSupervisor {
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        PROCESSES.scope(self.clone(), future).await
    }

    /// Call after the owning execution future has stopped: no new tools can be
    /// registered, and each dropped shell receiver has signalled cancellation.
    pub async fn join(&self) -> Result<()> {
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|p| p.into_inner()));
        let mut first_error = None;
        for task in tasks {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    first_error.get_or_insert(error);
                }
                Err(error) => {
                    first_error
                        .get_or_insert(anyhow::anyhow!("Process supervisor failed: {error}"));
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// A distinct process group contains ordinary shell descendants. This is basic
/// cancellation containment, not a sandbox against processes calling setsid().
struct ProcessGroup(i32, bool);

impl ProcessGroup {
    fn disarm(&mut self) {
        self.1 = false;
    }

    fn kill(&self) -> Result<()> {
        // SAFETY: kill accepts a negative process-group ID; this ID comes from
        // our freshly spawned child with process_group(0), never from tool input.
        let result = unsafe { libc::kill(-self.0, libc::SIGKILL) };
        if result == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn has_live_members(&self) -> Result<bool> {
        for entry in std::fs::read_dir("/proc")? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
                continue;
            }
            let stat = match std::fs::read_to_string(entry.path().join("stat")) {
                Ok(stat) => stat,
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(libc::ESRCH) =>
                {
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let Some((_, fields)) = stat.rsplit_once(')') else {
                continue;
            };
            let fields: Vec<_> = fields.split_whitespace().take(3).collect();
            if fields.len() == 3
                && fields[2].parse::<i32>().ok() == Some(self.0)
                && !matches!(fields[0], "Z" | "X")
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    #[cfg(not(target_os = "linux"))]
    fn has_live_members(&self) -> Result<bool> {
        // On other Unix platforms conservatively wait for the group to vanish.
        let result = unsafe { libc::kill(-self.0, 0) };
        Ok(result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH))
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if self.1 {
            let _ = self.kill();
        }
    }
}

async fn read_capped<R>(mut reader: R, limit: usize) -> Result<(Vec<u8>, bool)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut captured = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(captured.len());
        if read <= remaining {
            captured.extend_from_slice(&chunk[..read]);
        } else {
            if remaining > 0 {
                captured.extend_from_slice(&chunk[..remaining]);
            }
            // Keep draining so the child cannot block on a full pipe, but
            // never retain more than the configured cap in memory.
            truncated = true;
        }
    }
    Ok((captured, truncated))
}

pub(crate) async fn output(mut command: Command, timeout: Duration) -> Result<Output> {
    let supervisor = PROCESSES.try_with(Clone::clone).unwrap_or_default();
    command
        // Never let a model shell read the service's stdin (a terminal, socket
        // or inherited descriptor); commands that need input must fail/time out.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = command.spawn().context("Failed to spawn shell")?;
    let mut group = ProcessGroup(child.id().context("Shell has no process ID")? as i32, true);
    let stdout = child.stdout.take().context("Shell stdout unavailable")?;
    let stderr = child.stderr.take().context("Shell stderr unavailable")?;
    let stdout_task = tokio::spawn(read_capped(stdout, crate::isolation::MAX_STREAM_BYTES));
    let stderr_task = tokio::spawn(read_capped(stderr, crate::isolation::MAX_STREAM_BYTES));
    let (result_tx, result_rx) = oneshot::channel();
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let waiter = tokio::select! {
            result = child.wait() => result.map_err(anyhow::Error::from),
            _ = cancel_rx => Err(anyhow::anyhow!("Shell execution cancelled")),
            _ = tokio::time::sleep(timeout) => Err(anyhow::anyhow!("Command timed out after {} seconds", timeout.as_secs())),
        };
        // Terminate background descendants even when the shell exits normally.
        let cleanup = async {
            group.kill()?;
            child.wait().await.context("Failed to reap shell")?;
            while group.has_live_members()? {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, anyhow::Error>(())
        };
        let cleanup = tokio::time::timeout(Duration::from_secs(4), cleanup)
            .await
            .map_err(|_| anyhow::anyhow!("Shell process group did not stop; preserve workspace"))
            .and_then(|result| result);
        if let Err(error) = &cleanup {
            let _ = result_tx.send(Err(anyhow::anyhow!("Shell cleanup failed: {error}")));
        } else {
            group.disarm();
            let joined = tokio::time::timeout(Duration::from_secs(4), async {
                let stdout = stdout_task.await.context("stdout reader panicked")??;
                let stderr = stderr_task.await.context("stderr reader panicked")??;
                Ok::<_, anyhow::Error>((stdout, stderr))
            })
            .await;
            match joined {
                Err(_) => {
                    let _ = result_tx.send(Err(anyhow::anyhow!(
                        "Shell output readers did not stop after process group cleanup"
                    )));
                }
                Ok(Err(error)) => {
                    let _ = result_tx.send(Err(error));
                }
                Ok(Ok(((out, out_truncated), (err, err_truncated)))) => {
                    let result = match waiter {
                        Err(error) => Err(error),
                        Ok(_status) if out_truncated || err_truncated => Err(anyhow::anyhow!(
                            "Command output exceeded {} bytes per stream; output was truncated",
                            crate::isolation::MAX_STREAM_BYTES
                        )),
                        Ok(status) => Ok(Output {
                            status,
                            stdout: out,
                            stderr: err,
                        }),
                    };
                    let _ = result_tx.send(result);
                }
            }
        }
        cleanup
    });
    supervisor
        .tasks
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(task);
    // Keeping this sender in the caller's future ties cancellation to dropping
    // that future, including runner timeouts and manager abort-and-join.
    let result = result_rx.await.context("Shell supervisor disappeared")?;
    drop(cancel_tx);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn wait_for_file(path: &std::path::Path) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("script did not start");
    }

    #[tokio::test]
    async fn dropping_execution_joins_shell_and_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = ProcessSupervisor::default();
        let scope = supervisor.clone();
        let workspace = dir.path().to_path_buf();
        let task = tokio::spawn(async move {
            scope
                .scope(async move {
                    let mut command = Command::new("bash");
                    command
                        .arg("-c")
                        .arg("sleep 30 & echo $$ > group; echo $! > child; wait")
                        .current_dir(workspace);
                    output(command, Duration::from_secs(60)).await
                })
                .await
        });
        wait_for_file(&dir.path().join("child")).await;
        let group = std::fs::read_to_string(dir.path().join("group"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(5), supervisor.join())
            .await
            .unwrap()
            .unwrap();
        assert!(!ProcessGroup(group, false).has_live_members().unwrap());
    }

    #[tokio::test]
    async fn shell_timeout_waits_for_descendant_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = ProcessSupervisor::default();
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg("sleep 30 & echo $$ > group; wait")
            .current_dir(dir.path());
        let error = supervisor
            .scope(output(command, Duration::from_millis(100)))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        supervisor.join().await.unwrap();
        let group = std::fs::read_to_string(dir.path().join("group"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(!ProcessGroup(group, false).has_live_members().unwrap());
    }

    #[tokio::test]
    async fn successful_shell_stops_background_children_too() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = ProcessSupervisor::default();
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg("sleep 30 >/dev/null 2>&1 & echo $$ > group; echo done")
            .current_dir(dir.path());
        let result = supervisor
            .scope(output(command, Duration::from_secs(2)))
            .await
            .unwrap();
        supervisor.join().await.unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"done\n");
        let group = std::fs::read_to_string(dir.path().join("group"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(!ProcessGroup(group, false).has_live_members().unwrap());
    }
}
