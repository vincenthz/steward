use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use ureq::http::Response;
use ureq::{Body, RequestBuilder};

use crate::types::*;
use crate::{Error, ItemRef, RepoRef, Token};

const JSON: &str = "application/vnd.github+json";
const DIFF: &str = "application/vnd.github.diff";
/// the largest answer read, diffs included
const MAX_BODY: u64 = 64 << 20;

/// A connection to the API of one GitHub host
pub struct Client {
    agent: ureq::Agent,
    api: String,
    token: Option<Token>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    #[default]
    Open,
    Closed,
    All,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Open => "open",
            State::Closed => "closed",
            State::All => "all",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct IssueFilter {
    pub state: State,
    /// issues having all of these labels
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    /// updated at or after this time (ISO 8601)
    pub since: Option<String>,
    /// keep pull requests (the issues endpoint returns both)
    pub include_pulls: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewEvent {
    Comment,
    Approve,
    RequestChanges,
}

impl Client {
    /// `api` is the API base URL, e.g. [`crate::DEFAULT_API`]. Without a
    /// token, only public data is readable, under a much lower rate limit.
    pub fn new(api: impl Into<String>, token: Option<Token>) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .user_agent(concat!("steward/", env!("CARGO_PKG_VERSION")))
            .timeout_global(Some(Duration::from_secs(60)))
            .build()
            .into();
        Client {
            agent,
            api: api.into().trim_end_matches('/').to_string(),
            token,
        }
    }

    pub fn token(&self) -> Option<&Token> {
        self.token.as_ref()
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("https://") || path.starts_with("http://") {
            path.to_string()
        } else {
            format!("{}{path}", self.api)
        }
    }

    fn prepare<B>(&self, rb: RequestBuilder<B>, accept: &str) -> RequestBuilder<B> {
        let rb = rb
            .header("Accept", accept)
            .header("X-GitHub-Api-Version", "2022-11-28");
        match &self.token {
            Some(t) => rb.header("Authorization", format!("Bearer {}", t.secret())),
            None => rb,
        }
    }

    /// The answer, or the error GitHub explained
    fn check(mut resp: Response<Body>) -> Result<Response<Body>, Error> {
        let status = resp.status().as_u16();
        if status < 400 {
            return Ok(resp);
        }
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let rate_limit_reset = (matches!(status, 403 | 429)
            && header("x-ratelimit-remaining").as_deref() == Some("0"))
        .then(|| header("x-ratelimit-reset").and_then(|r| r.parse().ok()))
        .flatten();
        let text = resp
            .body_mut()
            .with_config()
            .limit(1 << 20)
            .read_to_string()
            .unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                let mut m = v.get("message")?.as_str()?.to_string();
                if let Some(errors) = v.get("errors").and_then(|e| e.as_array()) {
                    for e in errors {
                        let detail = e
                            .get("message")
                            .and_then(|m| m.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| e.to_string());
                        m.push_str("; ");
                        m.push_str(&detail);
                    }
                }
                Some(m)
            })
            .unwrap_or(text);
        Err(Error::Api {
            status,
            message,
            rate_limit_reset,
        })
    }

    fn read_json<T: DeserializeOwned>(mut resp: Response<Body>) -> Result<T, Error> {
        let text = resp
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_string()?;
        serde_json::from_str(&text).map_err(|e| Error::Decode(e.to_string()))
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        let rb = self.prepare(self.agent.get(&self.url(path)), JSON);
        Self::read_json(Self::check(rb.call()?)?)
    }

    /// The answer as text, with another media type (e.g. a diff)
    pub fn get_text(&self, path: &str, accept: &str) -> Result<String, Error> {
        let rb = self.prepare(self.agent.get(&self.url(path)), accept);
        let mut resp = Self::check(rb.call()?)?;
        Ok(resp
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_string()?)
    }

    pub fn post<T: DeserializeOwned>(&self, path: &str, body: &impl Serialize) -> Result<T, Error> {
        self.require_token()?;
        let rb = self.prepare(self.agent.post(&self.url(path)), JSON);
        Self::read_json(Self::check(rb.send_json(body)?)?)
    }

    pub fn patch<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, Error> {
        self.require_token()?;
        let rb = self.prepare(self.agent.patch(&self.url(path)), JSON);
        Self::read_json(Self::check(rb.send_json(body)?)?)
    }

    pub fn delete(&self, path: &str) -> Result<(), Error> {
        self.require_token()?;
        let rb = self.prepare(self.agent.delete(&self.url(path)), JSON);
        Self::check(rb.call()?).map(drop)
    }

    fn require_token(&self) -> Result<(), Error> {
        self.token.as_ref().map(drop).ok_or(Error::NoToken)
    }

    /// The items of a paginated list, following the `Link` headers, until
    /// `limit` of them are kept
    pub fn list<T: DeserializeOwned>(
        &self,
        path: &str,
        limit: usize,
        keep: impl Fn(&T) -> bool,
    ) -> Result<Vec<T>, Error> {
        let sep = if path.contains('?') { '&' } else { '?' };
        let mut next = Some(self.url(&format!("{path}{sep}per_page=100")));
        let mut out = Vec::new();
        while let Some(url) = next.take() {
            let rb = self.prepare(self.agent.get(&url), JSON);
            let resp = Self::check(rb.call()?)?;
            next = resp
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .and_then(next_link);
            let page: Vec<T> = Self::read_json(resp)?;
            for item in page {
                if out.len() >= limit {
                    return Ok(out);
                }
                if keep(&item) {
                    out.push(item);
                }
            }
        }
        Ok(out)
    }

    // the calls steward makes

    /// The authenticated user
    pub fn user(&self) -> Result<User, Error> {
        self.require_token()?;
        self.get("/user")
    }

    pub fn rate_limit(&self) -> Result<RateLimit, Error> {
        let v: serde_json::Value = self.get("/rate_limit")?;
        serde_json::from_value(v["resources"]["core"].clone())
            .map_err(|e| Error::Decode(e.to_string()))
    }

    pub fn repository(&self, repo: &RepoRef) -> Result<Repository, Error> {
        self.get(&repo.api_path())
    }

    pub fn labels(&self, repo: &RepoRef) -> Result<Vec<Label>, Error> {
        self.list(&format!("{}/labels", repo.api_path()), 1000, |_| true)
    }

    pub fn issues(
        &self,
        repo: &RepoRef,
        filter: &IssueFilter,
        limit: usize,
    ) -> Result<Vec<Issue>, Error> {
        let mut path = format!(
            "{}/issues?state={}&sort=updated",
            repo.api_path(),
            filter.state.as_str()
        );
        if !filter.labels.is_empty() {
            path.push_str("&labels=");
            path.push_str(&encode(&filter.labels.join(",")));
        }
        if let Some(a) = &filter.assignee {
            path.push_str("&assignee=");
            path.push_str(&encode(a));
        }
        if let Some(s) = &filter.since {
            path.push_str("&since=");
            path.push_str(&encode(s));
        }
        self.list(&path, limit, |i: &Issue| {
            filter.include_pulls || !i.is_pull_request()
        })
    }

    /// An issue, or a pull request seen as an issue
    pub fn issue(&self, item: &ItemRef) -> Result<Issue, Error> {
        self.get(&format!("{}/issues/{}", item.repo.api_path(), item.number))
    }

    pub fn issue_comments(&self, item: &ItemRef, limit: usize) -> Result<Vec<Comment>, Error> {
        self.list(
            &format!("{}/issues/{}/comments", item.repo.api_path(), item.number),
            limit,
            |_| true,
        )
    }

    pub fn comment(&self, item: &ItemRef, body: &str) -> Result<Comment, Error> {
        self.post(
            &format!("{}/issues/{}/comments", item.repo.api_path(), item.number),
            &json!({ "body": body }),
        )
    }

    /// Adds labels to an issue or pull request, keeping its others
    pub fn add_labels(&self, item: &ItemRef, labels: &[String]) -> Result<Vec<Label>, Error> {
        self.post(
            &format!("{}/issues/{}/labels", item.repo.api_path(), item.number),
            &json!({ "labels": labels }),
        )
    }

    pub fn remove_label(&self, item: &ItemRef, label: &str) -> Result<(), Error> {
        self.delete(&format!(
            "{}/issues/{}/labels/{}",
            item.repo.api_path(),
            item.number,
            encode(label)
        ))
    }

    pub fn set_issue_state(&self, item: &ItemRef, state: State) -> Result<Issue, Error> {
        self.patch(
            &format!("{}/issues/{}", item.repo.api_path(), item.number),
            &json!({ "state": state.as_str() }),
        )
    }

    pub fn pulls(
        &self,
        repo: &RepoRef,
        state: State,
        limit: usize,
    ) -> Result<Vec<PullRequest>, Error> {
        self.list(
            &format!(
                "{}/pulls?state={}&sort=updated&direction=desc",
                repo.api_path(),
                state.as_str()
            ),
            limit,
            |_| true,
        )
    }

    pub fn pull(&self, item: &ItemRef) -> Result<PullRequest, Error> {
        self.get(&format!("{}/pulls/{}", item.repo.api_path(), item.number))
    }

    /// The unified diff of a pull request
    pub fn pull_diff(&self, item: &ItemRef) -> Result<String, Error> {
        self.get_text(
            &format!("{}/pulls/{}", item.repo.api_path(), item.number),
            DIFF,
        )
    }

    pub fn pull_files(&self, item: &ItemRef) -> Result<Vec<PullFile>, Error> {
        self.list(
            &format!("{}/pulls/{}/files", item.repo.api_path(), item.number),
            3000,
            |_| true,
        )
    }

    pub fn create_pull(
        &self,
        repo: &RepoRef,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<PullRequest, Error> {
        self.post(
            &format!("{}/pulls", repo.api_path()),
            &json!({ "head": head, "base": base, "title": title, "body": body, "draft": draft }),
        )
    }

    /// A review with only a summary, no line comments (yet)
    pub fn create_review(
        &self,
        item: &ItemRef,
        commit: &str,
        body: &str,
        event: ReviewEvent,
    ) -> Result<Review, Error> {
        let event = match event {
            ReviewEvent::Comment => "COMMENT",
            ReviewEvent::Approve => "APPROVE",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
        };
        self.post(
            &format!("{}/pulls/{}/reviews", item.repo.api_path(), item.number),
            &json!({ "commit_id": commit, "body": body, "event": event }),
        )
    }
}

/// The `rel="next"` URL of a `Link` header
fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        params
            .split(';')
            .any(|p| p.trim() == "rel=\"next\"")
            .then(|| {
                url.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string()
            })
    })
}

/// Percent-encoding of a query value or path segment
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b',') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_header() {
        let h = r#"<https://api.github.com/repositories/1/issues?page=2>; rel="next", <https://api.github.com/repositories/1/issues?page=5>; rel="last""#;
        assert_eq!(
            next_link(h).as_deref(),
            Some("https://api.github.com/repositories/1/issues?page=2")
        );
        let last = r#"<https://api.github.com/x?page=1>; rel="prev", <https://api.github.com/x?page=1>; rel="first""#;
        assert_eq!(next_link(last), None);
    }

    #[test]
    fn encoding() {
        assert_eq!(encode("good first issue"), "good%20first%20issue");
        assert_eq!(encode("a,b"), "a,b");
        assert_eq!(encode("C-bug/ü"), "C-bug%2F%C3%BC");
    }
}
