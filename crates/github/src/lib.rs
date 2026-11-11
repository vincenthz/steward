//! A native client for the GitHub REST API: no `gh` process, but the tokens
//! `gh` stored are found and used ([`auth`]).
//!
//! The client is blocking: steward runs commands one after the other, and the
//! inference engine it drives is not `Send` anyway.

pub mod auth;
mod client;
mod error;
mod refs;
pub mod types;

pub use auth::Token;
pub use client::{Client, IssueFilter, ReviewEvent, State};
pub use error::Error;
pub use refs::{ItemRef, RepoRef};

/// The API of github.com; GitHub Enterprise serves it at `https://HOST/api/v3`
pub const DEFAULT_API: &str = "https://api.github.com";

/// The API base of `host`
pub fn api_for_host(host: &str) -> String {
    if host == "github.com" {
        DEFAULT_API.to_string()
    } else {
        format!("https://{host}/api/v3")
    }
}
