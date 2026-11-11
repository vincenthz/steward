//! `~/.config/steward/config.toml`, and where steward keeps things

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub github: Github,
    pub brain: Brain,
    pub cache: Cache,
    pub sandbox: Sandbox,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Github {
    pub host: String,
    /// the API base, by default derived from the host
    pub api: Option<String>,
    /// prefer the environment or `gh auth login`; a token here is read after
    /// the environment and before gh's
    pub token: Option<String>,
}

impl Default for Github {
    fn default() -> Self {
        Github {
            host: "github.com".into(),
            api: None,
            token: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Brain {
    /// a GGUF file or `hf.co/owner/repo:QUANT`; the first downloaded of the
    /// defaults if unset
    pub model: Option<String>,
    pub cpu: bool,
    pub context: usize,
    pub thinking: bool,
    /// low, medium or high
    pub effort: String,
    pub max_tokens: usize,
    pub temperature: Option<f32>,
    pub lookup: usize,
    /// tokens of reasoning before the model is asked for its answer, 0 for no limit
    pub thinking_budget: usize,
}

impl Default for Brain {
    fn default() -> Self {
        Brain {
            model: None,
            cpu: false,
            context: 32768,
            thinking: true,
            effort: "medium".into(),
            max_tokens: 8192,
            temperature: None,
            lookup: 0,
            thinking_budget: 4096,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cache {
    /// by default `$XDG_CACHE_HOME/steward` or `~/.cache/steward`
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sandbox {
    /// let sandboxed commands use the network
    pub network: bool,
    /// writable besides the working directory
    pub writable: Vec<PathBuf>,
    /// unreadable besides the usual secrets
    pub hide: Vec<PathBuf>,
    /// variables passed besides the basic ones (PATH, HOME, LANG...)
    pub pass_env: Vec<String>,
}

pub const TEMPLATE: &str = r#"# steward configuration

[github]
host = "github.com"
# api = "https://github.example.com/api/v3"
# The token comes from $STEWARD_GITHUB_TOKEN, $GH_TOKEN or $GITHUB_TOKEN, then
# from `gh auth login` (its hosts.yml or the system keyring).
# token = ""

[brain]
# a GGUF file or a model of the Hugging Face cache; by default the first downloaded of
# hf.co/unsloth/Qwen3.6-35B-A3B-MTP-GGUF:Q8_0 and hf.co/unsloth/Qwen3.8-27B-GGUF:Q4_1
# model = "hf.co/unsloth/Qwen3.6-35B-A3B-MTP-GGUF:Q8_0"
cpu = false
context = 32768
thinking = true
effort = "medium"
max_tokens = 8192
# tokens of reasoning before the model is asked for its answer, 0 for no limit
thinking_budget = 4096

[cache]
# dir = "~/.cache/steward"

[sandbox]
network = false
writable = []
hide = []
pass_env = []
"#;

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home() -> PathBuf {
    env_dir("HOME").unwrap_or_else(|| PathBuf::from("."))
}

/// `~/x` to the home directory
pub fn expand(p: &Path) -> PathBuf {
    match p.strip_prefix("~") {
        Ok(rest) => home().join(rest),
        Err(_) => p.to_path_buf(),
    }
}

pub fn config_dir() -> PathBuf {
    env_dir("STEWARD_CONFIG_DIR")
        .or_else(|| env_dir("XDG_CONFIG_HOME").map(|d| d.join("steward")))
        .unwrap_or_else(|| home().join(".config/steward"))
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.cache
            .dir
            .as_deref()
            .map(expand)
            .or_else(|| env_dir("XDG_CACHE_HOME").map(|d| d.join("steward")))
            .unwrap_or_else(|| home().join(".cache/steward"))
    }

    /// The bare repositories
    pub fn git_dir(&self) -> PathBuf {
        self.cache_dir().join("git")
    }

    /// The worktrees made by `repo checkout`
    pub fn work_dir(&self) -> PathBuf {
        self.cache_dir().join("work")
    }

    /// The sandboxes' temporary directories
    pub fn sandbox_dir(&self) -> PathBuf {
        self.cache_dir().join("sandbox")
    }

    pub fn api(&self) -> String {
        self.github
            .api
            .clone()
            .unwrap_or_else(|| steward_github::api_for_host(&self.github.host))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_defaults() {
        let c: Config = toml::from_str(TEMPLATE).unwrap();
        let d = Config::default();
        assert_eq!(c.github.host, d.github.host);
        assert_eq!(c.brain.context, d.brain.context);
        assert_eq!(c.brain.effort, d.brain.effort);
        assert_eq!(c.brain.thinking_budget, d.brain.thinking_budget);
        assert!(!c.sandbox.network);
    }

    #[test]
    fn unknown_keys_are_errors() {
        assert!(toml::from_str::<Config>("[brain]\nmodle = \"x\"\n").is_err());
    }
}
