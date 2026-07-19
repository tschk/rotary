use clap::{Parser, Subcommand};
use rx4::{print_banner, register_builtin_tools, ToolRegistry};

#[cfg(feature = "providers")]
use rx4::{Agent, Scope};
#[cfg(feature = "providers")]
use std::io::BufRead;
#[cfg(feature = "providers")]
use std::sync::Arc;

#[cfg(feature = "computer-use")]
use rx4::computer_use;

#[derive(Parser)]
#[command(name = "rx4", version, about = "Agent harness engine")]
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
        /// Emit final result as a single JSON object
        #[arg(long)]
        json: bool,
        /// Stream NDJSON events to stdout (Codex-style noninteractive)
        #[arg(long)]
        stream_json: bool,
    },
    /// Start the IPC server on a Unix socket
    Serve { socket: Option<String> },
    /// Print version, feature flags, and module list
    Version,
    /// Check environment, config, and connectivity
    Doctor,
    /// List all registered models
    Models,
    /// List all built-in tools
    Tools,
    /// Install a plugin from a local path or git URL
    Install {
        /// Plugin name
        name: String,
        /// Source path or git URL
        source: String,
        /// Require a SHA-256 hash and verify the install against it
        #[arg(long, value_name = "HASH")]
        verify: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();

    let command = cli.command.unwrap_or(Commands::Chat);

    match command {
        Commands::Chat => run_chat(cli.model, cli.scope),
        Commands::Exec {
            prompt,
            json,
            stream_json,
        } => run_exec(&prompt, json, stream_json, cli.model, cli.scope),
        Commands::Serve { socket } => run_serve(socket),
        Commands::Version => run_version(),
        Commands::Doctor => run_doctor(),
        Commands::Models => run_models(),
        Commands::Tools => run_tools(),
        Commands::Install {
            name,
            source,
            verify,
        } => run_install(&name, &source, verify.as_deref()),
    }
}

#[cfg(feature = "providers")]
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
    agent.load_project_context();

    #[cfg(feature = "providers")]
    if let Some(provider) = setup_provider() {
        agent.set_provider(provider);
    }

    let workspace = agent.workspace_root.clone();
    agent.set_sandbox(Arc::new(rx4::SandboxManager::new(
        rx4::SandboxProfile::Workspace,
        workspace,
    )));
    let _ = agent.enable_os_sandbox();

    #[cfg(feature = "skills")]
    if let Some(home) = dirs::home_dir() {
        let skills_dir = home.join(".agents").join("skills");
        let mut engine = rx4::SkillEngine::new(skills_dir);
        if engine.load().is_ok() {
            let mut reg = rx4::SkillRegistry::new();
            for skill in engine.list() {
                reg.register(skill.clone());
            }
            agent.set_skill_registry(reg);
            agent.set_skill_engine(engine);
        }
    }

    #[cfg(feature = "graph-memory")]
    {
        agent.set_graph_memory(rx4::GraphMemory::new());
        agent.enable_auto_dream(true);
    }

    agent
}

