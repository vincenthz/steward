//! The model steward thinks with: a local Qwen on [biv], loaded once per
//! process, and conversations on it.
//!
//! The model and the engine borrow the memory-mapped file, so they live in
//! the scope of [`with_brain`]. One conversation runs at a time (biv runs one
//! sequence at a time), each starting from nothing; [`Brain::call`] makes the
//! model answer through a function, for answers with a structure.

use std::cell::Cell;
use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use biv::chat::{Chat, ChatOptions, Event};
use biv::engine::Engine;
use biv::model::Model;
use biv::sampler::{Sampler, SamplerConfig};
use biv::tokenizer::Tokenizer;
use serde_json::{Value, json};

pub use biv::chat::{ReasoningEffort as Effort, ToolCall};

/// The models used when none is configured, the first one downloaded. The
/// mixture of experts suits work made of many short turns best.
pub const DEFAULT_MODELS: [&str; 3] = [
    "hf.co/unsloth/Qwen3.6-35B-A3B-MTP-GGUF:Q8_0",
    "hf.co/unsloth/Qwen3.8-27B-GGUF:Q4_1",
    "hf.co/unsloth/Qwen3.8-27B-GGUF:Q8_0",
];

pub fn default_model() -> String {
    DEFAULT_MODELS
        .iter()
        .find(|m| biv_cli::hf::resolve(m).is_ok())
        .unwrap_or(&DEFAULT_MODELS[0])
        .to_string()
}

/// Where a model name leads: a GGUF file, or the reason it can't be found
pub fn resolve(model: &str) -> Result<PathBuf, String> {
    biv_cli::hf::resolve(model)
}

