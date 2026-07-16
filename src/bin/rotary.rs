use clap::{Parser, Subcommand};
use rx4::{print_banner, register_builtin_tools, Agent, Scope, ToolRegistry};
use std::io::BufRead;
use std::sync::Arc;

#[cfg(feature = "computer-use")]
use rx4::computer_use;

#[cfg(feature = "pi-compat")]
use rx4::pi::PiRpcServer;

#[derive(Parser)]
#[command(name = "rx4", version, about = "Agent harness engine (pi-compatible)")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Model to use
    #[arg(long, global = true)]
    model: Option<String>,
    /// Scope (coding, research, plan, ask)
    #[arg(long, global = true)]
    scope: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Interactive REPL (default)
    Chat,
    /// One-shot prompt execution
    Exec {
        prompt: String,
        /// Emit JSON output
        #[arg(long)]
        json: bool,
    },
    /// Start the IPC server on a Unix socket
    Serve { socket: Option<String> },
    /// Run the pi-compatible JSON-RPC server over stdin/stdout
    Rpc,
    /// Print version, feature flags, and module list
    Version,
    /// Check environment, config, and connectivity
    Doctor,
    /// List all registered models
    Models,
    /// List all built-in tools
    Tools,
    /// Login to a provider via OAuth (xAI Grok, OpenAI ChatGPT)
    Login {
        /// Provider to login with (grok, openai)
        provider: String,
    },
}

fn main() {
    let cli = Cli::parse();

    let command = cli.command.unwrap_or(Commands::Chat);

    match command {
        Commands::Chat => run_chat(cli.model, cli.scope),
        Commands::Exec { prompt, json } => run_exec(&prompt, json, cli.model, cli.scope),
        Commands::Serve { socket } => run_serve(socket),
        Commands::Rpc => run_rpc_mode(),
        Commands::Version => run_version(),
        Commands::Doctor => run_doctor(),
        Commands::Models => run_models(),
        Commands::Tools => run_tools(),
        Commands::Login { provider } => run_login(&provider),
    }
}

fn build_agent(model: Option<&str>, scope: Option<&str>) -> Agent {
    let mut agent = Agent::new();
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);

    #[cfg(feature = "computer-use")]
    {
        computer_use::register_tools(&mut tools);
    }

    agent.set_tools(tools);

    if let Some(m) = model {
        agent.set_model(m);
    }

    let scope = scope.and_then(Scope::parse_scope).unwrap_or(Scope::Coding);
    agent.set_scope(scope);

    #[cfg(feature = "providers")]
    if let Some(provider) = setup_provider() {
        agent.set_provider(provider);
    }

    agent
}

#[cfg(feature = "providers")]
fn setup_provider() -> Option<Arc<dyn rx4::Provider>> {
    use rx4::provider::OpenAIProvider;
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return Some(Arc::new(OpenAIProvider::anthropic(key)));
        }
    }
    if let Ok(key) = std::env::var("XAI_API_KEY") {
        if !key.is_empty() {
            return Some(Arc::new(OpenAIProvider::with_base_url(
                "https://api.x.ai/v1",
                key,
                "xai",
                "xAI",
            )));
        }
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.is_empty() {
            return Some(Arc::new(OpenAIProvider::new(key)));
        }
    }
    None
}

#[cfg(not(feature = "providers"))]
fn setup_provider() -> Option<Arc<dyn rx4::Provider>> {
    None
}

fn run_chat(model: Option<String>, scope: Option<String>) {
    print_banner();

    #[cfg(not(feature = "providers"))]
    {
        eprintln!("chat requires the `providers` feature");
        std::process::exit(1);
    }

    #[cfg(feature = "providers")]
    {
        if setup_provider().is_none() {
            eprintln!(
                "warning: no API key found (set ANTHROPIC_API_KEY, XAI_API_KEY, or OPENAI_API_KEY)"
            );
        }

        let mut agent = build_agent(model.as_deref(), scope.as_deref());
        eprintln!(
            "model={} tools={} scope={}",
            agent.model,
            agent.tools.count(),
            agent.scope
        );
        eprintln!("type /help for slash commands, /quit to exit (ctrl-c exits)");

        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("failed to start runtime: {e}");
                std::process::exit(1);
            }
        };

        let stdin = std::io::stdin();
        loop {
            use std::io::Write;
            print!("> ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => break,
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with('/') {
                if handle_slash(trimmed, &mut agent) {
                    break;
                }
                continue;
            }
            agent.subscribe(|event| match event {
                rx4::Event::MessageDelta { delta } => {
                    use std::io::Write;
                    let mut out = std::io::stdout().lock();
                    let _ = out.write_all(delta.as_bytes());
                    let _ = out.flush();
                }
                rx4::Event::ToolCall(call) => {
                    eprintln!("\n[tool: {} {}]", call.name, call.arguments);
                }
                rx4::Event::Error(err) => {
                    eprintln!("\n[error: {err}]");
                }
                _ => {}
            });
            if let Err(e) = rt.block_on(agent.prompt(trimmed)) {
                eprintln!("\nagent error: {e}");
            }
            println!();
        }
    }
}

