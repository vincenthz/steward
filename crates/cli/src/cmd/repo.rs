use std::path::PathBuf;
use std::time::Instant;

use steward_gitcache::{Bare, Work};
use steward_github::ItemRef;

use crate::config::Protocol;
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
            let path = checkout(ctx, &a)?;
            println!("{}", path.display());
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

/// A pull request's head, and where its branch lives
struct Head {
    number: u64,
    commit: String,
    /// the branch in the head repository; unknown offline, or when the
    /// repository was deleted
    branch: Option<String>,
    /// the head repository, if a fork: a remote name (its owner) and URL
    fork: Option<(String, String)>,
}

/// Clones the repository through the cache (synced first) to `dir` or
/// `./NAME`, at the revision or pull request, `origin` set to the repository
/// on GitHub
pub fn checkout(ctx: &Ctx, a: &CheckoutArgs) -> Result<PathBuf> {
    for r in [&a.rev, &a.branch].into_iter().flatten() {
        if r.is_empty() || r.starts_with('-') {
            return Err(format!("invalid revision or branch `{r}`").into());
        }
    }
    let ssh = ctx.protocol() == Protocol::Ssh;
    let cache = ctx.cache();
    let key = ctx.key(&a.repo);
    let bare = match (cache.get(&key)?, a.offline) {
        (Some(b), true) => b,
        (None, true) => return Err(format!("{key} is not cached").into()),
        (_, false) => cache.sync(&ctx.remote(&a.repo))?,
    };
    let head = match a.pr {
        Some(n) => Some(pull_head(ctx, &bare, a, n, ssh)?),
        None => None,
    };
    if let Some(r) = &a.rev
        && bare.rev_parse(r).is_err()
    {
        return Err(format!("{key} has no revision `{r}`").into());
    }

    let dest = a.dir.clone().unwrap_or_else(|| PathBuf::from(&a.repo.name));
    let created = !dest.exists();
    if !created && (!dest.is_dir() || std::fs::read_dir(&dest)?.next().is_some()) {
        return Err(format!("{} exists already", dest.display()).into());
    }
    let work = bare.clone_to(&dest)?;
    let upstream = if ssh {
        a.repo.ssh_url(ctx.host())
    } else {
        a.repo.clone_url(ctx.host())
    };
    let set_up = match &head {
        Some(h) => pull_branch(&work, a, h),
        None => rev_branch(&work, a),
    }
    .and_then(|note| {
        work.git(&["remote", "set-url", "origin", &upstream])?;
        Ok(note)
    });
    match set_up {
        Ok(note) => {
            eprintln!("origin {upstream}; {note}");
            Ok(dest)
        }
        Err(e) => {
            if created {
                let _ = std::fs::remove_dir_all(&dest);
            }
            Err(e)
        }
    }
}

fn pull_head(ctx: &Ctx, bare: &Bare, a: &CheckoutArgs, n: u64, ssh: bool) -> Result<Head> {
    if a.offline {
        let commit = bare
            .rev_parse(&format!("refs/pull/{n}/head"))
            .map_err(|_| format!("#{n} is not in the cache; fetch it without --offline"))?;
        return Ok(Head {
            number: n,
            commit,
            branch: None,
            fork: None,
        });
    }
    let pr = ctx.github().pull(&ItemRef {
        repo: a.repo.clone(),
        number: n,
    })?;
    let commit = bare
        .fetch_pull(ctx.git_auth().as_ref(), n)
        .map_err(|e| match e {
            steward_gitcache::Error::Git { stderr, .. }
                if stderr.contains("couldn't find remote ref") =>
            {
                format!("{}#{n} is not a pull request", a.repo).into()
            }
            e => Box::<dyn std::error::Error>::from(e),
        })?;
    let base = format!("{}/{}", a.repo.owner, a.repo.name);
    let (branch, fork) = match &pr.head.repo {
        None => (None, None),
        Some(r) if r.full_name.eq_ignore_ascii_case(&base) => (Some(pr.head.branch.clone()), None),
        Some(r) => {
            let owner = r
                .full_name
                .split('/')
                .next()
                .unwrap_or_default()
                .to_string();
            let url = if ssh && !r.ssh_url.is_empty() {
                r.ssh_url.clone()
            } else {
                r.clone_url.clone()
            };
            (Some(pr.head.branch.clone()), Some((owner, url)))
        }
    };
    Ok(Head {
        number: n,
        commit,
        branch,
        fork,
    })
}

fn has_ref(work: &Work, r: &str) -> bool {
    work.git(&["rev-parse", "--verify", "--quiet", r]).is_ok()
}

/// The pull request on a branch tracking its head: the head repository's
/// branch (a remote added for a fork), so that pull and push go there; else
/// `refs/pull/N/head`, only pulled
fn pull_branch(work: &Work, a: &CheckoutArgs, h: &Head) -> Result<String> {
    let n = h.number;
    work.git(&["fetch", "--quiet", "origin", &format!("refs/pull/{n}/head")])?;
    let local = a.branch.clone().unwrap_or_else(|| match &h.branch {
        Some(b) if !has_ref(work, &format!("refs/heads/{b}")) => b.clone(),
        _ => format!("pr-{n}"),
    });
    work.git(&["switch", "--quiet", "-c", &local, "FETCH_HEAD"])?;
    let (remote, merge) = match (&h.branch, &h.fork) {
        (Some(b), Some((remote, url))) => {
            work.git(&["remote", "add", remote, url])?;
            // what `fetch` would record, without reaching the fork now
            work.git(&[
                "update-ref",
                &format!("refs/remotes/{remote}/{b}"),
                &h.commit,
            ])?;
            if &local != b {
                // push the local branch to the PR's, whatever its name
                work.git(&[
                    "config",
                    &format!("remote.{remote}.push"),
                    &format!("refs/heads/{local}:refs/heads/{b}"),
                ])?;
            }
            (remote.as_str(), format!("refs/heads/{b}"))
        }
        (Some(b), None) => ("origin", format!("refs/heads/{b}")),
        (None, _) => ("origin", format!("refs/pull/{n}/head")),
    };
    work.git(&["config", &format!("branch.{local}.remote"), remote])?;
    work.git(&["config", &format!("branch.{local}.merge"), &merge])?;
    let target = merge.strip_prefix("refs/heads/").unwrap_or(&merge);
    Ok(format!(
        "#{n} on branch {local}, tracking {remote} {target}"
    ))
}

/// `--rev`: a branch tracks the remote's, a tag or commit is detached; `-b`
/// makes a branch there
fn rev_branch(work: &Work, a: &CheckoutArgs) -> Result<String> {
    let current = || {
        work.git(&["symbolic-ref", "--short", "HEAD"])
            .map(|b| format!("on branch {}", b.trim()))
            .unwrap_or_else(|_| "detached".into())
    };
    match (&a.rev, &a.branch) {
        (None, None) => {}
        (None, Some(b)) => drop(work.git(&["switch", "--quiet", "-c", b])?),
        (Some(r), b) => {
            let remote_branch = has_ref(work, &format!("refs/remotes/origin/{r}"));
            match (remote_branch, b) {
                // an existing local branch, or one made tracking origin's
                (true, None) => work.git(&["switch", "--quiet", r])?,
                (true, Some(b)) => work.git(&[
                    "switch",
                    "--quiet",
                    "-c",
                    b,
                    "--track",
                    &format!("origin/{r}"),
                ])?,
                (false, None) => work.git(&["switch", "--quiet", "--detach", r])?,
                (false, Some(b)) => work.git(&["switch", "--quiet", "-c", b, r])?,
            };
        }
    }
    Ok(current())
}
