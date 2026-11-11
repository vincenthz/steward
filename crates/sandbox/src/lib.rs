//! Running commands under restrictions: writes only where allowed, no IP
//! network unless allowed, secrets (ssh keys, tokens, cloud credentials)
//! hidden, and a clean environment, so that a command steward runs on
//! someone else's code (a build, the tests) cannot leak or damage much.
//!
//! The backend is the system's: Seatbelt (`sandbox-exec`) on macOS,
//! bubblewrap (`bwrap`) on Linux.
//!
//! Known gaps: on macOS name resolution still works (through the system
//! resolver's socket), and everything outside the hidden paths stays readable.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use std::{fmt, fs, io};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Seatbelt,
    Bubblewrap,
    /// no isolation at all: only an explicit choice
    Unsandboxed,
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Backend::Seatbelt => "seatbelt (sandbox-exec)",
            Backend::Bubblewrap => "bubblewrap (bwrap)",
            Backend::Unsandboxed => "none",
        })
    }
}

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// The isolation this system offers
pub fn detect() -> Option<Backend> {
    if cfg!(target_os = "macos") && Path::new(SANDBOX_EXEC).is_file() {
        Some(Backend::Seatbelt)
    } else if cfg!(target_os = "linux") && find_in_path("bwrap").is_some() {
        Some(Backend::Bubblewrap)
    } else {
        None
    }
}

#[derive(Debug)]
pub enum Error {
    /// no sandbox backend on this system
    Unavailable,
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unavailable => write!(
                f,
                "no sandbox available (needs sandbox-exec on macOS, bwrap on Linux)"
            ),
            Error::Io(e) => write!(f, "sandbox: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// The variables a sandboxed command gets from the environment, besides
/// those a policy adds: none of the tokens and keys
pub const BASE_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "COLORTERM",
    "NO_COLOR",
];

/// The paths hidden by default, under the home directory
pub const SECRET_PATHS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".config/gcloud",
    ".config/gh",
    ".config/steward",
    ".config/hub",
    ".docker/config.json",
    ".kube",
    ".netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".password-store",
    "Library/Keychains",
];

#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// the only places writable (with the sandbox's temporary directory and
    /// a few devices)
    pub writable: Vec<PathBuf>,
    /// unreadable paths
    pub hidden: Vec<PathBuf>,
    pub network: bool,
    /// variables passed from the environment, besides [`BASE_ENV`]
    pub pass_env: Vec<String>,
    /// variables set
    pub env: Vec<(String, String)>,
}

impl Policy {
    /// Writes to `dir` only, no network, the usual secrets hidden
    pub fn workdir(dir: impl Into<PathBuf>) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Policy {
            writable: vec![dir.into()],
            hidden: home
                .map(|h| SECRET_PATHS.iter().map(|p| h.join(p)).collect())
                .unwrap_or_default(),
            ..Default::default()
        }
    }
}

/// Commands run under a policy
pub struct Sandbox {
    backend: Backend,
    policy: Policy,
    /// the sandbox's temporary directory, `$TMPDIR` for its commands
    tmp: PathBuf,
}

/// Absolute, with symbolic links resolved (Seatbelt matches real paths:
/// `/tmp` is `/private/tmp`). A path that doesn't exist yet is resolved
/// through its nearest existing ancestor.
fn real_path(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let mut tail = Vec::new();
    let mut cur = abs.as_path();
    loop {
        if let Ok(real) = cur.canonicalize() {
            return tail.iter().rev().fold(real, |acc, c| acc.join(c));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_owned());
                cur = parent;
            }
            _ => return abs,
        }
    }
}

impl Sandbox {
    /// A sandbox with the system's backend, its temporary directory under
    /// `scratch`
    pub fn new(policy: Policy, scratch: &Path) -> Result<Self, Error> {
        let backend = detect().ok_or(Error::Unavailable)?;
        Self::with_backend(backend, policy, scratch)
    }

