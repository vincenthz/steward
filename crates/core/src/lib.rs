//! What steward does, without the command line: the configuration, the
//! context the operations share (GitHub client, repository cache, sandbox),
//! checkouts through the cache, and the model's triage and reviews.
//!
//! Nothing here prints: operations return what they found or did, and the
//! model's progress goes to an observer ([`ai::Event`]). Outward actions
//! (labels added, reviews posted) are separate calls from the ones that
//! compute them.

pub mod ai;
pub mod config;
mod ctx;
pub mod repo;
pub mod review;
pub mod text;
pub mod triage;

pub use config::Config;
pub use ctx::Ctx;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T = (), E = Error> = std::result::Result<T, E>;
