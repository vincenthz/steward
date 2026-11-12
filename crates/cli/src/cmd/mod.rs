mod ai;
mod github;
mod repo;
mod sandbox;

use std::io::Read;
use std::str::FromStr;

pub use github::{auth, issue, pr};
pub use repo::repo;
pub use sandbox::sandbox;

use steward_core::{Ctx, config};

use crate::{ConfigCmd, Result};

#[derive(Debug, Clone, Copy)]
pub struct StateArg(pub steward_github::State);

impl FromStr for StateArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        use steward_github::State;
        Ok(StateArg(match s {
            "open" => State::Open,
            "closed" => State::Closed,
            "all" => State::All,
            _ => return Err("expected open, closed or all".into()),
        }))
    }
}

/// A text argument, or the standard input for `-`
pub fn text_arg(arg: &str) -> Result<String> {
    if arg == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        Ok(s)
    } else {
        Ok(arg.to_string())
    }
}

pub fn config(ctx: &Ctx, c: ConfigCmd, path: &std::path::Path) -> Result {
    match c {
        ConfigCmd::Show => {
            let state = if path.is_file() {
                ""
            } else {
                " (absent: defaults)"
            };
            println!("# {}{state}", path.display());
            println!("# cache:     {}", ctx.cfg.cache_dir().display());
            println!("# api:       {}", ctx.cfg.api());
            println!();
            print!("{}", ctx.cfg.redacted().to_toml()?);
            Ok(())
        }
        ConfigCmd::Init => {
            config::init(path)?;
            println!("wrote {}", path.display());
            Ok(())
        }
    }
}