    pub fn with_backend(backend: Backend, policy: Policy, scratch: &Path) -> Result<Self, Error> {
        let tmp = scratch.join(format!("tmp-{}-{}", std::process::id(), unique()));
        fs::create_dir_all(&tmp)?;
        let mut policy = policy;
        policy.writable = policy.writable.iter().map(|p| real_path(p)).collect();
        policy.hidden = policy.hidden.iter().map(|p| real_path(p)).collect();
        Ok(Sandbox {
            backend,
            policy,
            tmp: real_path(&tmp),
        })
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// `program args...` in `cwd`, wrapped by the backend, with the policy's
    /// environment only
    pub fn command<S: AsRef<std::ffi::OsStr>>(
        &self,
        program: impl AsRef<std::ffi::OsStr>,
        args: impl IntoIterator<Item = S>,
        cwd: &Path,
    ) -> Command {
        let cwd = real_path(cwd);
        let mut cmd = match self.backend {
            Backend::Seatbelt => {
                let mut c = Command::new(SANDBOX_EXEC);
                c.arg("-p").arg(self.seatbelt_profile()).arg("--");
                c.arg(program);
                c
            }
            Backend::Bubblewrap => {
                let mut c = Command::new("bwrap");
                c.args(self.bwrap_args(&cwd));
                c.arg("--").arg(program);
                c
            }
            Backend::Unsandboxed => Command::new(program),
        };
        cmd.args(args).current_dir(&cwd).env_clear();
        for var in BASE_ENV
            .iter()
            .copied()
            .chain(self.policy.pass_env.iter().map(String::as_str))
        {
            if let Some(v) = std::env::var_os(var) {
                cmd.env(var, v);
            }
        }
        cmd.env("TMPDIR", &self.tmp);
        for (k, v) in &self.policy.env {
            cmd.env(k, v);
        }
        cmd
    }

    /// The Seatbelt profile of the policy
    pub fn seatbelt_profile(&self) -> String {
        let mut p = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
        p.push_str("(allow file-write*\n");
        for w in self.policy.writable.iter().chain([&self.tmp]) {
            p.push_str(&format!("  (subpath {})\n", sbpl_string(w)));
        }
        p.push_str(
            "  (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/tty\")\n  \
             (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/fd/\"))\n",
        );
        if !self.policy.hidden.is_empty() {
            p.push_str("(deny file-read* file-write*\n");
            for h in &self.policy.hidden {
                p.push_str(&format!("  (subpath {})\n", sbpl_string(h)));
            }
            p.push_str(")\n");
        }
        if !self.policy.network {
            // IP only: local (unix) sockets keep working
            p.push_str("(deny network-outbound (remote ip))\n(deny network-inbound (local ip))\n");
        }
        p
    }

    /// The bubblewrap arguments of the policy
    pub fn bwrap_args(&self, cwd: &Path) -> Vec<String> {
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let mut a: Vec<String> = [
            "--die-with-parent",
            "--new-session",
            "--unshare-pid",
            "--unshare-ipc",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
        ]
        .map(String::from)
        .into();
        if !self.policy.network {
            a.push("--unshare-net".into());
        }
        for w in self.policy.writable.iter().chain([&self.tmp]) {
            if w.exists() {
                a.extend(["--bind".into(), s(w), s(w)]);
            }
        }
        for h in &self.policy.hidden {
            if h.is_dir() {
                a.extend(["--tmpfs".into(), s(h)]);
            } else if h.exists() {
                a.extend(["--ro-bind".into(), "/dev/null".into(), s(h)]);
            }
        }
        a.extend(["--chdir".into(), s(cwd)]);
        a
    }

    /// Runs `cmd` with its output captured (each stream up to
    /// `max_output` bytes), killing it and its children after `timeout`
    pub fn run(
        &self,
        mut cmd: Command,
        timeout: Option<Duration>,
        max_output: usize,
    ) -> Result<Outcome, Error> {
        use std::os::unix::process::CommandExt;
        let start = Instant::now();
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = cmd.spawn()?;
        let pid = child.id() as libc::pid_t;
        let reader = |r: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut truncated = false;
                if let Some(mut r) = r {
                    let mut chunk = [0u8; 8192];
                    while let Ok(n) = r.read(&mut chunk) {
                        if n == 0 {
                            break;
                        }
                        let room = max_output.saturating_sub(buf.len());
                        truncated |= n > room;
                        buf.extend_from_slice(&chunk[..n.min(room)]);
                    }
                }
                (buf, truncated)
            })
        };
        let out = reader(child.stdout.take().map(|s| Box::new(s) as _));
        let err = reader(child.stderr.take().map(|s| Box::new(s) as _));
        let mut timed_out = false;
        let status = loop {
            if let Some(st) = child.try_wait()? {
                break st;
            }
            if timeout.is_some_and(|t| start.elapsed() >= t) {
                timed_out = true;
                // SAFETY: signals the child's process group, which it leads
                unsafe { libc::kill(-pid, libc::SIGKILL) };
                break child.wait()?;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let (stdout, out_trunc) = out.join().unwrap_or_default();
        let (stderr, err_trunc) = err.join().unwrap_or_default();
        Ok(Outcome {
            status,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            truncated: out_trunc || err_trunc,
            timed_out,
            elapsed: start.elapsed(),
        })
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.tmp);
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    /// some output was dropped
    pub truncated: bool,
    pub timed_out: bool,
    pub elapsed: Duration,
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// A string literal of the Seatbelt profile language
fn sbpl_string(p: &Path) -> String {
    let s = p.to_string_lossy();
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("steward-sandbox-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn quoting() {
        assert_eq!(sbpl_string(Path::new("/a \"b\"\\c")), r#""/a \"b\"\\c""#);
    }

    #[test]
    fn real_paths() {
        let d = scratch("real");
        let real = d.canonicalize().unwrap();
        assert_eq!(real_path(&d.join("not/yet")), real.join("not/yet"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn environment_is_clean() {
        let d = scratch("env");
        let mut p = Policy::workdir(&d);
        p.env.push(("STEWARD_TEST".into(), "1".into()));
        let sb = Sandbox::with_backend(Backend::Unsandboxed, p, &d).unwrap();
        // SAFETY: tests of this crate don't read this variable concurrently
        unsafe { std::env::set_var("STEWARD_SECRET_TEST", "x") };
        let out = sb
            .run(
                sb.command("/usr/bin/env", [] as [&str; 0], &d),
                None,
                1 << 16,
            )
            .unwrap();
        assert!(out.stdout.contains("STEWARD_TEST=1"));
        assert!(out.stdout.contains("TMPDIR="));
        assert!(!out.stdout.contains("STEWARD_SECRET_TEST"));
        drop(sb);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn timeout_kills_the_group() {
        let d = scratch("timeout");
        let sb = Sandbox::with_backend(Backend::Unsandboxed, Policy::workdir(&d), &d).unwrap();
        let cmd = sb.command("/bin/sh", ["-c", "sleep 30 & sleep 30"], &d);
        let out = sb.run(cmd, Some(Duration::from_millis(200)), 1024).unwrap();
        assert!(out.timed_out);
        assert!(out.elapsed < Duration::from_secs(5));
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_confines() {
        let d = scratch("seatbelt");
        let work = d.join("work");
        let secret = d.join("secret");
        let outside = d.join("outside");
        fs::create_dir_all(&work).unwrap();
        fs::create_dir_all(&secret).unwrap();
        fs::write(secret.join("key"), "k").unwrap();
        let mut p = Policy::workdir(&work);
        p.hidden.push(secret.clone());
        let sb = Sandbox::with_backend(Backend::Seatbelt, p, &d).unwrap();
        let script = format!(
            "echo in > in && echo $TMPDIR && echo t > \"$TMPDIR/t\" && \
             (echo out > {o} || echo write-denied) && (cat {k} || echo read-denied)",
            o = outside.display(),
            k = secret.join("key").display()
        );
        let out = sb
            .run(sb.command("/bin/sh", ["-c", &script], &work), None, 1 << 16)
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(work.join("in").is_file());
        assert!(!outside.exists());
        assert!(out.stdout.contains("write-denied"), "{out:?}");
        assert!(out.stdout.contains("read-denied"), "{out:?}");
        drop(sb);
        fs::remove_dir_all(&d).unwrap();
    }
}
