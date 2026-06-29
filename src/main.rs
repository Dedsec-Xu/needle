//! needle: whole-machine, sub-millisecond file search for AI agents, exposed as
//! a CLI and an MCP server (`fast_glob` tool) so an agent can find any file on
//! the machine instantly instead of walking the filesystem.
//!
//! Because reading the MFT requires admin but Claude Code launches MCP servers
//! non-elevated, the tool splits into two roles:
//!   * `serve` — elevated daemon: holds the index, applies USN updates, answers
//!     queries over loopback TCP.
//!   * `mcp`   — non-elevated frontend launched by Claude Code: forwards the
//!     `fast_glob` tool to the daemon.
//! `query` runs a one-shot in-process search (must itself be elevated).

mod engine;
mod ipc;
mod mft;
mod service;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use engine::{Engine, Query};
use ipc::{WireHit, WireQuery, WireResponse, DEFAULT_ADDR};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "needle", about = "Whole-machine sub-millisecond file search for AI agents (NTFS MFT + USN)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a one-shot glob query in-process (requires admin). For ad-hoc use.
    Query {
        /// Glob pattern, e.g. "**/*.swift".
        pattern: String,
        #[arg(long, default_value = "")]
        root: String,
        #[arg(long, default_value_t = 200)]
        max_results: usize,
        #[arg(long, default_value_t = false)]
        respect_gitignore: bool,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Query the running daemon and print matching paths (fast; no admin needed).
    Find {
        /// Glob pattern, e.g. "**/*.rs".
        pattern: String,
        #[arg(long, default_value = "")]
        root: String,
        #[arg(long, default_value_t = 200)]
        max_results: usize,
        #[arg(long, default_value_t = false)]
        respect_gitignore: bool,
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr: String,
    },
    /// Build the index for a drive and report stats (warms cache / benchmark).
    Index {
        #[arg(default_value = "C")]
        drive: char,
    },
    /// Benchmark: time the index build and sample queries; print a Markdown
    /// table ready to paste into the README (requires admin).
    Bench {
        #[arg(default_value = "C")]
        drive: char,
    },
    /// Run the elevated daemon: index + USN refresh + loopback query server.
    Serve {
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr: String,
    },
    /// Run as an MCP server over stdio; forwards queries to the daemon.
    Mcp {
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr: String,
    },
    /// Manage the Windows service (install once; auto-starts as LocalSystem).
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Register and start the service (run elevated).
    Install,
    /// Stop and remove the service (run elevated).
    Uninstall,
    /// Internal: entry point invoked by the Service Control Manager.
    Run,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Query {
            pattern,
            root,
            max_results,
            respect_gitignore,
            json,
        } => run_query(&pattern, &root, max_results, respect_gitignore, json),
        Cmd::Find {
            pattern,
            root,
            max_results,
            respect_gitignore,
            addr,
        } => run_find(&addr, &pattern, &root, max_results, respect_gitignore),
        Cmd::Index { drive } => run_index(drive),
        Cmd::Bench { drive } => run_bench(drive),
        Cmd::Serve { addr } => run_serve(&addr),
        Cmd::Mcp { addr } => run_mcp(&addr),
        Cmd::Service { action } => match action {
            ServiceAction::Install => {
                service::install()?;
                eprintln!(
                    "[needle] service '{}' installed and started (LocalSystem, auto-start).",
                    service::SERVICE_NAME
                );
                Ok(())
            }
            ServiceAction::Uninstall => {
                service::uninstall()?;
                eprintln!("[needle] service '{}' removed.", service::SERVICE_NAME);
                Ok(())
            }
            ServiceAction::Run => service::run_dispatch(),
        },
    }
}

fn run_query(
    pattern: &str,
    root: &str,
    max_results: usize,
    respect_gitignore: bool,
    json: bool,
) -> Result<()> {
    let engine = Engine::new();
    let (hits, stats) = engine.query(&Query {
        root,
        pattern,
        max_results,
        respect_gitignore,
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&hits)?);
    } else {
        for h in &hits {
            println!("{}", h.path);
        }
    }
    eprintln!(
        "[needle] drive {} scanned {} returned {}{} in {:.1}ms",
        stats.drive,
        stats.scanned,
        stats.returned,
        if stats.truncated { " (truncated)" } else { "" },
        stats.elapsed_ms,
    );
    Ok(())
}

