//! Git worktree and remote operations with finite timeouts and explicit
//! failures. A worktree is a branch/work-file boundary, **not** a sandbox;
//! model shell execution is confined separately by TJ-034.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

pub const GIT_NETWORK_TIMEOUT: Duration = Duration::from_secs(300);
pub const GIT_LOCAL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub worktree_path: PathBuf,
    pub branch_name: String,
    pub repo_path: PathBuf,
}

#[derive(Clone)]
pub struct WorktreeManager {
    worktrees_base: PathBuf,
}

impl WorktreeManager {
    pub fn new(worktrees_base: &str) -> Result<Self> {
        let base = PathBuf::from(shellexpand::tilde(worktrees_base).to_string());
        std::fs::create_dir_all(&base)?;
        let base = std::fs::canonicalize(&base).context("Failed to canonicalize worktrees_base")?;
        tracing::debug!(base = %base.display(), "WorktreeManager initialized with canonical path");
        Ok(Self {
            worktrees_base: base,
        })
    }

    pub fn worktrees_base(&self) -> &PathBuf {
        &self.worktrees_base
    }

    pub fn create(
        &self,
        repo_url: &str,
        run_id: &str,
        slug: &str,
        base_branch: &str,
    ) -> Result<WorktreeInfo> {
        self.create_with_env(repo_url, run_id, slug, base_branch, None)
    }

    pub fn create_with_env(
        &self,
        repo_url: &str,
        run_id: &str,
        slug: &str,
        base_branch: &str,
        env: Option<&HashMap<String, String>>,
    ) -> Result<WorktreeInfo> {
        let branch_name = format!("tj/{}-{}", run_id, slug);
        let safe_name = branch_name.replace('/', "_");
        let worktree_path = self.worktrees_base.join(&safe_name);
        let repo_path = self.worktrees_base.join(format!("{}_repo", safe_name));

        // Never delete an existing workspace: it may contain unpublished work
        // that a restart/reconcile step still needs. Reconstruct instead.
        anyhow::ensure!(
            !worktree_path.exists(),
            "workspace {} already exists; preserving unpublished work instead of replacing it",
            worktree_path.display()
        );
        anyhow::ensure!(
            !repo_path.exists(),
            "repository clone {} already exists; preserving it for reconciliation",
            repo_path.display()
        );

        tracing::info!(
            repo_url = %repo_url,
            branch = %branch_name,
            base = %base_branch,
            has_ssh_env = env.is_some(),
            "Cloning repository for worktree"
        );

        let clone = run_git(
            None,
            &[
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                repo_url,
                repo_path.to_str().unwrap_or(""),
            ],
            env,
            GIT_NETWORK_TIMEOUT,
        );
        if let Err(error) = clone {
            // Do not leave a half-cloned directory that a later attempt could
            // mistake for a valid repository.
            let _ = std::fs::remove_dir_all(&repo_path);
            return Err(error)
                .context("initial remote clone failed; no empty repository was substituted");
        }

        run_git(
            Some(&repo_path),
            &[
                "fetch",
                "origin",
                &format!(
                    "refs/heads/{}:refs/remotes/origin/{}",
                    base_branch, base_branch
                ),
            ],
            env,
            GIT_NETWORK_TIMEOUT,
        )?;

        run_git(
            Some(&repo_path),
            &[
                "worktree",
                "add",
                "--detach",
                worktree_path.to_str().unwrap_or(""),
                &format!("origin/{}", base_branch),
            ],
            env,
            GIT_LOCAL_TIMEOUT,
        )?;

        run_git(
            Some(&worktree_path),
            &["checkout", "-b", &branch_name],
            env,
            GIT_LOCAL_TIMEOUT,
        )?;

        tracing::info!(path = %worktree_path.display(), branch = %branch_name, "Worktree created");
        Ok(WorktreeInfo {
            worktree_path,
            branch_name,
            repo_path,
        })
    }

    pub fn create_from_local(
        &self,
        local_repo: &str,
        run_id: &str,
        slug: &str,
        base_branch: &str,
    ) -> Result<WorktreeInfo> {
        self.create_from_local_with_env(local_repo, run_id, slug, base_branch, None)
    }

