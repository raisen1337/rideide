//! `ride-agent mcp`: a small MCP server, over stdin and stdout, that lets Claude Code on
//! the host show the user things in ride: it forwards a port to their computer and opens
//! their browser on it.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn tools() -> Value {
    let port = json!({
        "type": "integer",
        "minimum": 1,
        "maximum": 65535,
        "description": "The port on this host that the server listens on."
    });
    json!([
        {
            "name": "preview",
            "description": "Shows the user a web page served on this host: forwards the port to \
                the user's computer through ride and opens it in their browser. Use it whenever \
                the user asks for a demo or a preview of something that runs on a port. Start the \
                server first, listening on 127.0.0.1.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": port,
                    "path": {
                        "type": "string",
                        "description": "The path to open, starting with a slash. Defaults to /."
                    }
                },
                "required": ["port"]
            }
        },
        {
            "name": "forward_port",
            "description": "Forwards a port of this host to the user's computer through ride, \
                without opening a browser. Returns the address it is reachable at there.",
            "inputSchema": {
                "type": "object",
                "properties": { "port": port },
                "required": ["port"]
            }
        }
    ])
}

fn call(params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let arguments = &params["arguments"];
    let answer = match arguments["port"].as_u64() {
        _ if name != "preview" && name != "forward_port" => Err(format!("no tool named {name}")),
        Some(port @ 1..=65535) => {
            let path = arguments["path"].as_str().unwrap_or("/");
            let line = json!({ "tool": name, "port": port, "path": path }).to_string();
            ask(&line).ok_or_else(|| {
                format!(
                    "ride isn't connected to this host right now, so nothing was forwarded. \
                     Tell the user the server is on port {port} here; they can open ride and \
                     ask again, or forward the port themselves."
                )
            })
        }
        _ => Err("`port` must be a number from 1 to 65535".to_string()),
    };
    // The app starts its answer with "error:" when it couldn't do it.
    let (text, failed) = match answer {
        Ok(text) => match text.strip_prefix("error:") {
            Some(why) => (why.trim().to_string(), true),
            None => (text, false),
        },
        Err(text) => (text, true),
    };
    json!({ "content": [{ "type": "text", "text": text }], "isError": failed })
}

#[cfg(unix)]
fn ask(line: &str) -> Option<String> {
    crate::host::ask(line)
}

#[cfg(not(unix))]
fn ask(_line: &str) -> Option<String> {
    None
}

pub fn run() {
    let mut out = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let result = match msg["method"].as_str().unwrap_or("") {
            "initialize" => Some(json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "ride", "version": env!("CARGO_PKG_VERSION") },
            })),
            "ping" => Some(json!({})),
            "tools/list" => Some(json!({ "tools": tools() })),
            "tools/call" => Some(call(&msg["params"])),
            _ => None,
        };
        // A message without an id is a notification, which gets no reply.
        let Some(id) = msg.get("id").filter(|id| !id.is_null()) else {
            continue;
        };
        let reply = match result {
            Some(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            None => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "method not found" },
            }),
        };
        if writeln!(out, "{reply}").and_then(|_| out.flush()).is_err() {
            break;
        }
    }
}