#[cfg(feature = "providers")]
fn setup_provider() -> Option<Arc<dyn rx4::Provider>> {
    use rx4::provider::OpenAIProvider;

    // Providers with native Anthropic API (non-OpenAI-compatible)
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return Some(Arc::new(OpenAIProvider::anthropic(key)));
        }
    }

    // OpenAI-compatible providers: (env_var, base_url, provider_id, display_name)
    const COMPAT_PROVIDERS: &[(&str, &str, &str, &str)] = &[
        ("XAI_API_KEY", "https://api.x.ai/v1", "xai", "xAI"),
        (
            "OPENAI_API_KEY",
            "https://api.openai.com/v1",
            "openai",
            "OpenAI",
        ),
        (
            "GROQ_API_KEY",
            "https://api.groq.com/openai/v1",
            "groq",
            "Groq",
        ),
        (
            "DEEPINFRA_API_KEY",
            "https://api.deepinfra.com/v1/openai",
            "deepinfra",
            "Deep Infra",
        ),
        (
            "CEREBRAS_API_KEY",
            "https://api.cerebras.ai/v1",
            "cerebras",
            "Cerebras",
        ),
        (
            "OPENROUTER_API_KEY",
            "https://openrouter.ai/api/v1",
            "openrouter",
            "OpenRouter",
        ),
        (
            "MISTRAL_API_KEY",
            "https://api.mistral.ai/v1",
            "mistral",
            "Mistral AI",
        ),
        (
            "MOONSHOT_API_KEY",
            "https://api.moonshot.ai/v1",
            "moonshot",
            "Moonshot AI (Kimi)",
        ),
        (
            "KIMI_API_KEY",
            "https://api.moonshot.ai/v1",
            "moonshot",
            "Moonshot AI (Kimi)",
        ),
        (
            "DASHSCOPE_API_KEY",
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            "alibaba",
            "Alibaba (Qwen)",
        ),
        (
            "QWEN_API_KEY",
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            "alibaba",
            "Alibaba (Qwen)",
        ),
        (
            "DEEPSEEK_API_KEY",
            "https://api.deepseek.com",
            "deepseek",
            "DeepSeek",
        ),
        (
            "FIREWORKS_API_KEY",
            "https://api.fireworks.ai/inference/v1",
            "fireworks",
            "Fireworks AI",
        ),
        (
            "TOGETHER_API_KEY",
            "https://api.together.xyz/v1",
            "together",
            "Together AI",
        ),
        (
            "TOGETHER_AI_API_KEY",
            "https://api.together.xyz/v1",
            "together",
            "Together AI",
        ),
        (
            "PERPLEXITY_API_KEY",
            "https://api.perplexity.ai",
            "perplexity",
            "Perplexity",
        ),
        (
            "NVIDIA_API_KEY",
            "https://integrate.api.nvidia.com/v1",
            "nvidia",
            "NVIDIA",
        ),
        (
            "GITHUB_TOKEN",
            "https://models.github.ai/inference",
            "github-models",
            "GitHub Models",
        ),
        (
            "NOVITA_API_KEY",
            "https://api.novita.ai/v3/openai",
            "novita",
            "Novita AI",
        ),
        (
            "SILICONFLOW_API_KEY",
            "https://api.siliconflow.cn/v1",
            "siliconflow",
            "SiliconFlow",
        ),
        (
            "NEBIUS_API_KEY",
            "https://api.studio.nebius.ai/v1",
            "nebius",
            "Nebius",
        ),
        (
            "HF_TOKEN",
            "https://api-inference.huggingface.co/models",
            "huggingface",
            "Hugging Face",
        ),
        (
            "GOOGLE_API_KEY",
            "https://generativelanguage.googleapis.com/v1beta",
            "google",
            "Google Gemini",
        ),
        (
            "GEMINI_API_KEY",
            "https://generativelanguage.googleapis.com/v1beta",
            "google",
            "Google Gemini",
        ),
    ];

    for &(env_var, base_url, id, name) in COMPAT_PROVIDERS {
        if let Ok(key) = std::env::var(env_var) {
            if !key.is_empty() {
                return Some(Arc::new(OpenAIProvider::with_base_url(
                    base_url, key, id, name,
                )));
            }
        }
    }

    None
}

