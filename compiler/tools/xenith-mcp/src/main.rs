//! Stdio transport: newline-delimited JSON-RPC, requests in, responses out.
//!
//! Nothing but protocol goes to stdout. Anything worth saying to a human —
//! which should be rare — goes to stderr.

use std::io::{BufRead, Write};

use serde_json::{Value, json};
use xenith_mcp::server::handle_message;

fn main() {
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
            Ok(message) => handle_message(&message),
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
}
