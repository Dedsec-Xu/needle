//! needle: whole-machine, sub-millisecond file search for AI agents, exposed as
//! a CLI and an MCP server (`fast_glob` tool) so an agent can find any file on
//! the machine instantly instead of walking the filesystem.
//!
//! Because reading the MFT requires admin but Claude Code launches MCP servers
//! non-elevated, the tool splits into two roles: `serve` (the elevated daemon
//! that holds the index, applies USN updates, and answers queries over loopback
//! TCP) and `mcp` (the non-elevated frontend launched by Claude Code that
//! forwards the `fast_glob` tool to the daemon). `query` runs a one-shot
//! in-process search (must itself be elevated).

mod engine;
mod ipc;
mod mft;
mod service;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use engine::{Engine, Kind, Order, Query, SortKey};
use ipc::{WireHit, WireQuery, WireResponse, DEFAULT_ADDR};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Most paths `fast_glob` puts inline in the agent's context.
const MAX_INLINE: usize = 1000;
/// Most paths written to a results file (~100 MB of paths).
const SPILL_CAP: usize = 1_000_000;
const OLD_DAEMON: &str =
    "the running needle daemon is too old to count matches; reinstall it with: needle service install";

#[derive(Parser)]
#[command(
    name = "needle",
    version,
    about = "Whole-machine sub-millisecond file search for AI agents (NTFS MFT + USN)"
)]
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
        /// Restrict to "file", "dir", or "any".
        #[arg(long, default_value = "any")]
        kind: String,
        #[arg(long, default_value_t = false)]
        case_sensitive: bool,
        /// Sort by "name", "mtime", "size", or "none".
        #[arg(long, default_value = "none")]
        sort: String,
        /// "asc" or "desc".
        #[arg(long, default_value = "asc")]
        order: String,
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
        /// Restrict to "file", "dir", or "any".
        #[arg(long, default_value = "any")]
        kind: String,
        #[arg(long, default_value_t = false)]
        case_sensitive: bool,
        /// Sort by "name", "mtime", "size", or "none".
        #[arg(long, default_value = "none")]
        sort: String,
        /// "asc" or "desc".
        #[arg(long, default_value = "asc")]
        order: String,
        /// Print only the exact number of matches.
        #[arg(long, default_value_t = false)]
        count: bool,
        /// Write matching paths (one per line) to this file instead of stdout.
        #[arg(long)]
        export_txt: Option<String>,
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr: String,
    },
    /// es.exe-compatible subset over the daemon: -n, -path, /ad, /a-d, -i,
    /// -sort, -get-result-count, -export-txt. One search term, no Everything syntax.
    Es {
        #[arg(allow_hyphen_values = true, trailing_var_arg = true, num_args = 0..)]
        args: Vec<String>,
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
            kind,
            case_sensitive,
            sort,
            order,
            json,
        } => run_query(QueryArgs {
            pattern: &pattern,
            root: &root,
            max_results,
            respect_gitignore,
            kind: &kind,
            case_sensitive,
            sort: &sort,
            order: &order,
            json,
        }),
        Cmd::Find {
            pattern,
            root,
            max_results,
            respect_gitignore,
            kind,
            case_sensitive,
            sort,
            order,
            count,
            export_txt,
            addr,
        } => run_find(FindArgs {
            addr: &addr,
            pattern: &pattern,
            root: &root,
            max_results,
            respect_gitignore,
            kind: &kind,
            case_sensitive,
            sort: &sort,
            order: &order,
            count,
            export_txt: export_txt.as_deref(),
        }),
        Cmd::Es { args } => {
            let e = parse_es(&args)?;
            run_find(FindArgs {
                addr: DEFAULT_ADDR,
                pattern: &e.pattern,
                root: &e.root,
                max_results: e.max_results,
                respect_gitignore: false,
                kind: e.kind,
                case_sensitive: e.case_sensitive,
                sort: e.sort,
                order: e.order,
                count: e.count,
                export_txt: e.export_txt.as_deref(),
            })
        }
        Cmd::Index { drive } => run_index(drive),
        Cmd::Bench { drive } => run_bench(drive),
        Cmd::Serve { addr } => run_serve(&addr),
        Cmd::Mcp { addr } => run_mcp(&addr),
        Cmd::Service { action } => match action {
            ServiceAction::Install => {
                if service::ensure_elevated(&["service", "install"])? {
                    return Ok(());
                }
                service::install()?;
                eprintln!(
                    "[needle] service '{}' installed and started (LocalSystem, auto-start).",
                    service::SERVICE_NAME
                );
                Ok(())
            }
            ServiceAction::Uninstall => {
                if service::ensure_elevated(&["service", "uninstall"])? {
                    return Ok(());
                }
                service::uninstall()?;
                eprintln!("[needle] service '{}' removed.", service::SERVICE_NAME);
                Ok(())
            }
            ServiceAction::Run => service::run_dispatch(),
        },
    }
}

