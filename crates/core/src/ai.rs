//! The model: loading it as configured, fitting material to its context,
//! and what it reports while it works

use std::time::Duration;

use steward_brain::{Brain, Effort, Progress, Reply};

use crate::{Ctx, Result};

/// Tokens kept for the instructions and the framing of a prompt
const PROMPT_OVERHEAD: usize = 2048;

/// Choices of a model operation beyond the configuration
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// the model, instead of the configured one
    pub model: Option<String>,
}

/// What happens during a model operation, to show progress
pub enum Event<'a> {
    /// the model is being loaded
    Loading { model: &'a str },
    Loaded {
        name: &'a str,
        backend: &'a str,
        context: usize,
    },
    /// the material didn't fit the context: `shown` of its `total` lines go
    /// to the model
    Clipped {
        what: &'a str,
        shown: usize,
        total: usize,
    },
    /// the model's own progress
    Model(Progress<'a>),
}

/// Observes a model operation; `&mut |_| {}` to ignore it
pub type Observer<'o> = &'o mut dyn FnMut(Event<'_>);

/// What a model's answer cost
#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub generated_tokens: usize,
    pub elapsed: Duration,
    /// the answer was stopped by the token limit (`brain.max_tokens`)
    pub truncated: bool,
}

impl From<&Reply> for Usage {
    fn from(r: &Reply) -> Self {
        Usage {
            prompt_tokens: r.prompt_tokens,
            generated_tokens: r.generated_tokens,
            elapsed: r.elapsed,
            truncated: r.truncated,
        }
    }
}

/// The brain's configuration, from steward's and the options
pub fn brain_config(ctx: &Ctx, opts: &Options) -> Result<steward_brain::Config> {
    let b = &ctx.cfg.brain;
    let effort = Effort::parse(&b.effort)
        .ok_or_else(|| format!("brain.effort: `{}`: expected low, medium or high", b.effort))?;
    let model = opts
        .model
        .clone()
        .or_else(|| b.model.clone())
        .unwrap_or_else(steward_brain::default_model);
    Ok(steward_brain::Config {
        model,
        cpu: b.cpu,
        context: b.context,
        thinking: b.thinking,
        effort,
        max_tokens: b.max_tokens,
        temperature: b.temperature,
        lookup: b.lookup,
        thinking_budget: b.thinking_budget,
    })
}

/// Loads the model (telling `on`) and gives it to `f`, with the observer
pub fn with_brain<R>(
    ctx: &Ctx,
    opts: &Options,
    on: Observer<'_>,
    f: impl FnOnce(&mut Brain<'_>, Observer<'_>) -> Result<R>,
) -> Result<R> {
    let cfg = brain_config(ctx, opts)?;
    on(Event::Loading { model: &cfg.model });
    steward_brain::with_brain(&cfg, |brain| {
        on(Event::Loaded {
            name: brain.name(),
            backend: &brain.backend(),
            context: brain.context(),
        });
        f(brain, on)
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
