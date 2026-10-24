//! TJ-034: narrow, fail-closed execution isolation for model-driven shell
//! tools on Linux.
//!
//! The boundary is deliberately small and inspectable:
//!
//! * Landlock restricts filesystem access to the run workspace/output directory
//!   plus read-only toolchain/system paths. Home directories, SSH material and
//!   unrelated host files are not reachable.
//! * Landlock network rules deny TCP bind/connect. A seccomp deny-list also
//!   blocks socket creation, namespace/mount operations, ptrace, keyrings and
//!   other privileged escape primitives.
//! * The child inherits a non-root identity and an explicit environment
//!   allow-list; no ambient credentials or `GIT_SSH_COMMAND` pass through.
//! * Resource limits bound CPU, address space, file size, open files, processes
//!   and core dumps.
//! * The existing process-group supervisor terminates descendants and the
//!   stream reader caps captured output.
//!
//! A worktree remains a branch/work-file boundary, never a sandbox. If the
//! host cannot provide Landlock + seccomp + the required limits, shell
//! execution fails closed instead of running unconfined.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const MAX_STREAM_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxStatus {
    pub available: bool,
    pub backend: &'static str,
    pub abi: i64,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IsolationPolicy {
    pub workspace: PathBuf,
    pub output_dir: PathBuf,
    pub read_only_paths: Vec<PathBuf>,
    pub cpu_seconds: u64,
    pub address_space_bytes: u64,
    pub file_size_bytes: u64,
    pub open_files: u64,
    pub processes: u64,
}

impl IsolationPolicy {
    pub fn new(workspace: impl Into<PathBuf>, output_dir: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            output_dir: output_dir.into(),
            read_only_paths: vec![
                PathBuf::from("/usr"),
                PathBuf::from("/bin"),
                PathBuf::from("/sbin"),
                PathBuf::from("/lib"),
                PathBuf::from("/lib64"),
                PathBuf::from("/etc/ld.so.cache"),
                PathBuf::from("/etc/ld.so.conf"),
                PathBuf::from("/etc/ld.so.conf.d"),
                PathBuf::from("/etc/ssl"),
                PathBuf::from("/etc/ca-certificates"),
                PathBuf::from("/etc/nsswitch.conf"),
                PathBuf::from("/etc/hosts"),
                PathBuf::from("/etc/resolv.conf"),
            ],
            // Generous enough for a normal build/test command, still bounded.
            cpu_seconds: 900,
            address_space_bytes: 4 * 1024 * 1024 * 1024,
            file_size_bytes: 1024 * 1024 * 1024,
            open_files: 512,
            processes: 4096,
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::os::unix::process::CommandExt;

    const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
    const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

    const ACCESS_FS_EXECUTE: u64 = 1 << 0;
    const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
    const ACCESS_FS_READ_FILE: u64 = 1 << 2;
    const ACCESS_FS_READ_DIR: u64 = 1 << 3;
    const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
    const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
    const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
    const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
    const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
    const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
    const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
    const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
    const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
    const ACCESS_FS_REFER: u64 = 1 << 13;
    const ACCESS_FS_TRUNCATE: u64 = 1 << 14;
    const ACCESS_FS_IOCTL_DEV: u64 = 1 << 15;

    const ACCESS_NET_BIND_TCP: u64 = 1 << 0;
    const ACCESS_NET_CONNECT_TCP: u64 = 1 << 1;

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
        handled_access_net: u64,
        scoped: u64,
    }

    #[repr(C)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    const PR_SET_NO_NEW_PRIVS: i32 = 38;
    const PR_SET_PDEATHSIG: i32 = 1;
    const SECCOMP_SET_MODE_FILTER: u32 = 1;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
    #[cfg(target_arch = "x86_64")]
    const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const AUDIT_ARCH_AARCH64: u32 = 0xc000_00b7;

    #[repr(C)]
    #[derive(Clone)]
    struct SockFilter {
        code: u16,
        jt: u8,
        jf: u8,
        k: u32,
    }

    #[repr(C)]
    struct SockFprog {
        len: u16,
        filter: *const SockFilter,
    }

    const BPF_LD: u16 = 0x00;
    const BPF_W: u16 = 0x00;
    const BPF_ABS: u16 = 0x20;
    const BPF_JMP: u16 = 0x05;
    const BPF_JEQ: u16 = 0x10;
    const BPF_K: u16 = 0x00;
    const BPF_RET: u16 = 0x06;