struct QueryArgs<'a> {
    pattern: &'a str,
    root: &'a str,
    max_results: usize,
    respect_gitignore: bool,
    kind: &'a str,
    case_sensitive: bool,
    sort: &'a str,
    order: &'a str,
    json: bool,
}

fn run_query(a: QueryArgs) -> Result<()> {
    let engine = Engine::new();
    let (hits, stats) = engine.query(&Query {
        root: a.root,
        pattern: a.pattern,
        max_results: a.max_results,
        respect_gitignore: a.respect_gitignore,
        kind: Kind::parse(a.kind),
        case_sensitive: a.case_sensitive,
        sort: SortKey::parse(a.sort),
        order: Order::parse(a.order),
        count_all: false,
    })?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&hits)?);
    } else {
        for h in &hits {
            println!("{}", h.path);
        }
    }
    eprintln!(
        "[needle] drive {} scanned {} returned {}{}{} in {:.1}ms",
        stats.drive,
        stats.scanned,
        stats.returned,
        if stats.truncated { " (truncated)" } else { "" },
        if stats.sort_approximate {
            " (sort approximate)"
        } else {
            ""
        },
        stats.elapsed_ms,
    );
    Ok(())
}

struct FindArgs<'a> {
    addr: &'a str,
    pattern: &'a str,
    root: &'a str,
    max_results: usize,
    respect_gitignore: bool,
    kind: &'a str,
    case_sensitive: bool,
    sort: &'a str,
    order: &'a str,
    count: bool,
    export_txt: Option<&'a str>,
}

fn run_find(a: FindArgs) -> Result<()> {
    let q = WireQuery {
        root: a.root.to_string(),
        pattern: a.pattern.to_string(),
        max_results: if a.count { 0 } else { a.max_results },
        respect_gitignore: a.respect_gitignore,
        kind: a.kind.to_string(),
        case_sensitive: a.case_sensitive,
        sort: a.sort.to_string(),
        order: a.order.to_string(),
        count_all: a.count,
    };
    let resp = daemon_query(a.addr, &q)?;
    if !resp.ok {
        return Err(anyhow!(resp.error.unwrap_or_else(|| "daemon error".into())));
    }
    if a.count {
        println!("{}", resp.total.ok_or_else(|| anyhow!(OLD_DAEMON))?);
        return Ok(());
    }
    match a.export_txt {
        Some(path) => write_lines(Path::new(path), &resp.hits)?,
        None => {
            for h in &resp.hits {
                println!("{}", h.path);
            }
        }
    }
    eprintln!(
        "[needle] {} match(es){}{} in {:.2}ms (scanned {} on drive {})",
        resp.returned,
        if resp.truncated { " (truncated)" } else { "" },
        if resp.sort_approximate {
            " (sort approximate)"
        } else {
            ""
        },
        resp.elapsed_ms,
        resp.scanned,
        resp.drive,
    );
    Ok(())
}

struct EsArgs {
    pattern: String,
    root: String,
    max_results: usize,
    kind: &'static str,
    case_sensitive: bool,
    sort: &'static str,
    order: &'static str,
    count: bool,
    export_txt: Option<String>,
}