fn run_chat(model: Option<String>, scope: Option<String>) {
    print_banner();

    #[cfg(not(feature = "providers"))]
    {
        let _ = (model, scope);
        eprintln!("chat requires the `providers` feature");
        std::process::exit(1);
    }

    #[cfg(feature = "providers")]
    {
        if setup_provider().is_none() {
            eprintln!(
                "warning: no API key found (run 'rx4 doctor' to see all supported providers)"
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

#[cfg(feature = "providers")]
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
            eprintln!("  /save           save current session to ~/.agents/sessions/");
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

#[cfg(feature = "providers")]
fn sessions_dir() -> std::path::PathBuf {
    home_dir().join(".agents").join("sessions")
}

fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn run_exec(
    prompt: &str,
    json: bool,
    stream_json: bool,
    model: Option<String>,
    scope: Option<String>,
) {
    #[cfg(not(feature = "providers"))]
    {
        let _ = (prompt, json, stream_json, model, scope);
        eprintln!("exec requires the `providers` feature");
        std::process::exit(1);
    }

    #[cfg(feature = "providers")]
    {
        if setup_provider().is_none() {
            eprintln!("error: no API key found (run 'rx4 doctor' to see all supported providers)");
            std::process::exit(1);
        }
        let mut agent = build_agent(model.as_deref(), scope.as_deref());
        // Noninteractive CI: auto-allow tools unless host sets a stricter approver.
        agent.set_approver(Arc::new(rx4::permissions::AlwaysAllow));
        let output = Arc::new(parking_lot::Mutex::new(String::new()));
        let output_clone = output.clone();
        let stream = stream_json;
        agent.subscribe(move |event| {
            if stream {
                if let Ok(line) = serde_json::to_string(event) {
                    use std::io::Write;
                    let mut out = std::io::stdout().lock();
                    let _ = writeln!(out, "{line}");
                    let _ = out.flush();
                }
            }
            match event {
                rx4::Event::MessageDelta { delta } => {
                    output_clone.lock().push_str(delta);
                    if !json && !stream {
                        use std::io::Write;
                        let mut out = std::io::stdout().lock();
                        let _ = out.write_all(delta.as_bytes());
                        let _ = out.flush();
                    }
                }
                rx4::Event::Error(err) if !stream => {
                    eprintln!("\n[error: {err}]");
                }
                _ => {}
            }
        });
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("failed to start runtime: {e}");
                std::process::exit(1);
            }
        };
        if let Err(e) = rt.block_on(agent.prompt(prompt)) {
            if json || stream_json {
                println!(
                    "{}",
                    serde_json::json!({"type":"error","error": e.to_string()})
                );
            } else {
                eprintln!("agent error: {e}");
            }
            std::process::exit(1);
        }
        if json && !stream_json {
            let result = output.lock().clone();
            let json_out = serde_json::json!({
                "model": agent.model,
                "scope": agent.scope.to_string(),
                "output": result,
            });
            println!("{json_out}");
        } else if !json && !stream_json {
            println!();
        }
    }
}

#[cfg(feature = "ipc")]
fn run_serve(socket: Option<String>) {
    use rx4::ipc::IpcServer;
    let socket_path = socket.unwrap_or_else(|| {
        home_dir()
            .join(".agents")
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

fn run_version() {
    println!("rx4 {}", rx4::VERSION);
    println!("features:");
    println!("  providers:    {}", cfg_feature("providers"));
    println!("  ipc:          {}", cfg_feature("ipc"));
    println!("  computer-use: {}", cfg_feature("computer-use"));
    println!("  builtin-tools:{}", cfg_feature("builtin-tools"));
    println!("  memory:       {}", cfg_feature("memory"));
    println!("  mcp:          {}", cfg_feature("mcp"));
    println!("  sqlite-sessions: {}", cfg_feature("sqlite-sessions"));
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
        _ => "unknown",
    }
}

fn run_doctor() {
    println!("rx4 {} — doctor", rx4::VERSION);
    println!();

    println!("api keys:");
    const CHECK_KEYS: &[&str] = &[
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "XAI_API_KEY",
        "GROQ_API_KEY",
        "DEEPINFRA_API_KEY",
        "CEREBRAS_API_KEY",
        "OPENROUTER_API_KEY",
        "MISTRAL_API_KEY",
        "MOONSHOT_API_KEY",
        "KIMI_API_KEY",
        "DASHSCOPE_API_KEY",
        "QWEN_API_KEY",
        "DEEPSEEK_API_KEY",
        "FIREWORKS_API_KEY",
        "TOGETHER_API_KEY",
        "PERPLEXITY_API_KEY",
        "NVIDIA_API_KEY",
        "GITHUB_TOKEN",
        "NOVITA_API_KEY",
        "SILICONFLOW_API_KEY",
        "NEBIUS_API_KEY",
        "HF_TOKEN",
        "GOOGLE_API_KEY",
        "GEMINI_API_KEY",
    ];
    let mut any_key = false;
    for key in CHECK_KEYS {
        let set = std::env::var(key).map(|v| !v.is_empty()).unwrap_or(false);
        if set {
            any_key = true;
        }
        print_status(key, if set { "set" } else { "not set" }, set);
    }
    if !any_key {
        println!("\n  hint: set any one of the above env vars to use rx4");
    }

    let home = home_dir();
    let data_dir = home.join(".agents");
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

fn run_install(name: &str, source: &str, verify: Option<&str>) {
    use rx4::marketplace::{PluginInstaller, PluginManifest};

    let installer = PluginInstaller::default();
    let manifest = PluginManifest {
        name: name.to_string(),
        version: "0.0.0".to_string(),
        description: String::new(),
        author: String::new(),
        homepage: None,
        repository: None,
        license: None,
        categories: Vec::new(),
        keywords: Vec::new(),
        dependencies: Vec::new(),
        mcp_servers: Vec::new(),
        skills: Vec::new(),
        hooks: Vec::new(),
        sha256: verify.map(|s| s.to_string()),
    };

    match installer.install(&manifest, source) {
        Ok(installed) => {
            println!(
                "installed plugin {} v{} at {}",
                installed.name,
                installed.version,
                installed.path.display()
            );
        }
        Err(e) => {
            eprintln!("install failed: {e}");
            std::process::exit(1);
        }
    }
}