    fn bpf_stmt(code: u16, k: u32) -> SockFilter {
        SockFilter {
            code,
            jt: 0,
            jf: 0,
            k,
        }
    }
    fn bpf_jump(code: u16, k: u32, jt: u8, jf: u8) -> SockFilter {
        SockFilter { code, jt, jf, k }
    }

    fn deny(errno: u32) -> u32 {
        SECCOMP_RET_ERRNO | (errno & 0xffff)
    }

    fn expected_arch() -> Option<u32> {
        #[cfg(target_arch = "x86_64")]
        {
            Some(AUDIT_ARCH_X86_64)
        }
        #[cfg(target_arch = "aarch64")]
        {
            Some(AUDIT_ARCH_AARCH64)
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            None
        }
    }

    fn denied_syscalls() -> Vec<i64> {
        let mut calls = vec![
            libc::SYS_socket as i64,
            libc::SYS_socketpair as i64,
            libc::SYS_connect as i64,
            libc::SYS_bind as i64,
            libc::SYS_listen as i64,
            libc::SYS_accept as i64,
            libc::SYS_accept4 as i64,
            libc::SYS_sendto as i64,
            libc::SYS_recvfrom as i64,
            libc::SYS_sendmsg as i64,
            libc::SYS_recvmsg as i64,
            libc::SYS_setsid as i64,
            libc::SYS_unshare as i64,
            libc::SYS_setns as i64,
            libc::SYS_mount as i64,
            libc::SYS_umount2 as i64,
            libc::SYS_pivot_root as i64,
            libc::SYS_chroot as i64,
            libc::SYS_ptrace as i64,
            libc::SYS_bpf as i64,
            libc::SYS_perf_event_open as i64,
            libc::SYS_keyctl as i64,
            libc::SYS_add_key as i64,
            libc::SYS_request_key as i64,
            libc::SYS_process_vm_readv as i64,
            libc::SYS_process_vm_writev as i64,
            libc::SYS_open_by_handle_at as i64,
            libc::SYS_name_to_handle_at as i64,
            libc::SYS_init_module as i64,
            libc::SYS_finit_module as i64,
            libc::SYS_delete_module as i64,
            libc::SYS_kexec_load as i64,
            libc::SYS_kexec_file_load as i64,
            libc::SYS_reboot as i64,
            libc::SYS_swapon as i64,
            libc::SYS_swapoff as i64,
            libc::SYS_userfaultfd as i64,
            libc::SYS_io_uring_setup as i64,
            libc::SYS_io_uring_enter as i64,
            libc::SYS_io_uring_register as i64,
            libc::SYS_open_tree as i64,
            libc::SYS_move_mount as i64,
            libc::SYS_fsopen as i64,
            libc::SYS_fsconfig as i64,
            libc::SYS_fsmount as i64,
            libc::SYS_mount_setattr as i64,
            libc::SYS_landlock_create_ruleset as i64,
            libc::SYS_landlock_add_rule as i64,
            libc::SYS_landlock_restrict_self as i64,
        ];
        calls.sort_unstable();
        calls.dedup();
        calls
    }