/// Parse the supported es.exe subset. Anything else is rejected, never ignored,
/// so a caller never believes an unsupported filter was applied.
fn parse_es(args: &[String]) -> Result<EsArgs> {
    let mut e = EsArgs {
        pattern: String::new(),
        root: String::new(),
        max_results: SPILL_CAP,
        kind: "any",
        case_sensitive: false,
        sort: "none",
        order: "asc",
        count: false,
        export_txt: None,
    };
    let mut term: Option<&str> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .map(String::as_str)
                .ok_or_else(|| anyhow!("{arg} needs a value"))
        };
        match arg.to_ascii_lowercase().as_str() {
            "-n" => e.max_results = value()?.parse()?,
            "-path" => e.root = value()?.to_string(),
            "-export-txt" => e.export_txt = Some(value()?.to_string()),
            "-get-result-count" => e.count = true,
            "/ad" => e.kind = "dir",
            "/a-d" => e.kind = "file",
            "-i" | "-case" => e.case_sensitive = true,
            "-sort-ascending" => e.order = "asc",
            "-sort-descending" => e.order = "desc",
            "-sort" => {
                let v = value()?.to_ascii_lowercase();
                let (key, order) = match v.strip_suffix("-descending") {
                    Some(k) => (k.to_string(), "desc"),
                    None => (
                        v.strip_suffix("-ascending").unwrap_or(&v).to_string(),
                        e.order,
                    ),
                };
                e.order = order;
                e.sort = match key.as_str() {
                    "name" => "name",
                    "size" => "size",
                    "date-modified" | "dm" => "mtime",
                    _ => return Err(anyhow!("unsupported es sort: {key}")),
                };
            }
            _ if arg.starts_with('-') || arg.starts_with('/') => {
                return Err(anyhow!("unsupported es option: {arg}"))
            }
            _ if term.is_some() => return Err(anyhow!("only one search term is supported")),
            _ => term = Some(arg),
        }
    }
    e.pattern = es_pattern(term.unwrap_or("*"))?;
    Ok(e)
}

/// Map an es search term to a glob: wildcards match a whole name, a plain term
/// is a substring match (Everything's default).
fn es_pattern(term: &str) -> Result<String> {
    if term.contains([':', '|', '!', '<', '>', '"']) {
        return Err(anyhow!(
            "Everything search syntax is not supported: {term} (use -path for a folder)"
        ));
    }
    let term = term.replace('\\', "/");
    Ok(if term.contains(['*', '?']) {
        format!("**/{term}")
    } else {
        format!("**/*{term}*")
    })
}

fn write_lines(path: &Path, hits: &[WireHit]) -> Result<()> {
    let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
    for h in hits {
        writeln!(w, "{}", h.path)?;
    }
    w.flush()?;
    Ok(())
}

/// Remove files in `dir` at least `max_age` old.
fn prune(dir: &Path, max_age: Duration) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= max_age);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Save a full result list under the user's %LOCALAPPDATA% (per-user ACL).
/// Written by the non-elevated MCP process, never by the LocalSystem daemon.
// shortcut: age-only cleanup (24h), no total-size cap; add one if spills pile up.
fn spill(hits: &[WireHit]) -> Result<PathBuf> {
    let dir = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("needle")
        .join("results");
    std::fs::create_dir_all(&dir)?;
    prune(&dir, Duration::from_secs(24 * 3600));
    let ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let path = dir.join(format!("{ms}-{}.txt", std::process::id()));
    write_lines(&path, hits)?;
    Ok(path)
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
            kind: Kind::Any,
            case_sensitive: false,
            sort: SortKey::None,
            order: Order::Asc,
            count_all: false,
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

    // No polling thread: each drive index spawns its own USN-journal watcher when
    // it is first built (see Engine::ensure_drive), which blocks on the journal
    // and applies changes the instant NTFS records them.

    let listener = TcpListener::bind(addr).map_err(|e| anyhow!("failed to bind {addr}: {e}"))?;
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
            kind: Kind::parse(&q.kind),
            case_sensitive: q.case_sensitive,
            sort: SortKey::parse(&q.sort),
            order: Order::parse(&q.order),
            count_all: q.count_all,
        }) {
            Ok((hits, stats)) => WireResponse {
                ok: true,
                error: None,
                hits: hits
                    .into_iter()
                    .map(|h| WireHit {
                        path: h.path,
                        is_dir: h.is_dir,
                        size: h.size,
                        mtime: h.mtime,
                    })
                    .collect(),
                drive: stats.drive.to_string(),
                scanned: stats.scanned,
                returned: stats.returned,
                truncated: stats.truncated,
                sort_approximate: stats.sort_approximate,
                total: stats.total,
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
        sort_approximate: false,
        total: None,
        elapsed_ms: 0.0,
    }
}

