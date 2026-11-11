//! The commands that need the model

pub mod review;
pub mod triage;

use std::io::{IsTerminal, Write};

use steward_brain::{Brain, Effort, Progress};

use crate::ctx::Ctx;
use crate::{AiArgs, Result};

/// Tokens kept for the instructions and the framing of a prompt
const PROMPT_OVERHEAD: usize = 2048;

/// Loads the model (telling so on stderr) and gives it to `f`
pub fn with_brain<R>(
    ctx: &Ctx,
    args: &AiArgs,
    f: impl FnOnce(&mut Brain<'_>) -> Result<R>,
) -> Result<R> {
    let b = &ctx.cfg.brain;
    let effort = Effort::parse(&b.effort)
        .ok_or_else(|| format!("brain.effort: `{}`: expected low, medium or high", b.effort))?;
    let model = args
        .model
        .clone()
        .or_else(|| b.model.clone())
        .unwrap_or_else(steward_brain::default_model);
    let cfg = steward_brain::Config {
        model,
        cpu: b.cpu,
        context: b.context,
        thinking: b.thinking,
        effort,
        max_tokens: b.max_tokens,
        temperature: b.temperature,
        lookup: b.lookup,
        thinking_budget: b.thinking_budget,
    };
    eprintln!("loading {}…", cfg.model);
    steward_brain::with_brain(&cfg, |brain| {
        eprintln!(
            "{} on {}, {} tokens of context",
            brain.name(),
            brain.backend(),
            brain.context()
        );
        f(brain)
    })?
}

/// Tokens the material of a prompt may take: the context, less the answer
/// and the instructions
pub fn material_budget(brain: &Brain<'_>, max_tokens: usize) -> usize {
    brain
        .context()
        .saturating_sub(max_tokens + PROMPT_OVERHEAD)
        .max(1024)
}

/// The longest start of `text` (cut at a line) that takes at most `budget`
/// tokens, and whether it was cut
pub fn fit<'t>(brain: &Brain<'_>, text: &'t str, budget: usize) -> (&'t str, bool) {
    // a first cut by size: text takes at least ~2 bytes per token
    let (mut cut, mut clipped) = crate::text::clip(text, budget * 6);
    for _ in 0..8 {
        let n = brain.count_tokens(cut);
        if n <= budget {
            return (cut, clipped);
        }
        let target = (cut.len() as f64 * budget as f64 / n as f64 * 0.95) as usize;
        (cut, _) = crate::text::clip(cut, target);
        clipped = true;
    }
    (cut, clipped)
}

/// Shows the model's progress on stderr: its reasoning as it goes when
/// verbose, else a counter
pub struct Monitor {
    verbose: bool,
    tty: bool,
    thinking: usize,
    answer: usize,
    phase: &'static str,
}

impl Monitor {
    pub fn new(verbose: bool) -> Self {
        Monitor {
            verbose,
            tty: std::io::stderr().is_terminal(),
            thinking: 0,
            answer: 0,
            phase: "reading",
        }
    }

    pub fn observe(&mut self, p: Progress<'_>) {
        let mut err = std::io::stderr().lock();
        match p {
            Progress::PromptDone => self.phase = "thinking",
            Progress::Thinking(t) => {
                self.thinking += 1;
                if self.verbose {
                    let _ = write!(err, "\x1b[2m{t}\x1b[0m");
                    return;
                }
            }
            Progress::ThinkingDone => {
                self.phase = "writing";
                if self.verbose {
                    let _ = writeln!(err);
                    return;
                }
            }
            Progress::Answer(_) => {
                self.answer += 1;
                self.phase = "writing";
            }
            Progress::Call(_) => self.phase = "answering",
        }
        if self.tty && !self.verbose {
            let _ = write!(
                err,
                "\r\x1b[2K{}… {} tokens of reasoning, {} of answer",
                self.phase, self.thinking, self.answer
            );
        }
    }

    pub fn done(&mut self) {
        if self.tty && !self.verbose {
            eprint!("\r\x1b[2K");
        }
    }
}
