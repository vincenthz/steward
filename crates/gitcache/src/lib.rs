//! A cache of git repositories, kept as bare clones (`<root>/<host>/<owner>/<name>.git`)
//! and checked out as worktrees, so that working on a repository again only
//! fetches what changed, and several checkouts share one object store.
//!
//! It runs the `git` command. Credentials go through the environment
//! (`GIT_CONFIG_*`), never through the command line nor the repository's
//! configuration, and git never prompts.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;
use std::{fmt, io};

use base64::Engine as _;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// git failed
    Git {
        args: String,
        stderr: String,
    },
    InvalidKey(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "git cache: {e}"),
            Error::Git { args, stderr } => write!(f, "git {args}: {}", stderr.trim()),
            Error::InvalidKey(k) => write!(f, "git cache: invalid repository key `{k}`"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// An HTTP header git sends to the URLs under a prefix
#[derive(Clone)]
pub struct Auth {
    url_prefix: String,
    header: String,
}

impl Auth {
    /// A GitHub token, for `https://HOST/`
    pub fn github_token(host: &str, token: &str) -> Self {
        let basic =
            base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
        Auth {
            url_prefix: format!("https://{host}/"),
            header: format!("Authorization: Basic {basic}"),
        }
    }
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth")
            .field("url_prefix", &self.url_prefix)
            .finish_non_exhaustive()
    }
}

/// Where a repository comes from, and its place in the cache
#[derive(Debug, Clone)]
pub struct Remote {
    pub url: String,
    /// relative path in the cache, without `.git`: `github.com/owner/name`
    pub key: String,
    pub auth: Option<Auth>,
}

pub struct Cache {
    root: PathBuf,
}

/// A repository of the cache
#[derive(Debug, Clone)]
pub struct Bare {
    pub key: String,
    pub path: PathBuf,
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('/')
        && key
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != ".." && !s.starts_with('-'))
}

impl Cache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Cache { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_of(&self, key: &str) -> Result<PathBuf, Error> {
        if !valid_key(key) {
            return Err(Error::InvalidKey(key.into()));
        }
        Ok(self.root.join(format!("{key}.git")))
    }

    /// The repository, if cached
    pub fn get(&self, key: &str) -> Result<Option<Bare>, Error> {
        let path = self.path_of(key)?;
        Ok(path.join("HEAD").is_file().then(|| Bare {
            key: key.into(),
            path,
        }))
    }