#[derive(Debug, Clone)]
pub struct Config {
    /// a GGUF file, or a model of the Hugging Face cache
    /// (`hf.co/owner/repo:QUANT`)
    pub model: String,
    /// the CPU backend rather than Metal
    pub cpu: bool,
    /// tokens per conversation
    pub context: usize,
    pub thinking: bool,
    pub effort: Effort,
    /// per answer, thinking included
    pub max_tokens: usize,
    /// the model's recommended one if `None`
    pub temperature: Option<f32>,
    /// tokens proposed per step from earlier text (biv's lookup decoding)
    pub lookup: usize,
    /// tokens of reasoning per answer, after which the model is stopped and
    /// asked for its answer without more thinking
    pub thinking_budget: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            model: String::new(),
            cpu: false,
            context: 32768,
            thinking: true,
            effort: Effort::Medium,
            max_tokens: 8192,
            temperature: None,
            lookup: 0,
            thinking_budget: 4096,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    /// the model can't be found or loaded
    Model(String),
    /// the engine failed (out of memory, context full...)
    Engine(biv::Error),
    /// the model answered without calling the function it had to
    NoCall { function: String, answer: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Model(e) => write!(f, "model: {e}"),
            Error::Engine(e) => write!(f, "inference: {e}"),
            Error::NoCall { function, answer } => {
                let answer: String = answer.chars().take(400).collect();
                write!(f, "the model did not call `{function}`; it said: {answer}")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<biv::Error> for Error {
    fn from(e: biv::Error) -> Self {
        Error::Engine(e)
    }
}

/// What happens while the model works, to show progress
pub enum Progress<'a> {
    /// the prompt is processed, generation starts
    PromptDone,
    Thinking(&'a str),
    ThinkingDone,
    Answer(&'a str),
    Call(&'a ToolCall),
}

/// Loads the model and gives it to `f`
pub fn with_brain<R>(cfg: &Config, f: impl FnOnce(&mut Brain<'_>) -> R) -> Result<R, Error> {
    let path = resolve(&cfg.model).map_err(Error::Model)?;
    let gguf = biv_cli::open_gguf(&path).map_err(|e| Error::Model(e.to_string()))?;
    let tok = Tokenizer::from_gguf(&gguf)?;
    let model = Model::load(&gguf)?;
    let engine = make_engine(&model, cfg.cpu)?;

    let sampling = &gguf.metadata.general.sampling;
    let defaults = SamplerConfig::default();
    let sampler = SamplerConfig {
        temperature: cfg
            .temperature
            .or(sampling.temp)
            .unwrap_or(defaults.temperature),
        top_k: sampling.top_k.map(|v| v as usize).unwrap_or(defaults.top_k),
        top_p: sampling.top_p.unwrap_or(defaults.top_p),
        ..defaults
    };
    let options = ChatOptions {
        thinking: cfg.thinking,
        effort: cfg.effort,
        max_tokens: cfg.max_tokens,
        context: cfg.context.min(model.cfg.context_length),
        lookup: cfg.lookup,
        ..Default::default()
    };
    let mut brain = Brain {
        name: gguf
            .metadata
            .general
            .name
            .clone()
            .unwrap_or_else(|| "model".into()),
        path: path.clone(),
        engine,
        tok: &tok,
        sampler,
        options,
        thinking_budget: cfg.thinking_budget,
    };
    Ok(f(&mut brain))
}

fn make_engine<'m>(model: &'m Model<'m>, cpu: bool) -> Result<Box<dyn Engine + 'm>, Error> {
    #[cfg(target_os = "macos")]
    if !cpu {
        return Ok(Box::new(biv::metal::MetalEngine::new(model, 256)?));
    }
    let _ = cpu;
    Ok(Box::new(model))
}

pub struct Brain<'m> {
    name: String,
    path: PathBuf,
    engine: Box<dyn Engine + 'm>,
    tok: &'m Tokenizer,
    sampler: SamplerConfig,
    options: ChatOptions,
    thinking_budget: usize,
}

impl<'m> Brain<'m> {
    /// The model's name, from its metadata
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The backend: Metal and the GPU, or the CPU
    pub fn backend(&self) -> String {
        self.engine.description()
    }

    /// Tokens per conversation
    pub fn context(&self) -> usize {
        self.options.context
    }

    /// The number of tokens of `text`, to fit material in the context
    pub fn count_tokens(&self, text: &str) -> usize {
        self.tok.encode(text).len()
    }

    /// A new conversation, the model knowing `system` and able to call `tools`
    pub fn conversation(&self, system: &str, tools: &[Tool]) -> Result<Conversation<'_>, Error> {
        let opts = ChatOptions {
            system: Some(system.to_string()),
            tools: tools.iter().map(Tool::definition).collect(),
            ..self.options.clone()
        };
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
            | 1;
        Ok(Conversation {
            chat: Chat::new(self.engine.as_ref(), self.tok, opts)?,
            sampler: Sampler::new(SamplerConfig {
                seed,
                ..self.sampler.clone()
            }),
            thinking_budget: self.thinking_budget,
        })
    }

    /// One question, one answer
    pub fn ask(
        &self,
        system: &str,
        prompt: &str,
        progress: &mut dyn FnMut(Progress<'_>),
    ) -> Result<Reply, Error> {
        self.conversation(system, &[])?.send(prompt, progress)
    }

    /// One question, answered by calling `function` (reminded once if it
    /// doesn't)
    pub fn call(
        &self,
        system: &str,
        prompt: &str,
        function: &Tool,
        progress: &mut dyn FnMut(Progress<'_>),
    ) -> Result<(ToolCall, Reply), Error> {
        let mut conv = self.conversation(system, std::slice::from_ref(function))?;
        let mut reply = conv.send(prompt, progress)?;
        for attempt in 0..2 {
            if let Some(c) = reply.calls.iter().find(|c| c.name == function.name) {
                return Ok((c.clone(), reply));
            }
            if attempt == 0 {
                let reminder = format!(
                    "Give your answer now by calling the function `{}`, with every required parameter.",
                    function.name
                );
                reply = if reply.calls.is_empty() {
                    conv.send(&reminder, progress)?
                } else {
                    // it called something that doesn't exist
                    let unknown = vec![format!("Unknown function. {reminder}"); reply.calls.len()];
                    conv.respond(&unknown, progress)?
                };
            }
        }
        Err(Error::NoCall {
            function: function.name.clone(),
            answer: reply.text,
        })
    }
}

/// The answer to a message
#[derive(Debug, Default, Clone)]
pub struct Reply {
    pub text: String,
    pub thinking: String,
    pub calls: Vec<ToolCall>,
    pub prompt_tokens: usize,
    pub generated_tokens: usize,
    /// stopped by the token limit rather than by the model
    pub truncated: bool,
    pub elapsed: Duration,
}

/// A conversation with the model
pub struct Conversation<'b> {
    chat: Chat<'b>,
    sampler: Sampler,
    thinking_budget: usize,
}

/// What the model is sent
enum Message<'a> {
    User(&'a str),
    Results(&'a [String]),
}

/// Sent when the model reasoned past its budget
const ENOUGH_THINKING: &str = "Your reasoning time is up. Without further deliberation, give your final answer now, as it was asked.";

impl Conversation<'_> {
    /// Tokens used so far
    pub fn tokens(&self) -> usize {
        self.chat.n_tokens()
    }

    pub fn send(
        &mut self,
        message: &str,
        progress: &mut dyn FnMut(Progress<'_>),
    ) -> Result<Reply, Error> {
        self.turn(Message::User(message), progress)
    }

    /// Answers the calls of the last reply, with their results in order
    pub fn respond(
        &mut self,
        results: &[String],
        progress: &mut dyn FnMut(Progress<'_>),
    ) -> Result<Reply, Error> {
        self.turn(Message::Results(results), progress)
    }

    /// A message and its answer; when the model reasons past its budget, it
    /// is stopped and asked for its answer with thinking off
    fn turn(
        &mut self,
        message: Message<'_>,
        progress: &mut dyn FnMut(Progress<'_>),
    ) -> Result<Reply, Error> {
        let start = Instant::now();
        let (mut reply, overthought) = self.exchange(message, true, progress)?;
        if !overthought {
            return Ok(finish(reply, start));
        }
        let thinking = std::mem::replace(&mut self.chat.opts.thinking, false);
        let second = self.exchange(Message::User(ENOUGH_THINKING), false, progress);
        self.chat.opts.thinking = thinking;
        let (second, _) = second?;
        reply.text = second.text;
        reply.calls = second.calls;
        reply.prompt_tokens += second.prompt_tokens;
        reply.generated_tokens += second.generated_tokens;
        reply.truncated = second.truncated;
        Ok(finish(reply, start))
    }

    /// One exchange with the engine, and whether the reasoning was cut by
    /// the budget
    fn exchange(
        &mut self,
        message: Message<'_>,
        budgeted: bool,
        progress: &mut dyn FnMut(Progress<'_>),
    ) -> Result<(Reply, bool), Error> {
        let mut reply = Reply::default();
        let thinking = Cell::new(self.chat.opts.thinking);
        let thought = Cell::new(0usize);
        let budget = self.thinking_budget;
        let overthought = || budgeted && budget > 0 && thinking.get() && thought.get() >= budget;
        let mut on = |e: Event<'_>| {
            match &e {
                Event::Thinking(_) => thought.set(thought.get() + 1),
                Event::ThinkingDone => thinking.set(false),
                _ => {}
            }
            on_event(e, &mut reply, progress)
        };
        let stats = match message {
            Message::User(m) => self.chat.send(m, &mut self.sampler, &mut on, overthought),
            Message::Results(r) => {
                self.chat
                    .send_tool_responses(r, &mut self.sampler, &mut on, overthought)
            }
        }?;
        let cut = overthought();
        reply.calls = stats.tool_calls;
        reply.prompt_tokens = stats.prompt_tokens;
        reply.generated_tokens = stats.gen_tokens;
        reply.truncated = stats.truncated && !cut;
        Ok((reply, cut))
    }
}

fn on_event(e: Event<'_>, reply: &mut Reply, progress: &mut dyn FnMut(Progress<'_>)) {
    match e {
        Event::PromptDone => progress(Progress::PromptDone),
        Event::Thinking(t) => {
            reply.thinking.push_str(t);
            progress(Progress::Thinking(t));
        }
        Event::ThinkingDone => progress(Progress::ThinkingDone),
        Event::Answer(t) => {
            reply.text.push_str(t);
            progress(Progress::Answer(t));
        }
        Event::ToolCall(c) => progress(Progress::Call(c)),
    }
}

fn finish(mut reply: Reply, start: Instant) -> Reply {
    reply.text = reply.text.trim().to_string();
    reply.elapsed = start.elapsed();
    reply
}

/// A function the model may call
#[derive(Debug, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    params: Vec<(String, Value, bool)>,
}

impl Tool {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Tool {
            name: name.into(),
            description: description.into(),
            params: Vec::new(),
        }
    }

    fn param(mut self, name: &str, schema: Value, required: bool) -> Self {
        self.params.push((name.to_string(), schema, required));
        self
    }

    pub fn string(self, name: &str, description: &str) -> Self {
        self.param(
            name,
            json!({"type": "string", "description": description}),
            true,
        )
    }

    pub fn optional_string(self, name: &str, description: &str) -> Self {
        self.param(
            name,
            json!({"type": "string", "description": description}),
            false,
        )
    }

    /// One of `values`
    pub fn choice(self, name: &str, values: &[&str], description: &str) -> Self {
        self.param(
            name,
            json!({"type": "string", "enum": values, "description": description}),
            true,
        )
    }

    pub fn strings(self, name: &str, description: &str) -> Self {
        self.param(
            name,
            json!({"type": "array", "items": {"type": "string"}, "description": description}),
            true,
        )
    }

    pub fn integer(self, name: &str, description: &str) -> Self {
        self.param(
            name,
            json!({"type": "integer", "description": description}),
            true,
        )
    }

    /// The definition, as the chat template writes it
    pub fn definition(&self) -> String {
        let properties: serde_json::Map<String, Value> = self
            .params
            .iter()
            .map(|(n, s, _)| (n.clone(), s.clone()))
            .collect();
        let required: Vec<&str> = self
            .params
            .iter()
            .filter(|(_, _, r)| *r)
            .map(|(n, _, _)| n.as_str())
            .collect();
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": {"type": "object", "properties": properties, "required": required},
            }
        })
        .to_string()
    }
}

/// Reading the arguments of a call
pub trait Arguments {
    fn arg(&self, name: &str) -> Option<&str>;

    /// A list argument: a JSON array, or else one item per line or comma
    fn list(&self, name: &str) -> Vec<String> {
        let Some(v) = self.arg(name).map(str::trim).filter(|v| !v.is_empty()) else {
            return Vec::new();
        };
        if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(v) {
            return items
                .into_iter()
                .map(|i| match i {
                    Value::String(s) => s,
                    other => other.to_string(),
                })
                .filter(|s| !s.trim().is_empty())
                .collect();
        }
        v.split(['\n', ','])
            .map(|s| {
                s.trim()
                    .trim_start_matches("- ")
                    .trim_matches('"')
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .collect()
    }
}

impl Arguments for ToolCall {
    fn arg(&self, name: &str) -> Option<&str> {
        self.arguments
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(args: &[(&str, &str)]) -> ToolCall {
        ToolCall {
            name: "f".into(),
            arguments: args
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        }
    }

    #[test]
    fn lists() {
        assert_eq!(call(&[("l", r#"["a", "b c"]"#)]).list("l"), ["a", "b c"]);
        assert_eq!(call(&[("l", "a, b")]).list("l"), ["a", "b"]);
        assert_eq!(call(&[("l", "- a\n- b\n")]).list("l"), ["a", "b"]);
        assert!(call(&[("l", "[]")]).list("l").is_empty());
        assert!(call(&[]).list("l").is_empty());
    }

    #[test]
    fn definition() {
        let t = Tool::new("classify", "Classify")
            .choice("kind", &["bug", "feature"], "the kind")
            .optional_string("note", "a note");
        let v: Value = serde_json::from_str(&t.definition()).unwrap();
        assert_eq!(v["function"]["name"], "classify");
        assert_eq!(v["function"]["parameters"]["required"], json!(["kind"]));
        assert_eq!(
            v["function"]["parameters"]["properties"]["kind"]["enum"],
            json!(["bug", "feature"])
        );
    }
}
