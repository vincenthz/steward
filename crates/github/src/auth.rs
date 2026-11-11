//! Finding a token: steward's own, the environment variables `gh` reads, or
//! what `gh auth login` stored (its `hosts.yml`, or the system keyring).

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;

/// A token and where it was found. Its `Debug` doesn't show it.
#[derive(Clone)]
pub struct Token {
    secret: String,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// an environment variable
    Env(&'static str),
    /// steward's configuration file
    Config,
    /// `gh`'s hosts.yml
    GhHosts(PathBuf),
    /// the system keyring, where `gh` stores tokens by default
    Keyring { user: Option<String> },
}

impl Token {
    pub fn new(secret: impl Into<String>, source: Source) -> Self {
        Token {
            secret: secret.into(),
            source,
        }
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Token")
            .field("secret", &"<redacted>")
            .field("source", &self.source)
            .finish()
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Env(var) => write!(f, "${var}"),
            Source::Config => write!(f, "steward configuration"),
            Source::GhHosts(p) => write!(f, "{}", p.display()),
            Source::Keyring { user: Some(u) } => write!(f, "system keyring (gh, {u})"),
            Source::Keyring { user: None } => write!(f, "system keyring (gh)"),
        }
    }
}

/// The variables holding a token for `host`, in the order `gh` reads them,
/// after steward's own
fn env_vars(host: &str) -> &'static [&'static str] {
    if host == "github.com" {
        &["STEWARD_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"]
    } else {
        &[
            "STEWARD_GITHUB_TOKEN",
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
        ]
    }
}

/// `gh`'s configuration directory
pub fn gh_config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("GH_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(d.into());
    }
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d).join("gh"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/gh"))
}

/// The token for `host`: from the environment, then `configured` (steward's
/// configuration), then `gh`'s hosts.yml, then the keyring
pub fn discover(host: &str, configured: Option<&str>) -> Option<Token> {
    for &var in env_vars(host) {
        if let Ok(v) = std::env::var(var)
            && !v.trim().is_empty()
        {
            return Some(Token::new(v.trim(), Source::Env(var)));
        }
    }
    if let Some(t) = configured.filter(|t| !t.trim().is_empty()) {
        return Some(Token::new(t.trim(), Source::Config));
    }
    let hosts = gh_config_dir().map(|d| d.join("hosts.yml"));
    let entry = hosts
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| GhHost::parse(&text, host))
        .unwrap_or_default();
    if let Some(t) = entry.token {
        return Some(Token::new(t, Source::GhHosts(hosts.unwrap_or_default())));
    }
    keyring(host, entry.user.as_deref())
}

/// What `gh` knows of a host
#[derive(Debug, Default, PartialEq, Eq)]
struct GhHost {
    /// the active user
    user: Option<String>,
    /// stored in the file rather than in the keyring
    token: Option<String>,
}

impl GhHost {
    /// From hosts.yml:
    /// ```yaml
    /// github.com:
    ///     users:
    ///         someone:
    ///             oauth_token: gho_...
    ///     git_protocol: https
    ///     oauth_token: gho_...
    ///     user: someone
    /// ```
    /// The top `oauth_token` is the active user's, and only present when the
    /// token is not in the keyring.
    fn parse(text: &str, host: &str) -> Self {
        let entries = yaml_scalars(text);
        let get = |path: &[&str]| {
            entries
                .iter()
                .find(|(p, _)| p.iter().map(String::as_str).eq(path.iter().copied()))
                .map(|(_, v)| v.clone())
                .filter(|v| !v.is_empty())
        };
        let user = get(&[host, "user"]);
        let token = get(&[host, "oauth_token"]).or_else(|| {
            user.as_deref()
                .and_then(|u| get(&[host, "users", u, "oauth_token"]))
        });
        GhHost { user, token }
    }
}

/// The scalars of a block-style YAML mapping, with the path of keys leading
/// to each: just enough YAML for `gh`'s files (no flow style, no lists, no
/// multi-line strings)
fn yaml_scalars(text: &str) -> Vec<(Vec<String>, String)> {
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        while stack.last().is_some_and(|(i, _)| *i >= indent) {
            stack.pop();
        }
        let key = unquote(key.trim()).to_string();
        let value = strip_comment(value.trim());
        if value.is_empty() {
            stack.push((indent, key));
        } else {
            let mut path: Vec<String> = stack.iter().map(|(_, k)| k.clone()).collect();
            path.push(key);
            out.push((path, unquote(value).to_string()));
        }
    }
    out
}

