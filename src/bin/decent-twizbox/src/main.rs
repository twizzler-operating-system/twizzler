use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use twizbox::agent::{Agent, AgentConfig, ApprovalPolicy, build_welcome};
use twizbox::model::{ModelClient, OllamaModelClient, OpenRouterModelClient};
use twizbox::session::SessionStore;
use twizbox::workspace::WorkspaceContext;

mod readline;

const HELP_DETAILS: &str = "\
Commands:
/help    Show this help message.
/memory  Show the agent's distilled working memory.
/session Show the path to the saved session file.
/reset   Clear the current session history and memory.
/exit    Exit the agent.";

// Optional local defaults baked into the binary at compile time, so this
// doesn't have to be typed out (or exported) on every run. Both files live
// in `local/` (gitignored -- see build.rs, which creates them empty if
// missing) and are read as plain text; an empty/missing file just means
// "no default set". Populate them with e.g.:
//   printf '%s' "sk-or-v1-..." > local/openrouter_api_key.txt
//   printf '%s' "anthropic/claude-sonnet-4.5" > local/default_model.txt
const EMBEDDED_OPENROUTER_KEY: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/local/openrouter_api_key.txt"));
const EMBEDDED_DEFAULT_MODEL: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/local/default_model.txt"));

fn embedded_api_key() -> Option<&'static str> {
    let key = std::str::from_utf8(EMBEDDED_OPENROUTER_KEY).ok()?.trim();
    (!key.is_empty()).then_some(key)
}

fn embedded_default_model() -> Option<&'static str> {
    let model = EMBEDDED_DEFAULT_MODEL.trim();
    (!model.is_empty()).then_some(model)
}

/// Which backend serves model completions.
///
/// Adding a new backend: implement `ModelClient` in `src/model/`, add a
/// variant here, and add a case in `build_model_client` below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Provider {
    Ollama,
    Openrouter,
}

impl Provider {
    fn as_str(self) -> &'static str {
        match self {
            Provider::Ollama => "ollama",
            Provider::Openrouter => "openrouter",
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Minimal coding agent for local or hosted models.",
    disable_help_subcommand = true
)]
struct Args {
    #[arg(help = "Optional one-shot prompt.")]
    prompt: Vec<String>,

    #[arg(long, default_value = ".", help = "Workspace directory.")]
    cwd: String,

    #[arg(
        long,
        value_enum,
        default_value_t = Provider::Openrouter,
        help = "Which backend to use."
    )]
    provider: Provider,

    #[arg(
        long,
        help = "Model name: an Ollama tag (e.g. qwen3.5:4b) or an OpenRouter model id (e.g. openai/gpt-4o-mini). Defaults to local/default_model.txt if present, otherwise a provider-specific fallback."
    )]
    model: Option<String>,

    #[arg(
        long,
        default_value = "http://127.0.0.1:11434",
        help = "Ollama server URL. Only used with --provider ollama."
    )]
    host: String,

    #[arg(
        long,
        default_value = "https://openrouter.ai/api/v1",
        help = "OpenRouter API base URL. Only used with --provider openrouter."
    )]
    openrouter_base_url: String,

    #[arg(
        long,
        help = "OpenRouter API key. Falls back to OPENROUTER_API_KEY, then to local/openrouter_api_key.txt. Only used with --provider openrouter."
    )]
    api_key: Option<String>,

    #[arg(long, default_value_t = 300, help = "Model request timeout in seconds.")]
    timeout: u64,

    #[arg(long, help = "Session id to resume or 'latest'.")]
    resume: Option<String>,

    #[arg(
        long,
        default_value = "ask",
        value_parser = ["ask", "auto", "never"],
        help = "Approval policy for risky tools; auto grants the model arbitrary command execution and file writes."
    )]
    approval: String,

    #[arg(
        long,
        default_value_t = 10,
        help = "Maximum tool/model iterations per request."
    )]
    max_steps: usize,

    #[arg(
        long,
        default_value_t = 2048,
        help = "Maximum model output tokens per step. 512 (the old default) is too tight for hosted reasoning models; lower it for small local Ollama models if you want faster steps."
    )]
    max_new_tokens: usize,

    #[arg(
        long,
        default_value_t = 0.2,
        help = "Sampling temperature sent to the model."
    )]
    temperature: f64,

    #[arg(long, default_value_t = 0.9, help = "Top-p sampling value sent to the model.")]
    top_p: f64,
}

