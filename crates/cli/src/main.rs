//! steward: the operational work around software projects, on the command
//! line. GitHub through its API, repositories in a local cache, commands in
//! a sandbox, and a local model (biv) for what needs judgment.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use steward_github::{ItemRef, RepoRef};

mod ai;
mod cmd;
mod config;
mod ctx;
mod text;

pub type Result<T = (), E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

#[derive(Parser)]
#[command(
    name = "steward",
    version,
    about = "Operational work around software projects"
)]
struct Cli {
    /// configuration file [default: ~/.config/steward/config.toml]
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// GitHub credentials
    #[command(subcommand)]
    Auth(AuthCmd),
    /// The cache of bare repositories, and clones from it
    #[command(subcommand)]
    Repo(RepoCmd),
    /// Issues: list, read, triage, comment, label
    #[command(subcommand)]
    Issue(IssueCmd),
    /// Pull requests: list, read, review, create
    #[command(subcommand)]
    Pr(PrCmd),
    /// Commands run with restricted writes, network and environment
    #[command(subcommand)]
    Sandbox(SandboxCmd),
    /// steward's configuration
    #[command(subcommand)]
    Config(ConfigCmd),
}

#[derive(Subcommand)]
pub enum AuthCmd {
    /// Which token is used, for whom, and the rate limit left
    Status,
}

#[derive(Subcommand)]
pub enum RepoCmd {
    /// Clone repositories into the cache, or fetch them
    Sync {
        #[arg(required = true)]
        repos: Vec<RepoRef>,
    },
    /// The cached repositories
    List,
    /// Clone a repository (a revision, a pull request) through the cache,
    /// `origin` pointing to GitHub so that fetch, pull and push work as usual
    Checkout(CheckoutArgs),
    /// Delete a repository from the cache
    Remove { repo: RepoRef },
}

#[derive(Args)]
pub struct CheckoutArgs {
    pub repo: RepoRef,
    /// branch, tag or commit [default: the default branch]
    #[arg(long, conflicts_with = "pr")]
    pub rev: Option<String>,
    /// the head of a pull request
    #[arg(long)]
    pub pr: Option<u64>,
    /// create this branch at the revision rather than detaching HEAD (for a
    /// pull request: name its branch)
    #[arg(short, long)]
    pub branch: Option<String>,
    /// where, absent or empty [default: ./NAME]
    pub dir: Option<PathBuf>,
    /// use the cache as it is, without fetching
    #[arg(long)]
    pub offline: bool,
}

#[derive(Args)]
pub struct AiArgs {
    /// show the model's reasoning as it goes
    #[arg(short, long)]
    pub verbose: bool,
    /// the model, instead of the configured one
    #[arg(short, long)]
    pub model: Option<String>,
}

#[derive(Subcommand)]
pub enum IssueCmd {
    /// Issues of a repository, most recently updated first
    List {
        repo: RepoRef,
        /// open, closed or all
        #[arg(long, default_value = "open")]
        state: cmd::StateArg,
        /// having this label (repeatable)
        #[arg(short, long)]
        label: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 30)]
        limit: usize,
    },
    /// An issue and its comments
    Show {
        /// owner/name#number, or its URL
        item: ItemRef,
        /// comments shown, at most
        #[arg(long, default_value_t = 50)]
        comments: usize,
    },
    /// Classify an issue, judge its priority and choose its labels, with the model
    Triage {
        item: ItemRef,
        /// add the chosen labels to the issue
        #[arg(long)]
        apply: bool,
        /// print the result as JSON
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        ai: AiArgs,
    },
    /// Comment on an issue or pull request
    Comment {
        item: ItemRef,
        /// the comment (Markdown); `-` reads it from the standard input
        #[arg(short = 'm', long)]
        body: String,
    },
    /// Add or remove labels
    Label {
        item: ItemRef,
        #[arg(short, long)]
        add: Vec<String>,
        #[arg(short, long)]
        remove: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum PrCmd {
    /// Pull requests of a repository, most recently updated first
    List {
        repo: RepoRef,
        #[arg(long, default_value = "open")]
        state: cmd::StateArg,
        #[arg(short = 'n', long, default_value_t = 30)]
        limit: usize,
    },
    /// A pull request, its files, and optionally its diff
    Show {
        item: ItemRef,
        #[arg(long)]
        diff: bool,
    },
    /// Review a pull request with the model
    Review {
        item: ItemRef,
        /// post the review on the pull request (as a comment, never an approval)
        #[arg(long)]
        post: bool,
        #[command(flatten)]
        ai: AiArgs,
    },
    /// Open a pull request
    Create {
        repo: RepoRef,
        /// the branch with the changes (`owner:branch` for a fork)
        #[arg(long)]
        head: String,
        /// the branch to merge into [default: the default branch]
        #[arg(long)]
        base: Option<String>,
        #[arg(short, long)]
        title: String,
        /// the description (Markdown); `-` reads it from the standard input
        #[arg(short = 'm', long, default_value = "")]
        body: String,
        #[arg(long)]
        draft: bool,
    },
}

#[derive(Subcommand)]
pub enum SandboxCmd {
    /// The sandbox backend of this system, and the policy for a directory
    Check {
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Run a command in the sandbox
    Run(SandboxRunArgs),
}

#[derive(Args)]
pub struct SandboxRunArgs {
    /// the working directory, the only writable one
    #[arg(short = 'C', long, default_value = ".")]
    pub dir: PathBuf,
    /// let the command use the network
    #[arg(long)]
    pub net: bool,
    /// another writable path (repeatable)
    #[arg(short, long)]
    pub write: Vec<PathBuf>,
    /// pass this environment variable (repeatable)
    #[arg(short, long)]
    pub env: Vec<String>,
    /// kill the command after this many seconds
    #[arg(long)]
    pub timeout: Option<u64>,
    #[arg(required = true, last = true)]
    pub command: Vec<String>,
}

#[derive(Subcommand)]
pub enum ConfigCmd {
    /// The configuration in effect, and the paths steward uses
    Show,
    /// Write a commented configuration file, if there is none
    Init,
}

fn main() -> ExitCode {
    // die quietly when the output is closed (`steward ... | head`)
    // SAFETY: at the start of main, before any thread
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let cli = Cli::parse();
    let path = cli.config.clone().unwrap_or_else(config::config_file);
    let result = config::Config::load(&path)
        .map_err(Into::into)
        .and_then(|cfg| run(cli.command, ctx::Ctx::new(cfg), &path));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("steward: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command, ctx: ctx::Ctx, config_path: &std::path::Path) -> Result {
    match command {
        Command::Auth(c) => cmd::auth(&ctx, c),
        Command::Repo(c) => cmd::repo(&ctx, c),
        Command::Issue(c) => cmd::issue(&ctx, c),
        Command::Pr(c) => cmd::pr(&ctx, c),
        Command::Sandbox(c) => cmd::sandbox(&ctx, c),
        Command::Config(c) => cmd::config(&ctx, c, config_path),
    }
}