fn strip_comment(v: &str) -> &str {
    if v.starts_with(['"', '\'']) {
        return v;
    }
    match v.find(" #") {
        Some(i) => v[..i].trim_end(),
        None => v,
    }
}

fn unquote(v: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = v.strip_prefix(q).and_then(|v| v.strip_suffix(q)) {
            return inner;
        }
    }
    v
}

/// The token `gh` stored in the system keyring for `host` (service
/// `gh:HOST`, account the user's login)
fn keyring(host: &str, user: Option<&str>) -> Option<Token> {
    let service = format!("gh:{host}");
    let mut cmd;
    if cfg!(target_os = "macos") {
        cmd = Command::new("/usr/bin/security");
        cmd.args(["find-generic-password", "-s", &service]);
        if let Some(u) = user {
            cmd.args(["-a", u]);
        }
        cmd.arg("-w");
    } else if cfg!(target_os = "linux") {
        cmd = Command::new("secret-tool");
        cmd.args(["lookup", "service", &service]);
        if let Some(u) = user {
            cmd.args(["username", u]);
        }
    } else {
        return None;
    }
    let out = cmd.stderr(std::process::Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8(out.stdout).ok()?;
    let secret = decode_keyring_value(raw.trim())?;
    Some(Token::new(
        secret,
        Source::Keyring {
            user: user.map(str::to_string),
        },
    ))
}

/// go-keyring, which `gh` uses, may store the secret encoded
fn decode_keyring_value(v: &str) -> Option<String> {
    if let Some(b64) = v.strip_prefix("go-keyring-base64:") {
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        return String::from_utf8(bytes).ok();
    }
    if let Some(hex) = v.strip_prefix("go-keyring-encoded:") {
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
            .collect::<Option<Vec<u8>>>()?;
        return String::from_utf8(bytes).ok();
    }
    (!v.is_empty()).then(|| v.to_string())
}

/// Whether `path` is readable by others than its owner (for a warning about
/// tokens in configuration files)
#[cfg(unix)]
pub fn is_world_readable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o044 != 0)
}

#[cfg(not(unix))]
pub fn is_world_readable(_: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTS: &str = "\
github.com:
    users:
        alice:
            oauth_token: gho_alice
        bob:
            oauth_token: \"gho_bob\"
    git_protocol: https
    user: bob
ghe.example.com:
    user: carol # the work account
    oauth_token: 'gho_carol'
";

    #[test]
    fn hosts_file() {
        assert_eq!(
            GhHost::parse(HOSTS, "github.com"),
            GhHost {
                user: Some("bob".into()),
                token: Some("gho_bob".into()),
            }
        );
        assert_eq!(
            GhHost::parse(HOSTS, "ghe.example.com"),
            GhHost {
                user: Some("carol".into()),
                token: Some("gho_carol".into()),
            }
        );
        assert_eq!(GhHost::parse(HOSTS, "other.com"), GhHost::default());
    }

    #[test]
    fn keyring_only() {
        // with the token in the keyring, the file only names the user
        let text =
            "github.com:\n    users:\n        alice:\n    git_protocol: ssh\n    user: alice\n";
        assert_eq!(
            GhHost::parse(text, "github.com"),
            GhHost {
                user: Some("alice".into()),
                token: None,
            }
        );
    }

    #[test]
    fn keyring_encodings() {
        assert_eq!(decode_keyring_value("gho_x").as_deref(), Some("gho_x"));
        assert_eq!(
            decode_keyring_value("go-keyring-base64:Z2hvX3NlY3JldA==").as_deref(),
            Some("gho_secret")
        );
        assert_eq!(
            decode_keyring_value("go-keyring-encoded:67686f5f78").as_deref(),
            Some("gho_x")
        );
        assert_eq!(decode_keyring_value(""), None);
    }

    #[test]
    fn debug_hides_the_secret() {
        let t = Token::new("gho_secret", Source::Config);
        assert!(!format!("{t:?}").contains("gho_secret"));
    }
}