/// Model name to use if `--model` wasn't given: the CLI flag wins, then the
/// embedded `local/default_model.txt`, then a provider-specific fallback.
fn resolved_model(args: &Args) -> String {
    if let Some(model) = &args.model {
        return model.clone();
    }
    if let Some(model) = embedded_default_model() {
        return model.to_string();
    }
    match args.provider {
        Provider::Ollama => "qwen3.5:4b",
        // OpenRouter's built-in router; picks a reasonable model per-request.
        Provider::Openrouter => "openrouter/auto",
    }
    .to_string()
}

fn build_model_client(args: &Args) -> Result<Arc<dyn ModelClient>> {
    let model = resolved_model(args);
    match args.provider {
        Provider::Ollama => Ok(Arc::new(OllamaModelClient::new(
            model,
            args.host.clone(),
            args.temperature,
            args.top_p,
            args.timeout,
        ))),
        Provider::Openrouter => {
            let api_key = args
                .api_key
                .clone()
                .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
                .or_else(|| embedded_api_key().map(str::to_string))
                .context(
                    "OpenRouter requires an API key: pass --api-key, set OPENROUTER_API_KEY, or put it in local/openrouter_api_key.txt",
                )?;
            Ok(Arc::new(OpenRouterModelClient::new(
                model,
                args.openrouter_base_url.clone(),
                api_key,
                args.temperature,
                args.top_p,
                args.timeout,
            )))
        }
    }
}

fn build_agent(args: &Args) -> Result<Agent> {
    let workspace = WorkspaceContext::build(&args.cwd);
    let store = SessionStore::new(workspace.repo_root.join(".twizbox").join("sessions"))?;
    let model = build_model_client(args)?;
    let session_id = match args.resume.as_deref() {
        Some("latest") => store.latest()?,
        other => other.map(ToString::to_string),
    };
    let config = AgentConfig {
        approval_policy: ApprovalPolicy::parse(&args.approval)?,
        max_steps: args.max_steps,
        max_new_tokens: args.max_new_tokens,
        ..AgentConfig::default()
    };
    if let Some(session_id) = session_id {
        Agent::from_session(model, workspace, store, &session_id, config)
    } else {
        Agent::new(model, workspace, store, None, config)
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut agent = build_agent(&args)?;

    println!(
        "{}",
        build_welcome(&agent, &resolved_model(&args), args.provider.as_str())
    );

    if !args.prompt.is_empty() {
        let prompt = args.prompt.join(" ").trim().to_string();
        if !prompt.is_empty() {
            println!();
            println!("{}", agent.ask(&prompt)?);
        }
        return Ok(());
    }

    let mut history: Vec<String> = Vec::new();
    loop {
        println!();
        let Some(user_input) = readline::read_line("twizbox> ", &mut history)? else {
            println!();
            return Ok(());
        };
        let user_input = user_input.trim();
        if user_input.is_empty() {
            continue;
        }
        match user_input {
            "/exit" | "/quit" => return Ok(()),
            "/help" => {
                println!("{HELP_DETAILS}");
                continue;
            }
            "/memory" => {
                println!("{}", agent.memory_text());
                continue;
            }
            "/session" => {
                println!("{}", agent.session_path().display());
                continue;
            }
            "/reset" => {
                agent.reset()?;
                println!("session reset");
                continue;
            }
            _ => {}
        }

        println!();
        match agent.ask(user_input) {
            Ok(answer) => println!("{answer}"),
            Err(error) => eprintln!("{error:?}"),
        }
    }
}
