//! The parts of the API's objects steward uses. Unknown fields are ignored and
//! missing ones take their default, so that the objects of older GitHub
//! Enterprise versions decode too.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct User {
    pub login: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Label {
    pub name: String,
    pub color: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Repository {
    pub full_name: String,
    pub description: Option<String>,
    pub default_branch: String,
    pub private: bool,
    pub archived: bool,
    pub fork: bool,
    pub html_url: String,
    pub clone_url: String,
    pub ssh_url: String,
    pub open_issues_count: u64,
    pub stargazers_count: u64,
    pub language: Option<String>,
    pub pushed_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    /// `open` or `closed`
    pub state: String,
    pub state_reason: Option<String>,
    pub user: User,
    pub labels: Vec<Label>,
    pub assignees: Vec<User>,
    /// the number of comments
    pub comments: u64,
    /// `OWNER`, `MEMBER`, `COLLABORATOR`, `CONTRIBUTOR`, `FIRST_TIME_CONTRIBUTOR`, `NONE`...
    pub author_association: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub html_url: String,
    /// present when the issue is a pull request
    pub pull_request: Option<serde_json::Value>,
}

impl Issue {
    pub fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Comment {
    pub id: u64,
    pub user: User,
    pub body: Option<String>,
    pub author_association: String,
    pub created_at: String,
    pub html_url: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Branch {
    /// `owner:branch`
    pub label: String,
    #[serde(rename = "ref")]
    pub branch: String,
    pub sha: String,
    pub repo: Option<Repository>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub draft: bool,
    pub user: User,
    pub labels: Vec<Label>,
    pub head: Branch,
    pub base: Branch,
    pub author_association: String,
    pub created_at: String,
    pub updated_at: String,
    pub html_url: String,
    // only in the answers about one pull request
    pub merged: Option<bool>,
    pub mergeable: Option<bool>,
    pub mergeable_state: Option<String>,
    pub comments: Option<u64>,
    pub commits: Option<u64>,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub changed_files: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PullFile {
    pub filename: String,
    /// `added`, `removed`, `modified`, `renamed`...
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
    pub previous_filename: Option<String>,
    /// absent for binary files and big diffs
    pub patch: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Review {
    pub id: u64,
    pub user: User,
    pub body: Option<String>,
    pub state: String,
    pub html_url: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RateLimit {
    pub limit: u64,
    pub remaining: u64,
    pub reset: u64,
    pub used: u64,
}