    /// Clones the repository if it is not cached, else fetches it
    pub fn sync(&self, remote: &Remote) -> Result<Bare, Error> {
        let path = self.path_of(&remote.key)?;
        fs::create_dir_all(path.parent().expect("a key has a parent"))?;
        let _lock = lock(&path)?;
        let bare = Bare {
            key: remote.key.clone(),
            path: path.clone(),
        };
        if path.join("HEAD").is_file() {
            bare.fetch(remote.auth.as_ref(), &[])?;
            return Ok(bare);
        }
        // clone next to it and move it in place, so that an interrupted clone
        // never looks like a repository
        let tmp = path.with_extension("git.partial");
        if tmp.exists() {
            fs::remove_dir_all(&tmp)?;
        }
        let mut cmd = git(None, remote.auth.as_ref());
        cmd.args(["clone", "--bare", "--quiet", "--"])
            .arg(&remote.url)
            .arg(&tmp);
        if let Err(e) = run(cmd) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
        let partial = Bare {
            key: remote.key.clone(),
            path: tmp.clone(),
        };
        // a bare clone has no fetch refspec: mirror branches and tags
        partial.git(&[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/heads/*",
        ])?;
        partial.git(&[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/tags/*:refs/tags/*",
        ])?;
        partial.git(&["config", "gc.auto", "0"])?;
        fs::rename(&tmp, &path)?;
        Ok(bare)
    }

    /// Every cached repository
    pub fn list(&self) -> Result<Vec<Bare>, Error> {
        let mut out = Vec::new();
        if self.root.is_dir() {
            self.walk(&self.root, &mut out)?;
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    fn walk(&self, dir: &Path, out: &mut Vec<Bare>) -> Result<(), Error> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if !path.is_dir() {
                continue;
            }
            if path.extension() == Some(OsStr::new("git")) && path.join("HEAD").is_file() {
                let rel = path.strip_prefix(&self.root).expect("under the root");
                let key = rel.with_extension("").to_string_lossy().replace('\\', "/");
                out.push(Bare { key, path });
            } else if path.extension() != Some(OsStr::new("partial")) {
                self.walk(&path, out)?;
            }
        }
        Ok(())
    }

    /// Removes a repository from the cache, with its worktrees' records
    /// (not the worktrees themselves)
    pub fn remove(&self, key: &str) -> Result<bool, Error> {
        let path = self.path_of(key)?;
        if !path.exists() {
            return Ok(false);
        }
        let _lock = lock(&path)?;
        fs::remove_dir_all(&path)?;
        Ok(true)
    }
}

/// Serializes the processes using one repository of the cache
fn lock(repo: &Path) -> Result<File, Error> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(repo.with_extension("git.lock"))?;
    file.lock()?;
    Ok(file)
}

/// `git`, never prompting, with `auth` given through the environment
fn git(dir: Option<&Path>, auth: Option<&Auth>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.arg("--git-dir").arg(d);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .stdin(Stdio::null());
    if let Some(a) = auth {
        cmd.env("GIT_CONFIG_COUNT", "1")
            .env(
                "GIT_CONFIG_KEY_0",
                format!("http.{}.extraheader", a.url_prefix),
            )
            .env("GIT_CONFIG_VALUE_0", &a.header);
    }
    cmd
}

/// Runs a git command, returning its standard output
fn run(mut cmd: Command) -> Result<String, Error> {
    let out = cmd.output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let args = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        Err(Error::Git {
            args,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

impl Bare {
    /// Runs git in this repository
    pub fn git(&self, args: &[&str]) -> Result<String, Error> {
        let mut cmd = git(Some(&self.path), None);
        cmd.args(args);
        run(cmd)
    }

    /// Fetches the branches and tags, and `refspecs`
    pub fn fetch(&self, auth: Option<&Auth>, refspecs: &[&str]) -> Result<(), Error> {
        let mut cmd = git(Some(&self.path), auth);
        cmd.args(["fetch", "--quiet", "--prune", "origin"]);
        if !refspecs.is_empty() {
            // explicit refspecs replace the configured ones
            cmd.args(["+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*"]);
            cmd.args(refspecs);
        }
        run(cmd).map(drop)
    }

    /// Fetches the head of a GitHub pull request to `refs/pull/N/head`, and
    /// returns its commit
    pub fn fetch_pull(&self, auth: Option<&Auth>, number: u64) -> Result<String, Error> {
        let spec = format!("+refs/pull/{number}/head:refs/pull/{number}/head");
        let mut cmd = git(Some(&self.path), auth);
        cmd.args(["fetch", "--quiet", "origin", &spec]);
        run(cmd)?;
        self.rev_parse(&format!("refs/pull/{number}/head"))
    }

    /// The commit a revision names
    pub fn rev_parse(&self, rev: &str) -> Result<String, Error> {
        Ok(self
            .git(&[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{rev}^{{commit}}"),
            ])?
            .trim()
            .to_string())
    }

    /// The branch HEAD points to: the remote's default branch
    pub fn default_branch(&self) -> Option<String> {
        let r = self.git(&["symbolic-ref", "--short", "HEAD"]).ok()?;
        Some(r.trim().to_string()).filter(|r| !r.is_empty())
    }

    /// When the repository was last cloned or fetched
    pub fn last_sync(&self) -> Option<SystemTime> {
        ["FETCH_HEAD", "packed-refs", "HEAD"].iter().find_map(|f| {
            fs::metadata(self.path.join(f))
                .and_then(|m| m.modified())
                .ok()
        })
    }

    /// The size on disk
    pub fn disk_size(&self) -> u64 {
        fn size(p: &Path) -> u64 {
            fs::read_dir(p).map_or(0, |rd| {
                rd.flatten()
                    .map(|e| match e.file_type() {
                        Ok(t) if t.is_dir() => size(&e.path()),
                        Ok(_) => e.metadata().map_or(0, |m| m.len()),
                        Err(_) => 0,
                    })
                    .sum()
            })
        }
        size(&self.path)
    }

    /// Checks `rev` out at `path` as a worktree with a detached HEAD, or on
    /// a new branch `branch` starting there
    pub fn add_worktree(&self, path: &Path, rev: &str, branch: Option<&str>) -> Result<(), Error> {
        let commit = self.rev_parse(rev)?;
        let mut cmd = git(Some(&self.path), None);
        cmd.args(["worktree", "add", "--quiet"]);
        match branch {
            Some(b) => cmd.args(["-b", b]),
            None => cmd.arg("--detach"),
        };
        cmd.arg(path).arg(&commit);
        run(cmd).map(drop)
    }

    pub fn remove_worktree(&self, path: &Path, force: bool) -> Result<(), Error> {
        let mut cmd = git(Some(&self.path), None);
        cmd.args(["worktree", "remove"]);
        if force {
            cmd.arg("--force");
        }
        cmd.arg(path);
        run(cmd).map(drop)
    }

    /// The worktrees' paths, forgetting those deleted
    pub fn worktrees(&self) -> Result<Vec<PathBuf>, Error> {
        self.git(&["worktree", "prune"])?;
        let out = self.git(&["worktree", "list", "--porcelain"])?;
        // one block per worktree, the repository itself marked `bare`
        Ok(out
            .split("\n\n")
            .filter(|block| !block.lines().any(|l| l == "bare"))
            .filter_map(|block| block.lines().find_map(|l| l.strip_prefix("worktree ")))
            .map(PathBuf::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("steward-gitcache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn keys() {
        assert!(valid_key("github.com/o/r"));
        for k in ["", "/abs", "a/../b", "a//b", "-x/y"] {
            assert!(!valid_key(k), "{k}");
        }
    }

    #[test]
    fn clone_fetch_worktree() {
        let d = scratch("cycle");
        let upstream = d.join("upstream");
        fs::create_dir_all(&upstream).unwrap();
        sh(&upstream, &["init", "-q", "-b", "trunk"]);
        fs::write(upstream.join("a"), "1").unwrap();
        sh(&upstream, &["add", "a"]);
        sh(&upstream, &["commit", "-qm", "one"]);

        let cache = Cache::new(d.join("cache"));
        let remote = Remote {
            url: upstream.to_string_lossy().into_owned(),
            key: "local/o/r".into(),
            auth: None,
        };
        let bare = cache.sync(&remote).unwrap();
        assert_eq!(bare.default_branch().as_deref(), Some("trunk"));
        let first = bare.rev_parse("trunk").unwrap();

        fs::write(upstream.join("a"), "2").unwrap();
        sh(&upstream, &["commit", "-qam", "two"]);
        sh(&upstream, &["tag", "v2"]);
        cache.sync(&remote).unwrap();
        let second = bare.rev_parse("trunk").unwrap();
        assert_ne!(first, second);
        assert_eq!(bare.rev_parse("v2").unwrap(), second);

        let wt = d.join("wt");
        bare.add_worktree(&wt, &first, None).unwrap();
        assert_eq!(fs::read_to_string(wt.join("a")).unwrap(), "1");
        assert_eq!(bare.worktrees().unwrap(), vec![wt.canonicalize().unwrap()]);
        bare.remove_worktree(&wt, false).unwrap();
        assert!(bare.worktrees().unwrap().is_empty());

        let keys: Vec<_> = cache.list().unwrap().into_iter().map(|b| b.key).collect();
        assert_eq!(keys, ["local/o/r"]);
        assert!(cache.remove("local/o/r").unwrap());
        assert!(cache.list().unwrap().is_empty());
        fs::remove_dir_all(&d).unwrap();
    }
}
