use rx4::{print_banner, register_builtin_tools, Agent, Scope, ToolRegistry};

#[cfg(feature = "computer-use")]
use rx4::computer_use;

#[cfg(feature = "pi-compat")]
use rx4::pi::PiRpcServer;

fn main() {
    print_banner();

    let args: Vec<String> = std::env::args().collect();

    // pi-compatible RPC mode: `rx4 --mode rpc`
    #[cfg(feature = "pi-compat")]
    if args.iter().any(|a| a == "--mode" || a == "rpc") {
        if args.iter().any(|a| a == "rpc") {
            run_rpc_mode();
            return;
        }
    }

    // Default: run a single demo prompt
    let mut agent = Agent::new();
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);

    #[cfg(feature = "computer-use")]
    {
        computer_use::register_tools(&mut tools);
    }

    agent.set_tools(tools);
    agent.set_scope(Scope::Coding);

    agent.subscribe(|event| {
        tracing::info!("event: {:?}", event);
    });

    let rt = tokio::runtime::Runtime::new().unwrap();
    let _ = rt.block_on(agent.prompt("Hello, rx4!"));
    eprintln!(
        "rx4 demo complete — tools={} scope={}",
        agent.tools.count(),
        agent.scope
    );
}

#[cfg(feature = "pi-compat")]
fn run_rpc_mode() {
    use rx4::pi::tools::pi_to_rx4_tool;
    let _ = pi_to_rx4_tool; // ensure mapping is initialized

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