fn run_find(
    addr: &str,
    pattern: &str,
    root: &str,
    max_results: usize,
    respect_gitignore: bool,
) -> Result<()> {
    let q = WireQuery {
        root: root.to_string(),
        pattern: pattern.to_string(),
        max_results,
        respect_gitignore,
    };
    let resp = daemon_query(addr, &q)?;
    if !resp.ok {
        return Err(anyhow!(resp.error.unwrap_or_else(|| "daemon error".into())));
    }
    for h in &resp.hits {
        println!("{}", h.path);
    }
    eprintln!(
        "[needle] {} match(es){} in {:.2}ms (scanned {} on drive {})",
        resp.returned,
        if resp.truncated { " (truncated)" } else { "" },
        resp.elapsed_ms,
        resp.scanned,
        resp.drive,
    );
    Ok(())
}

fn run_index(drive: char) -> Result<()> {
    let start = std::time::Instant::now();
    let idx = mft::build_index(drive)?;
    eprintln!(
        "[needle] indexed {} entries on drive {} in {:.2}s",
        idx.entries.len(),
        drive,
        start.elapsed().as_secs_f64(),
    );
    Ok(())
}

fn run_bench(drive: char) -> Result<()> {
    let engine = Engine::new();

    // 1) Time the full index build for the drive.
    let t0 = std::time::Instant::now();
    engine.warm(drive)?;
    let build_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let entries = engine.entry_count();

    // 2) Time a set of representative whole-volume queries against the warm index.
    let samples = [
        "**/*.rs",
        "**/Cargo.toml",
        "**/*.dll",
        "**/package.json",
        "**/*.exe",
    ];
    let root = format!("{}:\\", drive.to_ascii_uppercase());

    println!("## Benchmark (drive {})\n", drive.to_ascii_uppercase());
    println!(
        "Indexed **{}** entries in **{:.0} ms** ({:.2} s).\n",
        entries,
        build_ms,
        build_ms / 1000.0
    );
    println!("| query (whole volume) | matches | time |");
    println!("|----------------------|---------|------|");
    for pat in samples {
        let (hits, stats) = engine.query(&Query {
            root: &root,
            pattern: pat,
            max_results: 1_000_000,
            respect_gitignore: false,
        })?;
        println!(
            "| `{}` | {} | {:.2} ms |",
            pat,
            hits.len(),
            stats.elapsed_ms
        );
    }
    println!(
        "\n_Index build is a one-time cost; queries run against the warm in-memory \
         index and are kept fresh via the USN Journal._"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Daemon (elevated)
// ---------------------------------------------------------------------------

fn run_serve(addr: &str) -> Result<()> {
    let engine = Arc::new(Engine::new());

    // Background USN refresh.
    {
        let engine = Arc::clone(&engine);
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(2));
            engine.refresh_all();
        });
    }

    let listener = TcpListener::bind(addr)
        .map_err(|e| anyhow!("failed to bind {addr}: {e}"))?;
    eprintln!("[needled] listening on {addr}");

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let engine = Arc::clone(&engine);
        thread::spawn(move || {
            if let Err(e) = serve_conn(stream, &engine) {
                eprintln!("[needled] connection error: {e}");
            }
        });
    }
    Ok(())
}

fn serve_conn(stream: TcpStream, engine: &Engine) -> Result<()> {
    let peer = stream.try_clone()?;
    let reader = BufReader::new(stream);
    let mut writer = peer;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let q: WireQuery = match serde_json::from_str(&line) {
            Ok(q) => q,
            Err(e) => {
                let resp = err_response(&format!("bad request: {e}"));
                writeln!(writer, "{}", serde_json::to_string(&resp)?)?;
                continue;
            }
        };
        let resp = match engine.query(&Query {
            root: &q.root,
            pattern: &q.pattern,
            max_results: q.max_results,
            respect_gitignore: q.respect_gitignore,
        }) {
            Ok((hits, stats)) => WireResponse {
                ok: true,
                error: None,
                hits: hits
                    .into_iter()
                    .map(|h| WireHit {
                        path: h.path,
                        is_dir: h.is_dir,
                    })
                    .collect(),
                drive: stats.drive.to_string(),
                scanned: stats.scanned,
                returned: stats.returned,
                truncated: stats.truncated,
                elapsed_ms: stats.elapsed_ms,
            },
            Err(e) => err_response(&e.to_string()),
        };
        writeln!(writer, "{}", serde_json::to_string(&resp)?)?;
        writer.flush()?;
    }
    Ok(())
}

