//! What the commands share: the configuration, the GitHub client, the cache

use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use steward_gitcache::{Auth, Cache, Remote};
use steward_github::{Client, RepoRef, Token, auth};
use steward_sandbox::{Policy, Sandbox};

use crate::Result;
use crate::config::{Config, Protocol, expand};

pub struct Ctx {
    pub cfg: Config,
    token: OnceCell<Option<Token>>,
    github: OnceCell<Client>,
}

impl Ctx {
    pub fn new(cfg: Config) -> Self {
        Ctx {
            cfg,
            token: OnceCell::new(),
            github: OnceCell::new(),
        }
    }

    pub fn host(&self) -> &str {
        &self.cfg.github.host
    }

    pub fn token(&self) -> Option<&Token> {
        self.token
            .get_or_init(|| auth::discover(self.host(), self.cfg.github.token.as_deref()))
            .as_ref()
    }

    pub fn github(&self) -> &Client {
        self.github
            .get_or_init(|| Client::new(self.cfg.api(), self.token().cloned()))
    }

    /// The bare repositories, at `<cache>/<host>/<owner>/<name>`
    pub fn cache(&self) -> Cache {
        Cache::new(self.cfg.cache_dir())
    }

    /// The cache key of a repository: `host/owner/name`
    pub fn key(&self, repo: &RepoRef) -> String {
        format!("{}/{}/{}", self.host(), repo.owner, repo.name)
    }

    pub fn git_auth(&self) -> Option<Auth> {
        self.token()
            .map(|t| Auth::github_token(self.host(), t.secret()))
    }

    /// How clones reach the host: `git.protocol` in the configuration, else
    /// gh's `git_protocol`, else https
    pub fn protocol(&self) -> Protocol {
        let host = self.host();
        self.cfg.git.protocol.get(host).copied().unwrap_or_else(|| {
            match auth::git_protocol(host).as_deref() {
                Some("ssh") => Protocol::Ssh,
                _ => Protocol::Https,
            }
        })
    }

    pub fn remote(&self, repo: &RepoRef) -> Remote {
        Remote {
            url: repo.clone_url(self.host()),
            key: self.key(repo),
            auth: self.git_auth(),
        }
    }

    /// The sandbox for commands working in `dir`: the configured policy, and
    /// these additions
    pub fn sandbox(
        &self,
        dir: &Path,
        network: bool,
        writable: &[PathBuf],
        env: &[String],
    ) -> Result<Sandbox> {
        let s = &self.cfg.sandbox;
        let mut p = Policy::workdir(dir);
        p.network = network || s.network;
        p.writable
            .extend(s.writable.iter().chain(writable).map(|w| expand(w)));
        p.hidden.extend(s.hide.iter().map(|h| expand(h)));
        p.pass_env.extend(s.pass_env.iter().chain(env).cloned());
        Ok(Sandbox::new(p, &self.cfg.sandbox_dir())?)
    }
}