fn handle_slash(input: &str, agent: &mut Agent) -> bool {
    let mut parts = input.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next();
    match cmd {
        "/quit" | "/exit" => {
            eprintln!("goodbye");
            true
        }
        "/help" => {
            eprintln!("slash commands:");
            eprintln!("  /model <name>   set the model");
            eprintln!("  /tools          list registered tools");
            eprintln!("  /scope <name>   set scope (coding, research, plan, ask)");
            eprintln!("  /sessions       list saved sessions");
            eprintln!("  /new            clear conversation");
            eprintln!("  /save           save current session to ~/.rx4/sessions/");
            eprintln!("  /load <id>      load a session by id");
            eprintln!("  /help           show this help");
            eprintln!("  /quit           exit");
            false
        }
        "/model" => {
            if let Some(m) = arg {
                agent.set_model(m);
                eprintln!("model set to {m}");
            } else {
                eprintln!("current model: {}", agent.model);
            }
            false
        }
        "/tools" => {
            for def in agent.tools.definitions() {
                let name = def.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let desc = def
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                eprintln!("  {name:<12} {desc}");
            }
            eprintln!("{} tools registered", agent.tools.count());
            false
        }
        "/scope" => {
            if let Some(s) = arg {
                if let Some(scope) = Scope::parse_scope(s) {
                    agent.set_scope(scope);
                    eprintln!("scope set to {scope}");
                } else {
                    eprintln!("unknown scope: {s} (try coding, research, plan, ask)");
                }
            } else {
                eprintln!("current scope: {}", agent.scope);
            }
            false
        }
        "/new" => {
            agent.clear_messages();
            eprintln!("conversation cleared");
            false
        }
        "/sessions" => {
            let dir = sessions_dir();
            match std::fs::read_dir(&dir) {
                Ok(entries) => {
                    let mut found = false;
                    for entry in entries.flatten() {
                        if let Some(name) = entry.file_name().to_str() {
                            if name.ends_with(".jsonl") {
                                eprintln!("  {}", name.trim_end_matches(".jsonl"));
                                found = true;
                            }
                        }
                    }
                    if !found {
                        eprintln!("no saved sessions in {}", dir.display());
                    }
                }
                Err(_) => eprintln!("no sessions directory at {}", dir.display()),
            }
            false
        }
        "/save" => {
            let dir = sessions_dir();
            let _ = std::fs::create_dir_all(&dir);
            let id = uuid::Uuid::new_v4().to_string();
            let path = dir.join(format!("{id}.jsonl"));
            let messages = agent.messages.read();
            let mut content = String::new();
            for msg in messages.iter() {
                if let Ok(line) = serde_json::to_string(msg) {
                    content.push_str(&line);
                    content.push('\n');
                }
            }
            match std::fs::write(&path, content) {
                Ok(_) => eprintln!("saved session {id} to {}", path.display()),
                Err(e) => eprintln!("save failed: {e}"),
            }
            false
        }
        "/load" => {
            let id = match arg {
                Some(id) => id,
                None => {
                    eprintln!("usage: /load <id>");
                    return false;
                }
            };
            let path = sessions_dir().join(format!("{id}.jsonl"));
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    agent.clear_messages();
                    let mut messages = agent.messages.write();
                    for line in content.lines() {
                        if let Ok(msg) = serde_json::from_str::<rx4::Message>(line) {
                            messages.push(msg);
                        }
                    }
                    eprintln!("loaded session {id} ({} messages)", messages.len());
                }
                Err(e) => eprintln!("load failed: {e}"),
            }
            false
        }
        other => {
            eprintln!("unknown command: {other} (try /help)");
            false
        }
    }
}

fn sessions_dir() -> std::path::PathBuf {
    home_dir().join(".rx4").join("sessions")
}

fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn run_exec(prompt: &str, json: bool, model: Option<String>, scope: Option<String>) {
    #[cfg(not(feature = "providers"))]
    {
        let _ = (prompt, json, model, scope);
        eprintln!("exec requires the `providers` feature");
        std::process::exit(1);
    }

    #[cfg(feature = "providers")]
    {
        if setup_provider().is_none() {
            eprintln!(
                "error: no API key found (set ANTHROPIC_API_KEY, XAI_API_KEY, or OPENAI_API_KEY)"
            );
            std::process::exit(1);
        }
        let mut agent = build_agent(model.as_deref(), scope.as_deref());
        let output = Arc::new(parking_lot::Mutex::new(String::new()));
        let output_clone = output.clone();
        agent.subscribe(move |event| match event {
            rx4::Event::MessageDelta { delta } => {
                output_clone.lock().push_str(delta);
                if !json {
                    use std::io::Write;
                    let mut out = std::io::stdout().lock();
                    let _ = out.write_all(delta.as_bytes());
                    let _ = out.flush();
                }
            }
            rx4::Event::Error(err) => {
                eprintln!("\n[error: {err}]");
            }
            _ => {}
        });
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("failed to start runtime: {e}");
                std::process::exit(1);
            }
        };
        if let Err(e) = rt.block_on(agent.prompt(prompt)) {
            if json {
                println!(r#"{{"error":"{e}"}}"#);
            } else {
                eprintln!("agent error: {e}");
            }
            std::process::exit(1);
        }
        if json {
            let result = output.lock().clone();
            let json_out = serde_json::json!({
                "model": agent.model,
                "scope": agent.scope.to_string(),
                "output": result,
            });
            println!("{json_out}");
        } else {
            println!();
        }
    }
}

#[cfg(feature = "ipc")]
fn run_serve(socket: Option<String>) {
    use rx4::ipc::IpcServer;
    let socket_path = socket.unwrap_or_else(|| {
        home_dir()
            .join(".rx4")
            .join("rx4.sock")
            .to_string_lossy()
            .to_string()
    });
    let server = IpcServer::new(socket_path);
    if let Err(e) = server.run() {
        eprintln!("ipc server error: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(feature = "ipc"))]
fn run_serve(_socket: Option<String>) {
    eprintln!("serve requires the `ipc` feature");
    std::process::exit(1);
}

#[cfg(feature = "pi-compat")]
fn run_rpc_mode() {
    use rx4::pi::tools::pi_to_rx4_tool;
    let _ = pi_to_rx4_tool;

    let mut agent = Agent::new();
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);

    #[cfg(feature = "computer-use")]
    {
        computer_use::register_tools(&mut tools);
    }

    agent.set_tools(tools);
    agent.set_scope(Scope::Coding);

    let server = PiRpcServer::new(agent);
    if let Err(e) = server.run() {
        eprintln!("rpc server error: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(feature = "pi-compat"))]
fn run_rpc_mode() {
    eprintln!("rpc mode requires pi-compat feature");
    std::process::exit(1);
}

fn run_version() {
    println!("rx4 {}", rx4::VERSION);
    println!("features:");
    println!("  providers:    {}", cfg_feature("providers"));
    println!("  ipc:          {}", cfg_feature("ipc"));
    println!("  pi-compat:    {}", cfg_feature("pi-compat"));
    println!("  pi-extensions:{}", cfg_feature("pi-extensions"));
    println!("  computer-use: {}", cfg_feature("computer-use"));
    println!("  builtin-tools:{}", cfg_feature("builtin-tools"));
    println!("  memory:       {}", cfg_feature("memory"));
    println!("  mcp:          {}", cfg_feature("mcp"));
    println!("  sqlite-sessions: {}", cfg_feature("sqlite-sessions"));
    println!("  oauth:        {}", cfg_feature("oauth"));
    println!("modules: agent, compaction, config, context, cost, extract, guardrails, hooks, mode, permissions, plugin, prompt_cache, provider, ranking, repomap, rollout, routing, sandbox, secrets, session, slash, sse, tools, models, acp, marketplace");
}

fn cfg_feature(name: &str) -> &'static str {
    match name {
        "providers" => {
            if cfg!(feature = "providers") {
                "enabled"
            } else {
                "disabled"
            }
        }
        "ipc" => {
            if cfg!(feature = "ipc") {
                "enabled"
            } else {
                "disabled"
            }
        }
        "pi-compat" => {
            if cfg!(feature = "pi-compat") {
                "enabled"
            } else {
                "disabled"
            }
        }
        "pi-extensions" => {
            if cfg!(feature = "pi-extensions") {
                "enabled"
            } else {
                "disabled"
            }
        }
        "computer-use" => {
            if cfg!(feature = "computer-use") {
                "enabled"
            } else {
                "disabled"
            }
        }
        "builtin-tools" => {
            if cfg!(feature = "builtin-tools") {
                "enabled"
            } else {
                "disabled"
            }
        }
        "memory" => {
            if cfg!(feature = "memory") {
                "enabled"
            } else {
                "disabled"
            }
        }
        "mcp" => {
            if cfg!(feature = "mcp") {
                "enabled"
            } else {
                "disabled"
            }
        }
        "sqlite-sessions" => {
            if cfg!(feature = "sqlite-sessions") {
                "enabled"
            } else {
                "disabled"
            }
        }
        "oauth" => {
            if cfg!(feature = "oauth") {
                "enabled"
            } else {
                "disabled"
            }
        }
        _ => "unknown",
    }
}

fn run_doctor() {
    println!("rx4 {} — doctor", rx4::VERSION);
    println!();

    let openai_key = std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|k| !k.is_empty());
    let anthropic_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.is_empty());
    let xai_key = std::env::var("XAI_API_KEY").ok().filter(|k| !k.is_empty());
    print_status(
        "OPENAI_API_KEY",
        if openai_key.is_some() {
            "set"
        } else {
            "not set"
        },
        openai_key.is_some(),
    );
    print_status(
        "ANTHROPIC_API_KEY",
        if anthropic_key.is_some() {
            "set"
        } else {
            "not set"
        },
        anthropic_key.is_some(),
    );
    print_status(
        "XAI_API_KEY",
        if xai_key.is_some() { "set" } else { "not set" },
        xai_key.is_some(),
    );

    let home = home_dir();
    let data_dir = home.join(".rx4");
    let data_dir_ok = data_dir.exists();
    print_status(
        "data directory",
        &data_dir.display().to_string(),
        data_dir_ok,
    );

    let config_path = data_dir.join("config.json");
    let config_ok = config_path.exists();
    print_status("config file", &config_path.display().to_string(), config_ok);

    println!();
    println!("providers feature: {}", cfg_feature("providers"));
    println!("ipc feature:       {}", cfg_feature("ipc"));
    println!("pi-compat feature: {}", cfg_feature("pi-compat"));
}

fn print_status(label: &str, value: &str, ok: bool) {
    let mark = if ok { "[ok]" } else { "[--]" };
    println!("  {mark} {label:<20} {value}");
}

fn run_models() {
    let registry = rx4::ModelRegistry::load();
    let mut models: Vec<_> = registry.models().collect();
    models.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)));
    println!(
        "{:<32} {:<12} {:>10}  provider",
        "model", "context", "max_out"
    );
    for m in models {
        println!(
            "{:<32} {:<12} {:>10}  {}",
            m.id, m.context_window, m.max_output_tokens, m.provider
        );
    }
}

