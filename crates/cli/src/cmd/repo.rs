use std::time::Instant;

use steward_core::Ctx;
use steward_core::repo::{Checkout, checkout};
use steward_core::text::{ago_system, bytes};

use crate::{CheckoutArgs, RepoCmd, Result};

impl From<CheckoutArgs> for Checkout {
    fn from(a: CheckoutArgs) -> Self {
        Checkout {
            repo: a.repo,
            rev: a.rev,
            pr: a.pr,
            branch: a.branch,
            dir: a.dir,
            offline: a.offline,
        }
    }
}

pub fn repo(ctx: &Ctx, c: RepoCmd) -> Result {
    let cache = ctx.cache();
    match c {
        RepoCmd::Sync { repos } => {
            for r in repos {
                let start = Instant::now();
                let fresh = cache.get(&ctx.key(&r))?.is_none();
                eprint!("{} {r}… ", if fresh { "cloning" } else { "fetching" });
                let bare = cache.sync(&ctx.remote(&r))?;
                eprintln!(
                    "{:.1}s, {}, {}",
                    start.elapsed().as_secs_f32(),
                    bare.default_branch().unwrap_or_else(|| "?".into()),
                    bytes(bare.disk_size())
                );
            }
            Ok(())
        }
        RepoCmd::List => {
            let repos = cache.list()?;
            if repos.is_empty() {
                println!("no repositories in {}", cache.root().display());
            }
            for b in repos {
                println!(
                    "{:<50} {:>10}  synced {}",
                    b.key,
                    bytes(b.disk_size()),
                    b.last_sync().map(ago_system).unwrap_or_default(),
                );
            }
            Ok(())
        }
        RepoCmd::Checkout(a) => {
            let done = checkout(ctx, &a.into())?;
            eprintln!("origin {}; {}", done.origin, done.head);
            println!("{}", done.path.display());
            Ok(())
        }
        RepoCmd::Remove { repo } => {
            // clones made by `checkout` have their own objects: they stay usable
            let key = ctx.key(&repo);
            if cache.remove(&key)? {
                println!("removed {key}");
            } else {
                println!("{key} is not cached");
            }
            Ok(())
        }
    }
}
