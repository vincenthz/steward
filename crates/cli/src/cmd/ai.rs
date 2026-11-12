//! The commands that need the model: their progress on stderr, their
//! results on stdout

use std::io::{IsTerminal, Write};

use steward_brain::Progress;
use steward_core::Ctx;
use steward_core::ai::{Event, Usage};
use steward_github::ItemRef;

use crate::{AiArgs, Result};

pub fn triage(ctx: &Ctx, item: &ItemRef, apply: bool, json: bool, args: &AiArgs) -> Result {
    let mut monitor = Monitor::new(args.verbose);
    let result =
        steward_core::triage::triage(ctx, item, &args.options(), &mut |e| monitor.observe(e));
    monitor.done();
    let mut t = result?;
    usage(&t.usage);
    if apply {
        t.apply(ctx)?;
    }

    let triage = &t.triage;
    if json {
        println!("{}", serde_json::to_string_pretty(triage)?);
        return Ok(());
    }
    println!("{item}  {}", t.issue.title);
    println!("  kind      {}", triage.kind);
    println!("  priority  {}", triage.priority);
    println!(
        "  labels    {}",
        if triage.labels.is_empty() {
            "(none)".to_string()
        } else {
            triage.labels.join(", ")
        }
    );
    if !triage.unknown_labels.is_empty() {
        println!(
            "            ignored, not labels of the repository: {}",
            triage.unknown_labels.join(", ")
        );
    }
    println!("  summary   {}", triage.summary);
    println!("  why       {}", triage.rationale);
    if let Some(m) = &triage.missing {
        println!("  missing   {m}");
    }
    println!("  next      {}", triage.next_step);
    if apply {
        if triage.applied.is_empty() {
            println!("no label to add");
        } else {
            println!("added {}", triage.applied.join(", "));
        }
    } else if !triage.labels.is_empty() {
        println!("(--apply adds the labels)");
    }
    Ok(())
}

pub fn review(ctx: &Ctx, item: &ItemRef, post: bool, args: &AiArgs) -> Result {
    let mut monitor = Monitor::new(args.verbose);
    let result =
        steward_core::review::review(ctx, item, &args.options(), &mut |e| monitor.observe(e));
    monitor.done();
    let review = result?;
    usage(&review.usage);
    println!("{}", review.text);
    if post {
        eprintln!("posted {}", review.post(ctx)?);
    }
    Ok(())
}

fn usage(u: &Usage) {
    eprintln!(
        "{} prompt tokens, {} generated, {:.1}s",
        u.prompt_tokens,
        u.generated_tokens,
        u.elapsed.as_secs_f32()
    );
    if u.truncated {
        eprintln!("warning: the answer hit the token limit (brain.max_tokens)");
    }
}

/// Shows a model operation's progress on stderr: the model's reasoning as it
/// goes when verbose, else a counter
struct Monitor {
    verbose: bool,
    tty: bool,
    thinking: usize,
    answer: usize,
    phase: &'static str,
}

impl Monitor {
    fn new(verbose: bool) -> Self {
        Monitor {
            verbose,
            tty: std::io::stderr().is_terminal(),
            thinking: 0,
            answer: 0,
            phase: "reading",
        }
    }

    fn observe(&mut self, e: Event<'_>) {
        match e {
            Event::Loading { model } => eprintln!("loading {model}…"),
            Event::Loaded {
                name,
                backend,
                context,
            } => eprintln!("{name} on {backend}, {context} tokens of context"),
            Event::Clipped { what, shown, total } => {
                eprintln!("the {what} is too long: reviewing {shown} of {total} lines")
            }
            Event::Model(p) => self.progress(p),
        }
    }

    fn progress(&mut self, p: Progress<'_>) {
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

    fn done(&mut self) {
        if self.tty && !self.verbose {
            eprint!("\r\x1b[2K");
        }
    }
}
