//! Stdio transport: newline-delimited JSON-RPC, requests in, responses out.
//!
//! Nothing but protocol goes to stdout. Anything worth saying to a human —
//! which should be rare — goes to stderr.
//!
//! Two flags: `--workspace-root <dir>` names the directory the file-taking
//! tools are confined to — it defaults to the working directory at startup,
//! which is what an MCP client configured with a project directory gives us —
//! and `--experimental-api-surface` exposes the unstable `api_surface` tool,
//! which stays off the default list deliberately (design/0013 §2).

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::{Value, json};
use xenith_mcp::server::{ServerOptions, handle_message_with};

fn main() -> ExitCode {
    let (workspace_root, options) = match configuration_from_args() {
        Ok(configuration) => configuration,
        Err(message) => {
            eprintln!("xenith-mcp: {message}");
            return ExitCode::FAILURE;
        }
    };

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle_message_with(&message, &workspace_root, &options),
            Err(_) => Some(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": { "code": -32700, "message": "parse error" },
            })),
        };

        if let Some(reply) = reply {
            if writeln!(out, "{reply}").and_then(|_| out.flush()).is_err() {
                break;
            }
        }
    }
    ExitCode::SUCCESS
}

/// Parse `--workspace-root <dir>` (or `--workspace-root=<dir>`) and
/// `--experimental-api-surface`, validating the root early: a root that does
/// not resolve to a directory would otherwise surface as a baffling per-call
/// error. The path handed to the server stays as given — containment
/// canonicalizes both sides on every call.
fn configuration_from_args() -> Result<(PathBuf, ServerOptions), String> {
    let mut root: Option<PathBuf> = None;
    let mut options = ServerOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--experimental-api-surface" {
            options.experimental_api_surface = true;
            continue;
        }
        let value = if argument == "--workspace-root" {
            args.next()
                .ok_or_else(|| "--workspace-root takes a directory".to_string())?
        } else if let Some(value) = argument.strip_prefix("--workspace-root=") {
            value.to_string()
        } else {
            return Err(format!(
                "unexpected argument `{argument}`; usage: \
                 xenith-mcp [--workspace-root <dir>] [--experimental-api-surface]"
            ));
        };
        root = Some(PathBuf::from(value));
    }

    let root = match root {
        Some(root) => root,
        None => std::env::current_dir()
            .map_err(|e| format!("cannot determine the working directory: {e}"))?,
    };
    let canonical = root
        .canonicalize()
        .map_err(|e| format!("workspace root {}: {e}", root.display()))?;
    if !canonical.is_dir() {
        return Err(format!(
            "workspace root {} is not a directory",
            root.display()
        ));
    }
    Ok((root, options))
}
