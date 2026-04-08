mod cli_add;
mod graph;
mod hook;
mod install_hook;
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
        Some("--hook-check") => return hook::run(),
        Some("--install-hook") => return install_hook::install(),
        Some("--uninstall-hook") => return install_hook::uninstall(),
        Some("--baseline") => {
            // Optional second arg = project path; default to CWD.
            let path = args
                .get(1)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().expect("cwd"));
            return run_baseline(&path);
        }
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

/// Snapshot the project's current violations into `.aetherlink-baseline.json`.
/// This is the "freeze the rot" command — once the baseline is committed,
/// existing violations stop blocking writes, but any *new* violation still
/// does. Re-run this command after fixing real violations to shrink the
/// baseline. Re-run after tightening rules to re-snapshot at the new bar.
fn run_baseline(project_root: &std::path::Path) -> Result<()> {
    use crate::graph::DependencyGraph;
    use crate::rules::{validate_with_overrides, Baseline, RulesFile};
    use crate::scanner::FileScanner;

    eprintln!("AetherLink --baseline");
    eprintln!("=====================");
    eprintln!("Project: {}", project_root.display());

    let files = FileScanner::new(project_root).scan()?;
    let mut graph = DependencyGraph::new(project_root);
    graph.build(&files);
    let rules_file = RulesFile::load(project_root)?;
    let violations =
        validate_with_overrides(&files, &graph, &rules_file, Some(project_root));

    let baseline = Baseline::from_violations(&violations);
    let path = baseline.save(project_root)?;

    eprintln!("Captured {} violation(s) into baseline.", violations.len());
    eprintln!("Baseline file: {}", path.display());
    eprintln!();
    eprintln!("These violations are now grandfathered. AetherLink will:");
    eprintln!("  * surface them as warnings in scan reports,");
    eprintln!("  * NOT block writes because of them, even on the offending files,");
    eprintln!("  * still block ANY new violation introduced after this point.");
    eprintln!();
    eprintln!("Commit {} to your repo so every machine sees the same baseline.", Baseline::FILE_NAME);
    eprintln!("Re-run `aetherlink --baseline` after fixing violations to shrink it.");
    Ok(())
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
    println!("    aetherlink --hook-check  Read a Claude Code PreToolUse hook payload");
    println!("                            from stdin and exit 0 (allow) or 2 (block).");
    println!("                            Installed automatically by install.bat into");
    println!("                            ~/.claude/settings.json so Edit/Write/MultiEdit");
    println!("                            calls cannot bypass AetherLink rules.");
    println!("    aetherlink --baseline [PATH]");
    println!("                            Snapshot current violations into");
    println!("                            .aetherlink-baseline.json so existing rot stops");
    println!("                            blocking writes. Commit the file. Re-run after");
    println!("                            fixing violations to shrink the baseline.");
    println!("    aetherlink --install-hook");
    println!("                            Patch ~/.claude/settings.json to register the");
    println!("                            PreToolUse hook on every OS. Idempotent. Use this");
    println!("                            on macOS / Linux instead of install.bat.");
    println!("    aetherlink --uninstall-hook");
    println!("                            Remove the AetherLink hook entry from settings.json,");
    println!("                            leaving any other hooks in place.");
    println!("    aetherlink --version    Print the binary version and exit");
    println!("    aetherlink --help       Show this help");
}