    fn seccomp_filter() -> Vec<SockFilter> {
        let mut filter = Vec::new();
        // Validate the architecture the syscall numbers were compiled for.
        filter.push(bpf_stmt(BPF_LD | BPF_W | BPF_ABS, 4));
        match expected_arch() {
            Some(arch) => {
                filter.push(bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, arch, 1, 0));
                filter.push(bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
            }
            None => filter.push(bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS)),
        }
        filter.push(bpf_stmt(BPF_LD | BPF_W | BPF_ABS, 0));
        for nr in denied_syscalls() {
            filter.push(bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, nr as u32, 0, 1));
            filter.push(bpf_stmt(BPF_RET | BPF_K, deny(libc::EPERM as u32)));
        }
        filter.push(bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
        filter
    }

    fn landlock_abi() -> io::Result<i64> {
        // SAFETY: version probing passes a null attribute pointer and size 0.
        let abi = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                LANDLOCK_CREATE_RULESET_VERSION as u32,
            )
        };
        if abi < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(abi)
        }
    }

    fn fs_mask(abi: i64) -> u64 {
        let mut mask = ACCESS_FS_EXECUTE
            | ACCESS_FS_WRITE_FILE
            | ACCESS_FS_READ_FILE
            | ACCESS_FS_READ_DIR
            | ACCESS_FS_REMOVE_DIR
            | ACCESS_FS_REMOVE_FILE
            | ACCESS_FS_MAKE_CHAR
            | ACCESS_FS_MAKE_DIR
            | ACCESS_FS_MAKE_REG
            | ACCESS_FS_MAKE_SOCK
            | ACCESS_FS_MAKE_FIFO
            | ACCESS_FS_MAKE_BLOCK
            | ACCESS_FS_MAKE_SYM;
        if abi >= 2 {
            mask |= ACCESS_FS_REFER;
        }
        if abi >= 3 {
            mask |= ACCESS_FS_TRUNCATE;
        }
        if abi >= 5 {
            mask |= ACCESS_FS_IOCTL_DEV;
        }
        mask
    }

    fn read_only_mask(mask: u64) -> u64 {
        // EXECUTE is required for interpreters/binaries and their loader.
        mask & (ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR | ACCESS_FS_EXECUTE)
    }

    fn add_path_rule(fd: RawFd, path: &Path, access: u64) -> io::Result<()> {
        if access == 0 {
            return Ok(());
        }
        let bytes = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        // SAFETY: O_PATH opens the path without following a final symlink to
        // anything executable; Landlock evaluates the opened inode.
        let parent = unsafe { libc::open(bytes.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if parent < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(());
            }
            return Err(error);
        }
        // SAFETY: `parent` is an owned descriptor returned above.
        let parent = unsafe { OwnedFd::from_raw_fd(parent) };
        // Directory-only rights (READ_DIR and the MAKE/REMOVE family) are
        // invalid on regular files/devices and would make the whole ruleset
        // fail with EINVAL. Restrict file rules to file-level access.
        let is_directory = std::fs::metadata(path)
            .map(|meta| meta.is_dir())
            .unwrap_or(false);
        let mut access = access;
        if !is_directory {
            access &= ACCESS_FS_READ_FILE
                | ACCESS_FS_WRITE_FILE
                | ACCESS_FS_EXECUTE
                | ACCESS_FS_TRUNCATE
                | ACCESS_FS_IOCTL_DEV;
        }
        if access == 0 {
            return Ok(());
        }
        let attr = PathBeneathAttr {
            allowed_access: access,
            parent_fd: parent.as_raw_fd(),
        };
        // SAFETY: the ruleset and path descriptor are valid; attr has the
        // kernel-declared layout for the current ABI.
        let result = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                fd,
                LANDLOCK_RULE_PATH_BENEATH as i32,
                &attr as *const PathBeneathAttr,
                0u32,
            )
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(super) fn status() -> SandboxStatus {
        if unsafe { libc::geteuid() } == 0 {
            return SandboxStatus {
                available: false,
                backend: "landlock+seccomp",
                abi: 0,
                reason: Some("refusing to run model shell commands as root".into()),
            };
        }
        if expected_arch().is_none() {
            return SandboxStatus {
                available: false,
                backend: "landlock+seccomp",
                abi: 0,
                reason: Some("unsupported CPU architecture for the seccomp filter".into()),
            };
        }
        match landlock_abi() {
            Ok(abi) if abi >= 4 => SandboxStatus {
                available: true,
                backend: "landlock+seccomp",
                abi,
                reason: None,
            },
            Ok(abi) => SandboxStatus {
                available: false,
                backend: "landlock+seccomp",
                abi,
                reason: Some(format!(
                    "Landlock ABI {abi} lacks network rules (need ABI >= 4)"
                )),
            },
            Err(error) => SandboxStatus {
                available: false,
                backend: "landlock+seccomp",
                abi: 0,
                reason: Some(format!("Landlock unavailable: {error}")),
            },
        }
    }

    pub(super) fn prepare(policy: &IsolationPolicy) -> anyhow::Result<LinuxSandbox> {
        let status = status();
        anyhow::ensure!(
            status.available,
            "{}",
            status
                .reason
                .unwrap_or_else(|| "sandbox unavailable".into())
        );
        let abi = status.abi;
        let fs = fs_mask(abi);
        let ruleset_attr = RulesetAttr {
            handled_access_fs: fs,
            handled_access_net: ACCESS_NET_BIND_TCP | ACCESS_NET_CONNECT_TCP,
            scoped: 0,
        };
        let size = if abi >= 6 {
            std::mem::size_of::<RulesetAttr>()
        } else if abi >= 4 {
            16usize
        } else {
            8usize
        };
        // SAFETY: passes a correctly sized ruleset attribute for the probed ABI.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &ruleset_attr as *const RulesetAttr,
                size,
                0u32,
            )
        };
        if fd < 0 {
            return Err(anyhow::anyhow!(
                "landlock_create_ruleset failed: {}",
                io::Error::last_os_error()
            ));
        }
        // SAFETY: fd is newly returned by landlock_create_ruleset.
        let fd = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
        let raw = fd.as_raw_fd();

        let workspace = canonical_existing(&policy.workspace, "workspace")?;
        let output_dir = canonical_existing(&policy.output_dir, "output directory")?;
        add_path_rule(raw, &workspace, fs)
            .map_err(|error| anyhow::anyhow!("workspace rule {}: {error}", workspace.display()))?;
        if output_dir != workspace {
            add_path_rule(raw, &output_dir, fs).map_err(|error| {
                anyhow::anyhow!("output rule {}: {error}", output_dir.display())
            })?;
        }
        // A private temp directory inside the workspace, never host /tmp.
        let tmp = workspace.join(".tj-tmp");
        std::fs::create_dir_all(&tmp)?;
        add_path_rule(raw, &tmp, fs)
            .map_err(|error| anyhow::anyhow!("temp rule {}: {error}", tmp.display()))?;
        for path in &policy.read_only_paths {
            add_path_rule(raw, path, read_only_mask(fs))
                .map_err(|error| anyhow::anyhow!("read-only rule {}: {error}", path.display()))?;
        }
        for special in ["/dev/null", "/dev/zero", "/dev/urandom", "/dev/random"] {
            add_path_rule(
                raw,
                Path::new(special),
                ACCESS_FS_READ_FILE | ACCESS_FS_WRITE_FILE,
            )
            .map_err(|error| anyhow::anyhow!("device rule {special}: {error}"))?;
        }

        let filter = seccomp_filter();
        if filter.len() > u16::MAX as usize {
            return Err(anyhow::anyhow!("seccomp filter is too large"));
        }
        Ok(LinuxSandbox {
            fd,
            filter,
            policy: policy.clone(),
            abi,
        })
    }

    fn canonical_existing(path: &Path, label: &str) -> anyhow::Result<PathBuf> {
        anyhow::ensure!(
            path.is_absolute(),
            "{label} must be absolute: {}",
            path.display()
        );
        Ok(path.canonicalize().map_err(|error| {
            anyhow::anyhow!("cannot canonicalize {label} {}: {error}", path.display())
        })?)
    }

    pub(super) struct LinuxSandbox {
        fd: OwnedFd,
        filter: Vec<SockFilter>,
        policy: IsolationPolicy,
        pub abi: i64,
    }

    impl LinuxSandbox {
        /// Configure a child before exec. All data used by the child was
        /// allocated in the parent; the closure only makes syscalls.
        pub(super) fn configure(&self, command: &mut Command) {
            let fd = self.fd.as_raw_fd();
            let filter = self.filter.clone();
            let policy = self.policy.clone();
            command.env_clear();
            command
                .env("PATH", "/usr/local/bin:/usr/bin:/bin")
                .env("HOME", &policy.workspace)
                .env("TMPDIR", policy.workspace.join(".tj-tmp"))
                .env("TMP", policy.workspace.join(".tj-tmp"))
                .env("LANG", "C.UTF-8")
                .env("LC_ALL", "C.UTF-8")
                .env("TERM", "dumb")
                .env("TZ", "UTC");
            // SAFETY: the closure only performs async-signal-safe syscalls on
            // values captured in the parent before fork.
            unsafe {
                command.pre_exec(move || {
                    apply_limits(&policy)?;
                    // Die with the API/Manager process; the process-group
                    // supervisor additionally terminates descendants.
                    if libc::prctl(PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    // Landlock and seccomp require no_new_privs before the
                    // restrictions are installed.
                    if libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    // SAFETY: restrictions are irreversible and applied before
                    // exec; failure means the child must not run unconfined.
                    if libc::syscall(libc::SYS_landlock_restrict_self, fd, 0u32) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let prog = SockFprog {
                        len: filter.len() as u16,
                        filter: filter.as_ptr(),
                    };
                    if libc::syscall(
                        libc::SYS_seccomp,
                        SECCOMP_SET_MODE_FILTER,
                        0u32,
                        &prog as *const SockFprog,
                    ) != 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
    }

    fn limit(resource: libc::__rlimit_resource_t, value: u64) -> io::Result<()> {
        let rlimit = libc::rlimit {
            rlim_cur: value as libc::rlim_t,
            rlim_max: value as libc::rlim_t,
        };
        // SAFETY: setrlimit is async-signal-safe and the struct is valid.
        if unsafe { libc::setrlimit(resource, &rlimit) } != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn apply_limits(policy: &IsolationPolicy) -> io::Result<()> {
        limit(libc::RLIMIT_CORE, 0)?;
        limit(libc::RLIMIT_CPU, policy.cpu_seconds)?;
        limit(libc::RLIMIT_AS, policy.address_space_bytes)?;
        limit(libc::RLIMIT_FSIZE, policy.file_size_bytes)?;
        limit(libc::RLIMIT_NOFILE, policy.open_files)?;
        limit(libc::RLIMIT_NPROC, policy.processes)?;
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod linux {
    use super::*;
    pub(super) struct LinuxSandbox {
        pub abi: i64,
    }
    impl LinuxSandbox {
        pub(super) fn configure(&self, _command: &mut Command) {}
    }
    pub(super) fn status() -> SandboxStatus {
        SandboxStatus {
            available: false,
            backend: "none",
            abi: 0,
            reason: Some("TJ-034 supports Linux only".into()),
        }
    }
    pub(super) fn prepare(_policy: &IsolationPolicy) -> anyhow::Result<LinuxSandbox> {
        anyhow::bail!("TJ-034 execution isolation requires Linux")
    }
}

#[cfg(target_os = "linux")]
pub struct LinuxSandbox(linux::LinuxSandbox);
#[cfg(not(target_os = "linux"))]
pub struct LinuxSandbox(linux::LinuxSandbox);

impl LinuxSandbox {
    pub fn status() -> SandboxStatus {
        linux::status()
    }

    pub fn prepare(policy: &IsolationPolicy) -> anyhow::Result<Self> {
        Ok(Self(linux::prepare(policy)?))
    }

    pub fn abi(&self) -> i64 {
        self.0.abi
    }

    pub fn configure_command(&self, command: &mut Command) {
        #[cfg(target_os = "linux")]
        self.0.configure(command);
        #[cfg(not(target_os = "linux"))]
        let _ = command;
    }
}

/// Human-readable prerequisite description used by API diagnostics/tests.
pub fn describe_prerequisite() -> String {
    let status = LinuxSandbox::status();
    match status.reason {
        Some(reason) if !status.available => format!("BLOCKED: {} ({reason})", status.backend),
        _ => format!("available: {} ABI {}", status.backend, status.abi),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn sandbox_reports_a_concrete_backend_or_reason() {
        let status = LinuxSandbox::status();
        assert!(status.backend == "landlock+seccomp" || status.backend == "none");
        if !status.available {
            assert!(status.reason.is_some());
        }
        assert!(!describe_prerequisite().is_empty());
    }

    #[test]
    fn policy_starts_with_documented_limits() {
        let dir = tempfile::tempdir().unwrap();
        let policy = IsolationPolicy::new(dir.path(), dir.path());
        assert_eq!(policy.cpu_seconds, 900);
        assert_eq!(policy.open_files, 512);
        assert!(policy.address_space_bytes >= 1024 * 1024 * 1024);
        assert!(!policy.read_only_paths.is_empty());
    }

    #[test]
    fn workspace_must_exist_before_preparation() {
        let dir = tempfile::tempdir().unwrap();
        let policy = IsolationPolicy::new(dir.path().join("missing"), dir.path().join("missing"));
        let error = LinuxSandbox::prepare(&policy).err().map(|e| e.to_string());
        if LinuxSandbox::status().available {
            assert!(error.unwrap().contains("workspace"));
        }
    }

    #[test]
    fn environment_allow_list_does_not_include_ambient_secrets() {
        let dir = tempfile::tempdir().unwrap();
        if !LinuxSandbox::status().available {
            return;
        }
        let sandbox = LinuxSandbox::prepare(&IsolationPolicy::new(dir.path(), dir.path())).unwrap();
        let mut command = Command::new("env");
        sandbox.configure_command(&mut command);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.lines()
                .all(|line| !line.starts_with("GIT_SSH_COMMAND="))
        );
        assert!(
            text.lines()
                .all(|line| !line.starts_with("AWS_SECRET_ACCESS_KEY="))
        );
        assert!(text.contains("HOME="));
        std::fs::write(dir.path().join("ok"), b"ok").unwrap();
    }

    #[test]
    fn path_rules_reject_missing_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let policy = IsolationPolicy::new(dir.path().join("nope"), dir.path());
        if LinuxSandbox::status().available {
            assert!(LinuxSandbox::prepare(&policy).is_err());
        }
    }

    #[test]
    fn output_cap_is_bounded() {
        assert_eq!(MAX_STREAM_BYTES, 1024 * 1024);
        let mut sink = Vec::new();
        sink.write_all(b"ok").unwrap();
    }
}
