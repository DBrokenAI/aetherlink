mod cli_add;
mod graph;
mod mcp;
mod notify;
mod register;
mod rules;
mod scanner;
mod security;
mod status;
mod tray;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

use mcp::McpServer;

#[tokio::main]
async fn main() -> Result<()> {
    // CLI dispatch: handle --register / --help before any MCP setup.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("--register") => return register::run(),
        Some("--add") => return cli_add::run(),
        Some("--tray") => return tray::run(),
        Some("--version") | Some("-V") => {
            println!("aetherlink {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help") | Some("-h") => {
            print_help();
            return Ok(());
        }
        Some(other) => {
            eprintln!("aetherlink: unknown argument '{other}'");
            print_help();
            std::process::exit(2);
        }
        None => {} // fall through to MCP server mode
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let server = McpServer::new();
    server.run().await
}

fn print_help() {
    println!("AetherLink {} — architectural guardrail MCP server", env!("CARGO_PKG_VERSION"));
    println!();
    println!("USAGE:");
    println!("    aetherlink              Run as an MCP server on stdio (default)");
    println!("    aetherlink --register   Install into Claude Desktop's config and create");
    println!("                            a starter AetherLink.toml in the current folder");
    println!("    aetherlink --add        Interactively add a rule to AetherLink.toml in");
    println!("                            the current folder, then re-scan");
    println!("    aetherlink --tray       Run the system tray supervisor — shows a green");
    println!("                            or red icon reflecting the latest scan state");
    println!("    aetherlink --version    Print the binary version and exit");
    println!("    aetherlink --help       Show this help");
}