    pub fn create_from_local_with_env(
        &self,
        local_repo: &str,
        run_id: &str,
        slug: &str,
        base_branch: &str,
        env: Option<&HashMap<String, String>>,
    ) -> Result<WorktreeInfo> {
        let branch_name = format!("tj/{}-{}", run_id, slug);
        let safe_name = branch_name.replace('/', "_");
        let worktree_path = self.worktrees_base.join(&safe_name);
        let repo_path = PathBuf::from(shellexpand::tilde(local_repo).to_string());

        anyhow::ensure!(
            !worktree_path.exists(),
            "workspace {} already exists; preserving unpublished work",
            worktree_path.display()
        );
        anyhow::ensure!(
            repo_path.is_dir(),
            "local repository {} does not exist",
            repo_path.display()
        );

        let has_remote = run_git_capture(&repo_path, &["remote"], None, GIT_LOCAL_TIMEOUT)
            .map(|r| !r.trim().is_empty())
            .unwrap_or(false);

        if has_remote {
            run_git(Some(&repo_path), &["fetch", "origin"], env, GIT_NETWORK_TIMEOUT)
                .context("fetch from the configured origin failed; refusing to continue with stale or empty content")?;
        }

        let has_commits =
            run_git_capture(&repo_path, &["rev-parse", "HEAD"], None, GIT_LOCAL_TIMEOUT)
                .map(|r| !r.trim().is_empty())
                .unwrap_or(false);

        if !has_commits {
            // This is an explicit local initialization, not a fallback after a
            // failed remote clone.
            run_git(
                Some(&repo_path),
                &["checkout", "-b", base_branch],
                env,
                GIT_LOCAL_TIMEOUT,
            )?;
            run_git(
                Some(&repo_path),
                &["commit", "--allow-empty", "-m", "TurboJet: initial commit"],
                env,
                GIT_LOCAL_TIMEOUT,
            )?;
        }

        let ref_to_use = if has_remote && has_commits {
            format!("origin/{}", base_branch)
        } else {
            base_branch.to_string()
        };

        let _ = run_git(
            Some(&repo_path),
            &[
                "worktree",
                "remove",
                "--force",
                worktree_path.to_str().unwrap_or(""),
            ],
            env,
            GIT_LOCAL_TIMEOUT,
        );
        let _ = run_git(
            Some(&repo_path),
            &["branch", "-D", &branch_name],
            env,
            GIT_LOCAL_TIMEOUT,
        );

        run_git(
            Some(&repo_path),
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                worktree_path.to_str().unwrap_or(""),
                &ref_to_use,
            ],
            env,
            GIT_LOCAL_TIMEOUT,
        )?;

        tracing::info!(path = %worktree_path.display(), branch = %branch_name, base = %base_branch, remote = has_remote, "Worktree created from local repo");
        Ok(WorktreeInfo {
            worktree_path,
            branch_name,
            repo_path,
        })
    }

    pub fn has_changes(worktree_path: &Path) -> Result<bool> {
        let output = run_git_capture(
            worktree_path,
            &["status", "--porcelain"],
            None,
            GIT_LOCAL_TIMEOUT,
        )?;
        Ok(!output.trim().is_empty())
    }

    pub fn commit_all(worktree_path: &Path, message: &str) -> Result<String> {
        run_git(Some(worktree_path), &["add", "-A"], None, GIT_LOCAL_TIMEOUT)?;
        run_git(
            Some(worktree_path),
            &["commit", "-m", message],
            None,
            GIT_LOCAL_TIMEOUT,
        )?;
        Self::head_sha(worktree_path)
    }

    pub fn head_sha(worktree_path: &Path) -> Result<String> {
        Ok(run_git_capture(
            worktree_path,
            &["rev-parse", "HEAD"],
            None,
            GIT_LOCAL_TIMEOUT,
        )?
        .trim()
        .to_string())
    }

    pub fn resolve_base_sha(worktree_path: &Path, base_branch: &str) -> Result<String> {
        for candidate in [format!("origin/{base_branch}"), base_branch.to_string()] {
            if let Ok(sha) = run_git_capture(
                worktree_path,
                &["rev-parse", "--verify", &format!("{candidate}^{{commit}}")],
                None,
                GIT_LOCAL_TIMEOUT,
            ) {
                let sha = sha.trim().to_string();
                if !sha.is_empty() {
                    return Ok(sha);
                }
            }
        }
        anyhow::bail!("cannot resolve base branch '{base_branch}' for scope evidence")
    }

    /// Files changed by this task relative to `base_ref`, including staged,
    /// unstaged and untracked work. The Manager computes this before publication
    /// commits the worktree, so a committed-only `base...HEAD` diff would report
    /// no files and silently skip the approved-component scope check.
    pub fn changed_files(worktree_path: &Path, base_ref: &str) -> Result<Vec<String>> {
        let tracked = run_git_capture(
            worktree_path,
            &["diff", "--name-only", base_ref, "--"],
            None,
            GIT_LOCAL_TIMEOUT,
        )?;
        let untracked = run_git_capture(
            worktree_path,
            &["ls-files", "--others", "--exclude-standard"],
            None,
            GIT_LOCAL_TIMEOUT,
        )?;
        let mut files: Vec<String> = tracked
            .lines()
            .chain(untracked.lines())
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        files.sort();
        files.dedup();
        Ok(files)
    }

    pub fn push(worktree_path: &Path, branch: &str) -> Result<()> {
        Self::push_with_env(worktree_path, branch, None)
    }

    pub fn push_with_env(
        worktree_path: &Path,
        branch: &str,
        env: Option<&HashMap<String, String>>,
    ) -> Result<()> {
        run_git(
            Some(worktree_path),
            &["push", "-u", "origin", branch],
            env,
            GIT_NETWORK_TIMEOUT,
        )?;
        Ok(())
    }

    pub fn ls_remote_branch(
        worktree_path: &Path,
        branch: &str,
        env: Option<&HashMap<String, String>>,
    ) -> Result<Option<String>> {
        let output = run_git_capture(
            worktree_path,
            &["ls-remote", "origin", &format!("refs/heads/{}", branch)],
            env,
            GIT_NETWORK_TIMEOUT,
        )?;
        Ok(output
            .split_whitespace()
            .next()
            .filter(|sha| !sha.is_empty())
            .map(str::to_string))
    }

    pub fn cleanup(worktree_path: &Path, repo_path: &Path) -> Result<()> {
        if worktree_path.exists() {
            let _ = run_git(
                Some(repo_path),
                &[
                    "worktree",
                    "remove",
                    "--force",
                    worktree_path.to_str().unwrap_or(""),
                ],
                None,
                GIT_LOCAL_TIMEOUT,
            );
            if worktree_path.exists() {
                std::fs::remove_dir_all(worktree_path)?;
            }
        }
        tracing::info!(path = %worktree_path.display(), "Worktree cleaned up");
        Ok(())
    }
}

