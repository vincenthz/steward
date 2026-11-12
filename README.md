# steward

The operational work around software projects, on the command line: issue
triage, pull request review, and the plumbing an agent needs to do more later
(GitHub, a repository cache, a sandbox). The judgment comes from a model
running locally on [biv](https://github.com/vincenthz/biv): nothing leaves the
machine but the GitHub calls.

```sh
cargo build --release
steward auth status
steward issue triage rust-lang/rustlings#2200          # classify, prioritise, choose labels
steward issue triage rust-lang/rustlings#2200 --apply  # and add the labels
steward pr review https://github.com/o/r/pull/12       # print a review
steward pr review o/r#12 --post                        # and post it (as a comment, never an approval)
steward repo checkout o/r --pr 12                      # ./r on the PR's branch, cloned through the cache
steward sandbox run -C r -- cargo test                 # run its tests confined
```

## Commands

| command | does |
|---|---|
| `auth status` | the token used and where it was found, the user, the rate limit |
| `issue list REPO [--state] [-l LABEL] [-n N]` | issues, most recently updated first (no PRs) |
| `issue show ITEM` | an issue and its comments |
| `issue triage ITEM [--apply] [--json]` | **model**: kind, priority, labels, summary, what's missing, next step |
| `issue comment ITEM -m TEXT` | comment (`-m -` reads stdin) |
| `issue label ITEM [-a L] [-r L]` | add / remove labels |
| `pr list REPO` / `pr show ITEM [--diff]` | pull requests, a PR with its files |
| `pr review ITEM [--post]` | **model**: summary, issues by severity with `file:line`, suggestions |
| `pr create REPO --head B [--base B] -t TITLE [-m BODY] [--draft]` | open a PR |
| `repo sync REPO...` | clone into the cache, or fetch |
| `repo list` | cached repositories, size, last sync |
| `repo checkout REPO [DIR] [--rev R \| --pr N] [-b BRANCH] [--offline]` | a clone in `DIR` (default `./NAME`) through the cache, `origin` on GitHub: see below |
| `repo remove REPO` | delete a cached repository (clones keep working) |
| `sandbox check [--dir D]` | the backend and the policy |
| `sandbox run [-C DIR] [--net] [-w PATH] [-e VAR] [--timeout S] -- CMD...` | a command, confined |
| `config show` / `config init` | the configuration in effect / write a commented one |

`REPO` is `owner/name` or its URL; `ITEM` is `owner/name#N` or an issue or
pull request URL. Model commands take `-v` (stream the reasoning) and
`-m MODEL`.

## Design

A workspace of small crates, each usable on its own, and a binary that puts
them together:

| crate | |
|---|---|
| `steward-github` | blocking REST client (ureq, rustls): issues, comments, labels, PRs, diffs, reviews, pagination, API errors and rate limits. Token discovery: `$STEWARD_GITHUB_TOKEN`, `$GH_TOKEN`/`$GITHUB_TOKEN` (`$GH_ENTERPRISE_TOKEN` for other hosts), the config, then what `gh auth login` stored: `hosts.yml` or the system keyring (`security` on macOS, `secret-tool` on Linux, go-keyring encodings decoded). No `gh` process. |
| `steward-gitcache` | bare clones at `<cache>/<host>/<owner>/<name>` mirroring branches and tags, PR heads fetched on demand to `refs/pull/N/head`; local clones and worktrees from them. Clones land atomically (cloned aside, then renamed), a file lock serializes processes per repository, git never prompts, and the token goes to git through `GIT_CONFIG_*` environment variables: never on the command line or in the repository's config. |
| `steward-sandbox` | Seatbelt (`sandbox-exec`) on macOS, bubblewrap on Linux. Default policy: writes only to the working directory and a private `$TMPDIR`; no IP network; `~/.ssh`, `~/.config/gh`, cloud and registry credentials, keychains unreadable; the environment cleared to a basic set (so `GH_TOKEN` and friends never reach the command). Captured runs have a timeout that kills the whole process group. |
| `steward-brain` | biv behind a small API: `with_brain(cfg, \|brain\| ...)` loads the model once; `ask` (text), `call` (the answer as a function call, for structure, reminded once if the model doesn't call), `conversation` (multi-turn, tool results: the base for agent loops). A thinking budget stops runaway reasoning and asks for the answer with thinking off. |
| `steward-core` | what the commands do, as a library: the config (`~/.config/steward/config.toml`), `Ctx` (the GitHub client, the cache, the sandbox policy), checkouts through the cache, triage and review with their prompts. Nothing prints: operations return their results, model progress goes to an observer, and outward actions (`Triaged::apply`, `Review::post`) are separate calls. |
| `steward` | the command line on steward-core: arguments, output, progress on stderr. |

Principles kept so far:

- **Outward actions are explicit.** Model commands print; only `--apply` /
  `--post` write to GitHub. Reviews are posted as comments, never approvals,
  with a footer saying a local model wrote them. Triage only *adds* labels,
  and only labels the repository has (others are reported, not created).
- **Issue and PR text is untrusted.** Prompts fence it in tags (closing tags
  inside defused) and tell the model it is data, not instructions.
- **Material fits the context**, measured with the model's tokenizer: issue
  threads and diffs are cut at a line, and the model is told what it didn't
  see. Lockfiles and minified files are left out of reviewed diffs.

Paths: config `$STEWARD_CONFIG_DIR` or `$XDG_CONFIG_HOME/steward` or
`~/.config/steward`; cache (`<host>/<owner>/<name>` repositories,
`sandbox/`) `cache.dir` or `$XDG_CACHE_HOME/steward` or `~/.cache/steward`.

### Checkouts

`repo checkout` syncs the cached repository, then clones it like `git clone`
would, from the cache: objects are hardlinked (same file system), so it is
quick and the clone doesn't depend on the cache afterwards. `origin` is then
set to the repository on GitHub, over ssh or https as configured for the
host (else gh's `git_protocol`, else https), so fetch, pull and push are plain git with your
own credentials; steward's token is never written to the clone.

```toml
[git.protocol]
"github.com" = "ssh"
"github.example.com" = "https"
```

- no revision: the default branch, tracking `origin`'s
- `--rev BRANCH`: that branch, tracking `origin`'s; `--rev TAG|COMMIT`:
  detached; `-b NAME` makes a branch there instead
- `--pr N`: the PR's head on a branch named like its branch (or `pr-N` if
  taken, or `-b NAME`), tracking the head branch where it lives: `origin`, or
  for a fork a remote named after its owner, so `git pull` and `git push`
  update the PR (pushing to a fork needs "allow edits by maintainers"). If
  the fork was deleted, or with `--offline`, the branch tracks
  `refs/pull/N/head`: pull only.

## Model

Without `brain.model`, steward takes the first downloaded of
`hf.co/unsloth/Qwen3.6-35B-A3B-MTP-GGUF:Q8_0` (the mixture of experts, much
faster) and `hf.co/unsloth/Qwen3.8-27B-GGUF:Q4_1`. On an M2 Max with the
35B-A3B, a triage takes ~25 s and a review of a 200-line PR ~75 s, loading
included.

## Known gaps

- Sandbox: on macOS name resolution still works (via the system resolver's
  socket); everything outside the hidden paths stays readable. The bubblewrap
  backend is written but untested.
- No Windows support (the sandbox and the keyring lookup are Unix).
- GitHub REST only: no GraphQL, no webhooks, no app authentication.

## Roadmap

Each step builds on the ones before; the crates above are the foundation.

1. **Triage at scale**: `issue triage REPO --since` over a batch, with
   duplicate detection (similar open issues in the prompt), a stored record of
   what was triaged, and an `--apply` that can also comment asking for missing
   information.
2. **Investigate**: an agent loop on `Conversation` with read-only tools
   (read, grep, git log/blame) over a cache checkout, reporting a diagnosis
   and a proposed fix for an issue.
3. **Fix and open PRs**: the same loop with edit tools and `sandbox run` for
   builds and tests, on a branch of a worktree, pushed and opened with
   `pr create`, a human approving before anything leaves.
4. **Deeper reviews**: inline comments on lines, the PR checked out and its
   tests run in the sandbox, context from the files around the diff.
5. **Project health**: stale issues and PRs, docs drift, dependency updates,
   CI failures; a `steward report REPO`.
6. **Contributors**: answering "how do I…" issues from the code and docs,
   pointing to good first issues, guiding a PR through review.
7. **Orchestration**: a work queue, subagents with their own roles (as in
   vcode), scheduling, and a long-running mode driven by notifications.