fn err_response(msg: &str) -> WireResponse {
    WireResponse {
        ok: false,
        error: Some(msg.to_string()),
        hits: vec![],
        drive: String::new(),
        scanned: 0,
        returned: 0,
        truncated: false,
        elapsed_ms: 0.0,
    }
}

/// Send one query to the daemon and read its response.
fn daemon_query(addr: &str, q: &WireQuery) -> Result<WireResponse> {
    let stream = TcpStream::connect(addr)
        .map_err(|e| anyhow!("cannot reach daemon at {addr}: {e}. Start it with: needle serve (as admin)"))?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut writer = stream.try_clone()?;
    writeln!(writer, "{}", serde_json::to_string(q)?)?;
    writer.flush()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let resp: WireResponse = serde_json::from_str(line.trim())?;
    Ok(resp)
}

// ---------------------------------------------------------------------------
// MCP server (non-elevated frontend)
// ---------------------------------------------------------------------------

const PROTOCOL_VERSION: &str = "2024-11-05";

fn run_mcp(addr: &str) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[needle-mcp] parse error: {e}");
                continue;
            }
        };

        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

        let response = match method {
            "initialize" => Some(reply(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "serverInfo": { "name": "needle", "version": env!("CARGO_PKG_VERSION") },
                    "capabilities": { "tools": {} }
                }),
            )),
            "tools/list" => Some(reply(id, tools_list())),
            "tools/call" => Some(handle_tool_call(id, &req, addr)),
            "ping" => Some(reply(id, json!({}))),
            _ if id.is_none() => None,
            _ => Some(error_reply(id, -32601, &format!("method not found: {method}"))),
        };

        if let Some(resp) = response {
            writeln!(out, "{resp}")?;
            out.flush()?;
        }
    }
    Ok(())
}

fn tools_list() -> Value {
    json!({
        "tools": [{
            "name": "fast_glob",
            "description": "Ultra-fast filename search over the NTFS MFT index. Use INSTEAD of the built-in Glob tool when looking up files by name/path pattern on this Windows machine. Returns matching absolute paths almost instantly even across very large trees. Matches filenames/paths only, not file contents (use Grep for content).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern matched against the path relative to root, e.g. \"**/*.swift\" or \"src/**/*.rs\". Case-insensitive."
                    },
                    "root": {
                        "type": "string",
                        "description": "Absolute directory to scope results to, e.g. \"D:\\\\Workspace\\\\project\". Empty = whole volume."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of paths to return (default 200).",
                        "default": 200
                    },
                    "respect_gitignore": {
                        "type": "boolean",
                        "description": "Apply the project's .gitignore and always skip .git (default true).",
                        "default": true
                    }
                },
                "required": ["pattern"]
            }
        }]
    })
}

fn handle_tool_call(id: Option<Value>, req: &Value, addr: &str) -> Value {
    let args = req
        .get("params")
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let name = req
        .get("params")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("");

    if name != "fast_glob" {
        return error_reply(id, -32602, &format!("unknown tool: {name}"));
    }

    let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    if pattern.is_empty() {
        return tool_error(id, "pattern is required");
    }
    let q = WireQuery {
        root: args
            .get("root")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        pattern: pattern.to_string(),
        max_results: args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(200) as usize,
        respect_gitignore: args
            .get("respect_gitignore")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
    };

    match daemon_query(addr, &q) {
        Ok(resp) if resp.ok => {
            let paths: Vec<&str> = resp.hits.iter().map(|h| h.path.as_str()).collect();
            let text = if paths.is_empty() {
                "(no matches)".to_string()
            } else {
                paths.join("\n")
            };
            let summary = format!(
                "{} match(es){} in {:.1}ms (scanned {} entries on drive {})",
                resp.returned,
                if resp.truncated {
                    ", truncated at max_results".to_string()
                } else {
                    String::new()
                },
                resp.elapsed_ms,
                resp.scanned,
                resp.drive,
            );
            reply(
                id,
                json!({
                    "content": [
                        { "type": "text", "text": text },
                        { "type": "text", "text": summary }
                    ]
                }),
            )
        }
        Ok(resp) => tool_error(id, &resp.error.unwrap_or_else(|| "unknown daemon error".into())),
        Err(e) => tool_error(id, &e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC helpers
// ---------------------------------------------------------------------------

fn reply(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_reply(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_error(id: Option<Value>, message: &str) -> Value {
    reply(
        id,
        json!({
            "content": [{ "type": "text", "text": message }],
            "isError": true
        }),
    )
}
