use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::ctx::Ctx;
use crate::text::{ago_system, bytes};
use crate::{CheckoutArgs, RepoCmd, Result};

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
        RepoCmd::List { verbose } => {
            let repos = cache.list()?;
            if repos.is_empty() {
                println!("no repositories in {}", cache.root().display());
            }
            for b in repos {
                let worktrees = b.worktrees().unwrap_or_default();
                println!(
                    "{:<50} {:>10}  synced {:<10} {} checkout(s)",
                    b.key,
                    bytes(b.disk_size()),
                    b.last_sync().map(ago_system).unwrap_or_default(),
                    worktrees.len()
                );
                if verbose {
                    for w in worktrees {
                        println!("    {}", w.display());
                    }
                }
            }
            Ok(())
        }
        RepoCmd::Checkout(a) => {
            let path = checkout(ctx, &a)?;
            println!("{}", path.display());
            Ok(())
        }
        RepoCmd::Discard { path, force } => {
            let path = path.canonicalize()?;
            // find the repository owning the worktree
            for b in cache.list()? {
                if b.worktrees()?.iter().any(|w| w == &path) {
                    b.remove_worktree(&path, force)?;
                    println!("removed {}", path.display());
                    return Ok(());
                }
            }
            Err(format!("{} is not a checkout of the cache", path.display()).into())
        }
        RepoCmd::Remove { repo } => {
            let key = ctx.key(&repo);
            if let Some(b) = cache.get(&key)? {
                let wt = b.worktrees()?;
                if !wt.is_empty() {
                    return Err(format!(
                        "{key} has {} checkout(s); discard them first (`steward repo list -v`)",
                        wt.len()
                    )
                    .into());
                }
            }
            if cache.remove(&key)? {
                println!("removed {key}");
            } else {
                println!("{key} is not cached");
            }
            Ok(())
        }
    }
}

/// A worktree of the revision or pull request, the repository synced first
pub fn checkout(ctx: &Ctx, a: &CheckoutArgs) -> Result<PathBuf> {
    let cache = ctx.cache();
    let key = ctx.key(&a.repo);
    let bare = match (cache.get(&key)?, a.offline) {
        (Some(b), true) => b,
        (None, true) => return Err(format!("{key} is not cached").into()),
        (_, false) => cache.sync(&ctx.remote(&a.repo))?,
    };
    let (rev, label) = match (&a.pr, &a.rev) {
        (Some(n), _) => {
            let commit = if a.offline {
                bare.rev_parse(&format!("refs/pull/{n}/head"))?
            } else {
                bare.fetch_pull(ctx.git_auth().as_ref(), *n)
                    .map_err(|e| match e {
                        steward_gitcache::Error::Git { stderr, .. }
                            if stderr.contains("couldn't find remote ref") =>
                        {
                            format!("{}#{n} is not a pull request", a.repo).into()
                        }
                        e => Box::<dyn std::error::Error>::from(e),
                    })?
            };
            (commit, format!("pr-{n}"))
        }
        (None, Some(r)) => (r.clone(), r.clone()),
        (None, None) => {
            let b = bare
                .default_branch()
                .ok_or("the repository has no default branch")?;
            (b.clone(), b)
        }
    };
    let path = match &a.path {
        Some(p) => p.clone(),
        None => unused(&ctx.cfg.work_dir().join(&key).join(sanitize(&label))),
    };
    if path.exists() {
        return Err(format!("{} exists already", path.display()).into());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    bare.add_worktree(&path, &rev, a.branch.as_deref())?;
    Ok(path)
}

fn sanitize(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// `path`, or `path-2`, `path-3`... the first that doesn't exist
fn unused(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    (2..)
        .map(|i| PathBuf::from(format!("{}-{i}", path.display())))
        .find(|p| !p.exists())
        .expect("some suffix is free")
}
