//! The commands that only talk to GitHub

use steward_github::types::{Issue, Label};
use steward_github::{IssueFilter, auth};

use super::text_arg;
use crate::ctx::Ctx;
use crate::text::{ago, truncate};
use crate::{AuthCmd, IssueCmd, PrCmd, Result, ai};

pub fn auth(ctx: &Ctx, c: AuthCmd) -> Result {
    match c {
        AuthCmd::Status => {
            println!("host     {}", ctx.host());
            println!("api      {}", ctx.cfg.api());
            match ctx.token() {
                None => println!("token    none (public data only, 60 requests/hour)"),
                Some(t) => {
                    println!("token    from {}", t.source);
                    if let auth::Source::GhHosts(p) = &t.source
                        && auth::is_world_readable(p)
                    {
                        println!("         warning: {} is readable by others", p.display());
                    }
                    match ctx.github().user() {
                        Ok(u) => println!("user     {}", u.login),
                        Err(e) => println!("user     ? ({e})"),
                    }
                }
            }
            let rl = ctx.github().rate_limit()?;
            println!(
                "rate     {}/{} left, resets {}",
                rl.remaining,
                rl.limit,
                reset_in(rl.reset)
            );
            Ok(())
        }
    }
}

fn reset_in(reset: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("in {}m", reset.saturating_sub(now).div_ceil(60))
}

pub fn label_names(labels: &[Label]) -> String {
    labels
        .iter()
        .map(|l| l.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_issue_line(i: &Issue) {
    let labels = if i.labels.is_empty() {
        String::new()
    } else {
        format!("  [{}]", label_names(&i.labels))
    };
    println!(
        "#{:<6} {}{labels}  ({}, {}, {} comments)",
        i.number,
        truncate(&i.title, 80),
        i.user.login,
        ago(&i.updated_at),
        i.comments
    );
}

pub fn issue(ctx: &Ctx, c: IssueCmd) -> Result {
    let gh = ctx.github();
    match c {
        IssueCmd::List {
            repo,
            state,
            label,
            limit,
        } => {
            let filter = IssueFilter {
                state: state.0,
                labels: label,
                ..Default::default()
            };
            let issues = gh.issues(&repo, &filter, limit)?;
            if issues.is_empty() {
                println!("no issues");
            }
            for i in &issues {
                print_issue_line(i);
            }
            Ok(())
        }
        IssueCmd::Show { item, comments } => {
            let i = gh.issue(&item)?;
            let kind = if i.is_pull_request() {
                "pull request"
            } else {
                "issue"
            };
            println!("{item}  {}", i.title);
            println!(
                "{kind}, {}{}, by {} ({}), opened {}, updated {}",
                i.state,
                i.state_reason
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default(),
                i.user.login,
                i.author_association.to_lowercase(),
                ago(&i.created_at),
                ago(&i.updated_at)
            );
            if !i.labels.is_empty() {
                println!("labels: {}", label_names(&i.labels));
            }
            println!("{}\n", i.html_url);
            println!("{}", i.body.as_deref().unwrap_or("(no description)").trim());
            if i.comments > 0 && comments > 0 {
                for c in gh.issue_comments(&item, comments)? {
                    println!(
                        "\n--- {} ({}), {}",
                        c.user.login,
                        c.author_association.to_lowercase(),
                        ago(&c.created_at)
                    );
                    println!("{}", c.body.as_deref().unwrap_or("").trim());
                }
                if i.comments as usize > comments {
                    println!("\n({} more comments)", i.comments as usize - comments);
                }
            }
            Ok(())
        }
        IssueCmd::Triage {
            item,
            apply,
            json,
            ai,
        } => ai::triage::run(ctx, &item, apply, json, &ai),
        IssueCmd::Comment { item, body } => {
            let body = text_arg(&body)?;
            if body.trim().is_empty() {
                return Err("empty comment".into());
            }
            let c = gh.comment(&item, &body)?;
            println!("{}", c.html_url);
            Ok(())
        }
        IssueCmd::Label { item, add, remove } => {
            if add.is_empty() && remove.is_empty() {
                return Err("nothing to do: give --add or --remove".into());
            }
            for l in &remove {
                gh.remove_label(&item, l)?;
            }
            let labels = if add.is_empty() {
                gh.issue(&item)?.labels
            } else {
                gh.add_labels(&item, &add)?
            };
            println!("{item}: [{}]", label_names(&labels));
            Ok(())
        }
    }
}

pub fn pr(ctx: &Ctx, c: PrCmd) -> Result {
    let gh = ctx.github();
    match c {
        PrCmd::List { repo, state, limit } => {
            let pulls = gh.pulls(&repo, state.0, limit)?;
            if pulls.is_empty() {
                println!("no pull requests");
            }
            for p in &pulls {
                let draft = if p.draft { " (draft)" } else { "" };
                println!(
                    "#{:<6} {}{draft}  ({} → {}, {}, {})",
                    p.number,
                    truncate(&p.title, 80),
                    p.head.label,
                    p.base.branch,
                    p.user.login,
                    ago(&p.updated_at)
                );
            }
            Ok(())
        }
        PrCmd::Show { item, diff } => {
            let p = gh.pull(&item)?;
            println!("{item}  {}", p.title);
            let state = if p.merged == Some(true) {
                "merged".to_string()
            } else if p.draft {
                format!("{} (draft)", p.state)
            } else {
                p.state.clone()
            };
            println!(
                "{state}, {} → {}, by {} ({}), opened {}, updated {}",
                p.head.label,
                p.base.branch,
                p.user.login,
                p.author_association.to_lowercase(),
                ago(&p.created_at),
                ago(&p.updated_at)
            );
            println!(
                "{} commits, {} files, +{} -{}{}",
                p.commits.unwrap_or(0),
                p.changed_files.unwrap_or(0),
                p.additions.unwrap_or(0),
                p.deletions.unwrap_or(0),
                p.mergeable_state
                    .as_deref()
                    .map(|m| format!(", mergeable: {m}"))
                    .unwrap_or_default()
            );
            println!("{}\n", p.html_url);
            println!("{}", p.body.as_deref().unwrap_or("(no description)").trim());
            println!();
            for f in gh.pull_files(&item)? {
                println!(
                    "  {:<9} +{:<5} -{:<5} {}",
                    f.status, f.additions, f.deletions, f.filename
                );
            }
            if diff {
                println!("\n{}", gh.pull_diff(&item)?);
            }
            Ok(())
        }
        PrCmd::Review { item, post, ai } => ai::review::run(ctx, &item, post, &ai),
        PrCmd::Create {
            repo,
            head,
            base,
            title,
            body,
            draft,
        } => {
            let base = match base {
                Some(b) => b,
                None => gh.repository(&repo)?.default_branch,
            };
            let body = text_arg(&body)?;
            let p = gh.create_pull(&repo, &head, &base, &title, &body, draft)?;
            println!("{}", p.html_url);
            Ok(())
        }
    }
}
