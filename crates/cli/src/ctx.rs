//! What the commands share: the configuration, the GitHub client, the cache

use std::cell::OnceCell;

use steward_gitcache::{Auth, Cache, Remote};
use steward_github::{Client, RepoRef, Token, auth};

use crate::config::Config;

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

    pub fn cache(&self) -> Cache {
        Cache::new(self.cfg.git_dir())
    }

    /// The cache key of a repository: `host/owner/name`
    pub fn key(&self, repo: &RepoRef) -> String {
        format!("{}/{}/{}", self.host(), repo.owner, repo.name)
    }

    pub fn git_auth(&self) -> Option<Auth> {
        self.token()
            .map(|t| Auth::github_token(self.host(), t.secret()))
    }

    pub fn remote(&self, repo: &RepoRef) -> Remote {
        Remote {
            url: repo.clone_url(self.host()),
            key: self.key(repo),
            auth: self.git_auth(),
        }
    }
}
