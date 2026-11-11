//! Naming repositories and their issues and pull requests on the command line

use std::fmt;
use std::str::FromStr;

/// `owner/name`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

/// An issue or a pull request: `owner/name#123` (they share their numbers)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ItemRef {
    pub repo: RepoRef,
    pub number: u64,
}

impl RepoRef {
    pub fn new(owner: impl Into<String>, name: impl Into<String>) -> Self {
        RepoRef {
            owner: owner.into(),
            name: name.into(),
        }
    }

    /// `/repos/owner/name`
    pub fn api_path(&self) -> String {
        format!("/repos/{}/{}", self.owner, self.name)
    }

    /// The https clone URL on `host`
    pub fn clone_url(&self, host: &str) -> String {
        format!("https://{host}/{}/{}.git", self.owner, self.name)
    }
}

fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The path of a URL or `host/owner/name` form, without the scheme and host:
/// what follows the first `/` when the first segment looks like a host
fn strip_host(s: &str) -> &str {
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let s = s.strip_prefix("git@").unwrap_or(s);
    match s.split_once(['/', ':']) {
        Some((first, rest)) if first.contains('.') => rest,
        _ => s,
    }
}

impl FromStr for RepoRef {
    type Err = String;

    /// `owner/name`, `github.com/owner/name`, `https://github.com/owner/name(.git)`
    fn from_str(s: &str) -> Result<Self, String> {
        let path = strip_host(s.trim()).trim_end_matches('/');
        let mut parts = path.splitn(3, '/');
        let (Some(owner), Some(name)) = (parts.next(), parts.next()) else {
            return Err(format!("`{s}`: expected owner/name"));
        };
        let name = name.strip_suffix(".git").unwrap_or(name);
        if !valid_segment(owner) || !valid_segment(name) {
            return Err(format!("`{s}`: expected owner/name"));
        }
        Ok(RepoRef::new(owner, name))
    }
}

impl FromStr for ItemRef {
    type Err = String;

    /// `owner/name#123`, or an issue or pull request URL
    /// (`https://github.com/owner/name/issues/123`, `.../pull/123`)
    fn from_str(s: &str) -> Result<Self, String> {
        let err = || format!("`{s}`: expected owner/name#number or an issue/PR URL");
        let (path, fragment) = match s.trim().split_once('#') {
            Some((path, fragment)) => (path, Some(fragment)),
            None => (s.trim(), None),
        };
        let path = strip_host(path).trim_end_matches('/');
        let segs: Vec<&str> = path.split('/').collect();
        let (owner, name, number) = match (segs.as_slice(), fragment) {
            ([owner, name, "issues" | "pull" | "pulls", number, ..], _) => (owner, name, *number),
            ([owner, name], Some(number)) => (owner, name, number),
            _ => return Err(err()),
        };
        Ok(ItemRef {
            repo: format!("{owner}/{name}").parse()?,
            number: number.parse().map_err(|_| err())?,
        })
    }
}

impl fmt::Display for RepoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

impl fmt::Display for ItemRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.repo, self.number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_forms() {
        let want = RepoRef::new("rust-lang", "cargo");
        for s in [
            "rust-lang/cargo",
            "github.com/rust-lang/cargo",
            "https://github.com/rust-lang/cargo",
            "https://github.com/rust-lang/cargo.git",
            "https://github.com/rust-lang/cargo/",
            "https://github.com/rust-lang/cargo/tree/master/src",
            "git@github.com:rust-lang/cargo.git",
        ] {
            assert_eq!(s.parse::<RepoRef>().unwrap(), want, "{s}");
        }
        for s in ["cargo", "a/..", "/x", "a b/c", ""] {
            assert!(s.parse::<RepoRef>().is_err(), "{s}");
        }
    }

    #[test]
    fn item_forms() {
        let want = ItemRef {
            repo: RepoRef::new("o", "r.rs"),
            number: 42,
        };
        for s in [
            "o/r.rs#42",
            "https://github.com/o/r.rs/issues/42",
            "https://github.com/o/r.rs/pull/42",
            "https://github.com/o/r.rs/pull/42/files",
            "github.com/o/r.rs/issues/42#issuecomment-1",
        ] {
            assert_eq!(s.parse::<ItemRef>().unwrap(), want, "{s}");
        }
        for s in ["o/r", "o/r#x", "https://github.com/o/r/commits/42"] {
            assert!(s.parse::<ItemRef>().is_err(), "{s}");
        }
        assert_eq!(want.to_string(), "o/r.rs#42");
    }
}
