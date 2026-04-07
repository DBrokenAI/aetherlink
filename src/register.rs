//! `aetherlink --register` — one-shot installer.
//!
//! Locates the Claude Desktop config for the current platform, injects an
//! `aetherlink` entry into its `mcpServers` map, and writes a starter
//! `AetherLink.toml` into the current directory if none exists.

use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

const STARTER_TOML: &str = r#"# AetherLink architectural rules.
# Edit these to define the laws AetherLink will enforce on your project.

[rules]
# Reject any source file longer than this many lines.
max_file_lines = 500

# Block circular dependencies in the import graph.
no_cycles = true

# Block specific cross-folder imports. Format: "from -> to".
# Each side matches any directory component of a file's path.
# Example: forbidden_imports = ["ui -> db", "api -> secret"]
forbidden_imports = []
"#;

/// Run the registration flow. Prints progress to stderr (so it doesn't pollute
/// stdout if the user later runs the binary as an MCP server).
pub fn run() -> Result<()> {
    eprintln!("AetherLink --register");
    eprintln!("=====================");

    let exe = env::current_exe().context("locating current executable")?;
    eprintln!("Binary path:        {}", exe.display());

    let config_path = locate_claude_config()
        .context("could not determine Claude Desktop config path for this platform")?;
    eprintln!("Claude config:      {}", config_path.display());

    register_mcp_server(&config_path, &exe)?;
    eprintln!("Registered with Claude Desktop as MCP server 'aetherlink'.");

    let toml_path = ensure_starter_toml()?;
    match &toml_path {
        StarterTomlOutcome::Created(p) => eprintln!("Created starter:    {}", p.display()),
        StarterTomlOutcome::AlreadyExists(p) => {
            eprintln!("Existing config:    {} (left untouched)", p.display())
        }
    }

    print_next_steps(&toml_path);
    Ok(())
}

/// Print a friendly onboarding block at the end of `--register`. The user
/// just installed an MCP server they've never used before — tell them what
/// to do next, in order, in plain English.
fn print_next_steps(toml_path: &StarterTomlOutcome) {
    let toml_display = match toml_path {
        StarterTomlOutcome::Created(p) | StarterTomlOutcome::AlreadyExists(p) => p.display().to_string(),
    };

    eprintln!();
    eprintln!("================================================================");
    eprintln!("  AetherLink is installed. Here's what to do next:");
    eprintln!("================================================================");
    eprintln!();
    eprintln!("  1. RESTART CLAUDE DESKTOP completely.");
    eprintln!("     Right-click the system tray icon -> Quit (don't just close");
    eprintln!("     the window). Then reopen it. Claude only loads new MCP");
    eprintln!("     servers on startup.");
    eprintln!();
    eprintln!("  2. VERIFY it's working.");
    eprintln!("     In a new Claude chat, ask: 'What AetherLink tools do you");
    eprintln!("     have?' You should see scan_project, apply_guarded_change,");
    eprintln!("     acquire_lease, add_rule, and a few others.");
    eprintln!();
    eprintln!("  3. SET UP RULES for the projects you care about.");
    eprintln!("     A starter rules file was placed at:");
    eprintln!("       {toml_display}");
    eprintln!("     Edit it directly, or run `aetherlink --add` from inside");
    eprintln!("     any project to walk through an interactive menu.");
    eprintln!();
    eprintln!("  4. (OPTIONAL) Run `aetherlink --tray` once at login for a");
    eprintln!("     small green/red icon next to your clock that reflects");
    eprintln!("     the latest project health.");
    eprintln!();
    eprintln!("  IF SOMETHING BREAKS: create an empty file named");
    eprintln!("  `.aetherlink_bypass` in your project root. AetherLink will");
    eprintln!("  let writes through (with a CRITICAL warning) until you");
    eprintln!("  delete the file.");
    eprintln!();
    eprintln!("  Run `aetherlink --help` to see every command.");
    eprintln!("================================================================");
}

/// Find the Claude Desktop config path for the current OS, creating parent
/// directories so the write below will succeed.
fn locate_claude_config() -> Result<PathBuf> {
    let dir = if cfg!(target_os = "windows") {
        // %APPDATA%\Claude
        let appdata = env::var("APPDATA")
            .map_err(|_| anyhow!("APPDATA environment variable is not set"))?;
        PathBuf::from(appdata).join("Claude")
    } else if cfg!(target_os = "macos") {
        // ~/Library/Application Support/Claude
        let home = env::var("HOME").map_err(|_| anyhow!("HOME environment variable is not set"))?;
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("Claude")
    } else if cfg!(target_os = "linux") {
        // Not officially supported by Claude Desktop, but follow the XDG convention
        // so future versions or community ports work without extra effort.
        let home = env::var("HOME").map_err(|_| anyhow!("HOME environment variable is not set"))?;
        PathBuf::from(home).join(".config").join("Claude")
    } else {
        return Err(anyhow!("unsupported operating system"));
    };

    fs::create_dir_all(&dir)
        .with_context(|| format!("creating Claude config directory {}", dir.display()))?;

    Ok(dir.join("claude_desktop_config.json"))
}