/// Send one query to the daemon and read its response.
fn daemon_query(addr: &str, q: &WireQuery) -> Result<WireResponse> {
    let stream = TcpStream::connect(addr).map_err(|e| {
        anyhow!("cannot reach daemon at {addr}: {e}. Start it with: needle serve (as admin)")
    })?;
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
            _ => Some(error_reply(
                id,
                -32601,
                &format!("method not found: {method}"),
            )),
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
            "description": "Ultra-fast filename search over the NTFS MFT index. Use INSTEAD of the built-in Glob tool when looking up files by name/path pattern on this Windows machine. Returns matching absolute paths almost instantly even across very large trees. Matches filenames/paths only, not file contents (use Grep for content). To ask how many files match, use count_only instead of listing them. When more than max_results match, the complete list is saved to a text file (one path per line) and its path is returned; query that file with rg / Select-String instead of reading it whole.",
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
                        "description": "Maximum number of paths to return inline (default 200, max 1000). Larger result sets go to a file.",
                        "default": 200
                    },
                    "count_only": {
                        "type": "boolean",
                        "description": "Return only the exact number of matches, no paths (default false).",
                        "default": false
                    },
                    "spill": {
                        "type": "string",
                        "enum": ["auto", "never"],
                        "description": "auto: when results exceed max_results, save the full list (up to 1,000,000 paths, unsorted) to a file and return its path. never: only return inline results (default auto).",
                        "default": "auto"
                    },
                    "respect_gitignore": {
                        "type": "boolean",
                        "description": "Apply the project's .gitignore and always skip .git (default true).",
                        "default": true
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["any", "file", "dir"],
                        "description": "Restrict results to files, directories, or any (default any).",
                        "default": "any"
                    },
                    "case_sensitive": {
                        "type": "boolean",
                        "description": "Case-sensitive pattern matching (default false).",
                        "default": false
                    },
                    "sort": {
                        "type": "string",
                        "enum": ["none", "name", "mtime", "size"],
                        "description": "Sort results. 'mtime'/'size' lazily stat matched files (good for 'most recent' or 'largest'); over ~5000 matches the top-k becomes approximate. Default none (fastest).",
                        "default": "none"
                    },
                    "order": {
                        "type": "string",
                        "enum": ["asc", "desc"],
                        "description": "Sort order (default asc). Use 'desc' with sort=mtime for newest-first, or sort=size for largest-first.",
                        "default": "asc"
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
    let count_only = args
        .get("count_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let spill_auto = args.get("spill").and_then(|v| v.as_str()) != Some("never");
    let mut q = WireQuery {
        root: args
            .get("root")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        pattern: pattern.to_string(),
        max_results: args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(200)
            .min(MAX_INLINE as u64) as usize,
        respect_gitignore: args
            .get("respect_gitignore")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        kind: args
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("any")
            .to_string(),
        case_sensitive: args
            .get("case_sensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        sort: args
            .get("sort")
            .and_then(|v| v.as_str())
            .unwrap_or("none")
            .to_string(),
        order: args
            .get("order")
            .and_then(|v| v.as_str())
            .unwrap_or("asc")
            .to_string(),
        count_all: false,
    };

    if count_only {
        q.max_results = 0;
        q.count_all = true;
        return match ask(addr, &q) {
            Ok(resp) => match resp.total {
                Some(total) => reply(
                    id,
                    json!({ "content": [{ "type": "text", "text": format!(
                        "{total} match(es) in {:.1}ms (scanned {} entries on drive {})",
                        resp.elapsed_ms, resp.scanned, resp.drive
                    ) }] }),
                ),
                None => tool_error(id, OLD_DAEMON),
            },
            Err(e) => tool_error(id, &e),
        };
    }

    let resp = match ask(addr, &q) {
        Ok(resp) => resp,
        Err(e) => return tool_error(id, &e),
    };
    let paths: Vec<&str> = resp.hits.iter().map(|h| h.path.as_str()).collect();
    let text = if paths.is_empty() {
        "(no matches)".to_string()
    } else {
        paths.join("\n")
    };
    let mut summary = format!(
        "{} match(es){} in {:.1}ms (scanned {} entries on drive {})",
        resp.returned,
        if resp.truncated {
            ", truncated at max_results"
        } else {
            ""
        },
        resp.elapsed_ms,
        resp.scanned,
        resp.drive,
    );
    if resp.truncated && spill_auto {
        let mut full = q.clone();
        full.max_results = SPILL_CAP;
        full.count_all = true;
        // Stat-ing up to a million files for a metadata sort is not viable.
        full.sort = "none".into();
        let saved = ask(addr, &full)
            .and_then(|r| spill(&r.hits).map(|p| (r, p)).map_err(|e| e.to_string()));
        summary = match saved {
            Ok((r, path)) => format!(
                "{} of {} match(es) shown. Full list ({} lines{}{}) saved to:\n{}\n\
                 Query it with rg / Select-String instead of reading it whole.",
                resp.returned,
                match (r.total, r.truncated) {
                    (Some(t), _) => t.to_string(),
                    // Pre-0.2 daemon: an uncapped list is still the exact total.
                    (None, false) => r.returned.to_string(),
                    (None, true) => format!("at least {}", r.returned),
                },
                r.returned,
                if r.truncated {
                    ", capped at 1,000,000"
                } else {
                    ""
                },
                if q.sort == "none" { "" } else { ", unsorted" },
                path.display(),
            ),
            Err(e) => format!("{summary} (could not save the full list: {e})"),
        };
    }
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

/// One daemon round trip, with daemon-side failures folded into the error.
fn ask(addr: &str, q: &WireQuery) -> std::result::Result<WireResponse, String> {
    match daemon_query(addr, q) {
        Ok(resp) if resp.ok => Ok(resp),
        Ok(resp) => Err(resp.error.unwrap_or_else(|| "unknown daemon error".into())),
        Err(e) => Err(e.to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn es(args: &[&str]) -> Result<EsArgs> {
        parse_es(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn es_maps_supported_options() {
        let e = es(&[
            "-n",
            "5",
            "-path",
            "D:/x",
            "/a-d",
            "-i",
            "-sort",
            "size-descending",
            "-get-result-count",
            "*.exe",
        ])
        .unwrap();
        assert_eq!(e.pattern, "**/*.exe");
        assert_eq!(
            (e.max_results, e.root.as_str(), e.kind),
            (5, "D:/x", "file")
        );
        assert!(e.case_sensitive && e.count);
        assert_eq!((e.sort, e.order), ("size", "desc"));
    }

    #[test]
    fn es_plain_term_is_substring_and_defaults_unlimited() {
        let e = es(&["chrome"]).unwrap();
        assert_eq!(e.pattern, "**/*chrome*");
        assert_eq!(e.max_results, SPILL_CAP);
        assert_eq!(es(&[]).unwrap().pattern, "**/*");
    }

    #[test]
    fn es_rejects_what_it_cannot_honor() {
        assert!(es(&["-dm", "x"]).is_err());
        assert!(es(&["/a-h"]).is_err());
        assert!(es(&["ext:exe"]).is_err());
        assert!(es(&["a", "b"]).is_err());
        assert!(es(&["-n"]).is_err());
    }

    #[test]
    fn prune_removes_expired_files() {
        let dir = std::env::temp_dir().join(format!("needle-prune-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("old.txt");
        write_lines(&f, &[]).unwrap();
        prune(&dir, Duration::from_secs(3600));
        assert!(f.exists());
        prune(&dir, Duration::ZERO);
        assert!(!f.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