fn run_tools() {
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);

    #[cfg(feature = "computer-use")]
    {
        computer_use::register_tools(&mut tools);
    }

    let mut defs = tools.definitions();
    defs.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });
    println!("{:<16} description", "tool");
    for def in defs {
        let name = def.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let desc = def
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        println!("{name:<16} {desc}");
    }
    println!("\n{} tools registered", tools.count());
}

#[cfg(feature = "oauth")]
fn run_login(provider: &str) {
    use rs_ai_oauth::{fetch_models, start_oauth_flow, OAuthProvider};

    let oauth_provider = match OAuthProvider::parse(provider) {
        Some(p) => p,
        None => {
            eprintln!("unknown provider '{provider}'. supported: grok (xAI), openai (ChatGPT)");
            std::process::exit(1);
        }
    };

    eprintln!("starting OAuth flow for {}...", oauth_provider.name());
    eprintln!("opening browser — sign in and authorize, then return here");

    let tokens = match start_oauth_flow(oauth_provider) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("oauth failed: {e}");
            std::process::exit(1);
        }
    };

    eprintln!("\noauth successful!");
    eprintln!("access token expires at: {}", tokens.expires_at);

    let home = home_dir();
    let data_dir = home.join(".rx4");
    let _ = std::fs::create_dir_all(&data_dir);
    let token_path = data_dir.join(format!("oauth_{}.json", oauth_provider.name()));

    let token_json = serde_json::json!({
        "access_token": tokens.access_token,
        "refresh_token": tokens.refresh_token,
        "expires_at": tokens.expires_at,
        "provider": oauth_provider.name(),
    });

    if let Err(e) = std::fs::write(&token_path, serde_json::to_vec_pretty(&token_json).unwrap()) {
        eprintln!(
            "warning: could not save tokens to {}: {e}",
            token_path.display()
        );
    } else {
        eprintln!("tokens saved to {}", token_path.display());
    }

    eprintln!("\nfetching available models...");
    match fetch_models(oauth_provider, &tokens.access_token) {
        Ok(models) => {
            eprintln!("\n{} models available:", models.len());
            for m in &models {
                eprintln!("  {}", m.id);
            }
            eprintln!(
                "\nset {}=your-token to use with rx4",
                match oauth_provider {
                    OAuthProvider::Xai => "XAI_API_KEY",
                    OAuthProvider::ChatGpt => "OPENAI_API_KEY",
                }
            );
        }
        Err(e) => {
            eprintln!("could not fetch models: {e}");
        }
    }
}

#[cfg(not(feature = "oauth"))]
fn run_login(_provider: &str) {
    eprintln!("login requires the `oauth` feature (build with --features oauth)");
    std::process::exit(1);
}