/// Read (or create) the Claude config file and inject our MCP server entry.
fn register_mcp_server(config_path: &PathBuf, exe: &PathBuf) -> Result<()> {
    // Load existing JSON if present, otherwise start from an empty object.
    let mut config: Value = if config_path.exists() {
        let text = fs::read_to_string(config_path)
            .with_context(|| format!("reading {}", config_path.display()))?;
        // Strip a UTF-8 BOM if present. PowerShell's `Set-Content -Encoding UTF8`,
        // Windows Notepad's "Save As UTF-8", and various other Windows tools
        // write a leading EF BB BF that serde_json refuses to parse. Tolerating
        // it here means real users editing the file by hand can't accidentally
        // brick their install.
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
        if text.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(text)
                .with_context(|| format!("parsing existing {}", config_path.display()))?
        }
    } else {
        json!({})
    };

    // Top-level object must exist.
    let root = config
        .as_object_mut()
        .ok_or_else(|| anyhow!("Claude config root is not a JSON object"))?;

    // Ensure `mcpServers` is an object.
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("'mcpServers' exists but is not a JSON object"))?;

    // Overwrite (or insert) our entry. Always overwrite so re-running --register
    // after moving the binary updates the path correctly.
    servers.insert(
        "aetherlink".to_string(),
        json!({
            "command": exe.to_string_lossy(),
            "args": []
        }),
    );

    let pretty = serde_json::to_string_pretty(&config)?;
    fs::write(config_path, pretty)
        .with_context(|| format!("writing {}", config_path.display()))?;
    Ok(())
}

enum StarterTomlOutcome {
    Created(PathBuf),
    AlreadyExists(PathBuf),
}

/// Drop a starter `AetherLink.toml` into the current working directory if one
/// is not already there. Never overwrites — users may have customized it.
fn ensure_starter_toml() -> Result<StarterTomlOutcome> {
    let cwd = env::current_dir().context("reading current directory")?;
    let path = cwd.join("AetherLink.toml");
    if path.exists() {
        return Ok(StarterTomlOutcome::AlreadyExists(path));
    }
    fs::write(&path, STARTER_TOML)
        .with_context(|| format!("writing starter config to {}", path.display()))?;
    Ok(StarterTomlOutcome::Created(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn injects_into_empty_config() {
        let dir = tempdir();
        let cfg = dir.join("claude_desktop_config.json");
        let exe = PathBuf::from("/fake/aetherlink");
        register_mcp_server(&cfg, &exe).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["aetherlink"]["command"], "/fake/aetherlink");
        assert_eq!(v["mcpServers"]["aetherlink"]["args"], json!([]));
    }

    #[test]
    fn preserves_other_servers() {
        let dir = tempdir();
        let cfg = dir.join("claude_desktop_config.json");
        let existing = json!({
            "mcpServers": {
                "other": { "command": "/bin/other", "args": ["--foo"] }
            },
            "someOtherTopLevelKey": 42
        });
        fs::File::create(&cfg)
            .unwrap()
            .write_all(serde_json::to_string_pretty(&existing).unwrap().as_bytes())
            .unwrap();

        register_mcp_server(&cfg, &PathBuf::from("/fake/aetherlink")).unwrap();

        let v: Value = serde_json::from_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["other"]["command"], "/bin/other");
        assert_eq!(v["mcpServers"]["aetherlink"]["command"], "/fake/aetherlink");
        assert_eq!(v["someOtherTopLevelKey"], 42);
    }

    #[test]
    fn tolerates_utf8_bom_in_existing_config() {
        // PowerShell, Notepad's "Save As UTF-8", and other Windows tools
        // commonly write a UTF-8 BOM (EF BB BF) at the start of files. The
        // installer must survive that — otherwise touching the config in
        // Notepad once would brick every future re-register call.
        let dir = tempdir();
        let cfg = dir.join("claude_desktop_config.json");
        let json_with_bom = {
            let mut bytes = vec![0xEF, 0xBB, 0xBF];
            bytes.extend_from_slice(br#"{"mcpServers":{"other":{"command":"/bin/other","args":[]}}}"#);
            bytes
        };
        fs::write(&cfg, &json_with_bom).unwrap();

        register_mcp_server(&cfg, &PathBuf::from("/fake/aetherlink")).unwrap();

        let v: Value = serde_json::from_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["other"]["command"], "/bin/other");
        assert_eq!(v["mcpServers"]["aetherlink"]["command"], "/fake/aetherlink");
    }

    #[test]
    fn overwrites_stale_aetherlink_entry() {
        let dir = tempdir();
        let cfg = dir.join("claude_desktop_config.json");
        let existing = json!({
            "mcpServers": {
                "aetherlink": { "command": "/old/path", "args": [] }
            }
        });
        fs::write(&cfg, serde_json::to_string(&existing).unwrap()).unwrap();

        register_mcp_server(&cfg, &PathBuf::from("/new/path")).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["aetherlink"]["command"], "/new/path");
    }

    /// Tiny ad-hoc temp dir helper to avoid pulling in `tempfile` as a dep.
    fn tempdir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = env::temp_dir().join(format!("aetherlink-test-{nanos}"));
        fs::create_dir_all(&p).unwrap();
        p
    }
}