/// Explicit clone used by project provisioning. There is deliberately no
/// `git init` fallback here: a failed remote clone is reported, not converted
/// into a plausible-looking empty repository.
pub fn clone_repository(
    repo_url: &str,
    destination: &Path,
    branch: &str,
    env: Option<&HashMap<String, String>>,
) -> Result<()> {
    anyhow::ensure!(
        !destination.exists()
            || std::fs::read_dir(destination)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false),
        "clone destination {} is not empty",
        destination.display()
    );
    run_git(
        None,
        &[
            "clone",
            "--branch",
            branch,
            repo_url,
            destination.to_str().unwrap_or(""),
        ],
        env,
        GIT_NETWORK_TIMEOUT,
    )
    .context("remote clone failed; no empty repository was substituted")?;
    Ok(())
}

pub fn init_local_repository(destination: &Path) -> Result<()> {
    run_git(Some(destination), &["init"], None, GIT_LOCAL_TIMEOUT)
        .context("local repository initialization failed")?;
    Ok(())
}

fn base_command(dir: Option<&Path>) -> Command {
    let mut command = Command::new("git");
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    command
        // Do not consult user/system credential helpers or ambient SSH config.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS", "/bin/false")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "TurboJet")
        .env("GIT_AUTHOR_EMAIL", "turbojet@localhost")
        .env("GIT_COMMITTER_NAME", "TurboJet")
        .env("GIT_COMMITTER_EMAIL", "turbojet@localhost")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run_git(
    dir: Option<&Path>,
    args: &[&str],
    env: Option<&HashMap<String, String>>,
    timeout: Duration,
) -> Result<()> {
    let output = run_git_output(dir, args, env, timeout)?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.map(|p| p.display().to_string())
                .unwrap_or_else(|| "<cwd>".into()),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn run_git_capture(
    dir: &Path,
    args: &[&str],
    env: Option<&HashMap<String, String>>,
    timeout: Duration,
) -> Result<String> {
    let output = run_git_output(Some(dir), args, env, timeout)?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_git_output(
    dir: Option<&Path>,
    args: &[&str],
    env: Option<&HashMap<String, String>>,
    timeout: Duration,
) -> Result<Output> {
    let mut command = base_command(dir);
    command.args(args);
    if let Some(env) = env {
        command.envs(env);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to execute git {}", args.join(" ")))?;
    let stdout = child.stdout.take().context("git stdout unavailable")?;
    let stderr = child.stderr.take().context("git stderr unavailable")?;
    let stdout_reader = std::thread::spawn(move || read_all(stdout));
    let stderr_reader = std::thread::spawn(move || read_all(stderr));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            anyhow::bail!(
                "git {} timed out after {}s",
                args.join(" "),
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("git stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("git stderr reader panicked"))??;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read_all<R: Read>(mut reader: R) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader.read_to_end(&mut output)?;
    Ok(output)
}
