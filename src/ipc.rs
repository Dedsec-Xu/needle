//! Tiny line-delimited JSON IPC between the elevated daemon (`serve`) and the
//! non-elevated MCP frontend (`mcp`). Loopback TCP keeps it simple and avoids
//! named-pipe ACL juggling across integrity levels.

use serde::{Deserialize, Serialize};

/// Default loopback endpoint for the daemon.
pub const DEFAULT_ADDR: &str = "127.0.0.1:48923";

/// A query sent from the MCP frontend to the daemon.
#[derive(Serialize, Deserialize)]
pub struct WireQuery {
    pub root: String,
    pub pattern: String,
    pub max_results: usize,
    pub respect_gitignore: bool,
}

/// A hit row on the wire.
#[derive(Serialize, Deserialize, Clone)]
pub struct WireHit {
    pub path: String,
    pub is_dir: bool,
}

/// The daemon's response.
#[derive(Serialize, Deserialize)]
pub struct WireResponse {
    pub ok: bool,
    pub error: Option<String>,
    pub hits: Vec<WireHit>,
    pub drive: String,
    pub scanned: usize,
    pub returned: usize,
    pub truncated: bool,
    pub elapsed_ms: f64,
}
