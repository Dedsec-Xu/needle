//! Tiny line-delimited JSON IPC between the elevated daemon (`serve`) and the
//! non-elevated MCP frontend (`mcp`). Loopback TCP keeps it simple and avoids
//! named-pipe ACL juggling across integrity levels.

use serde::{Deserialize, Serialize};

/// Default loopback endpoint for the daemon.
pub const DEFAULT_ADDR: &str = "127.0.0.1:48923";

/// A query sent from the MCP frontend to the daemon. Newer optional fields use
/// serde defaults so older clients/servers stay compatible.
#[derive(Serialize, Deserialize)]
pub struct WireQuery {
    pub root: String,
    pub pattern: String,
    pub max_results: usize,
    pub respect_gitignore: bool,
    /// "any" | "file" | "dir".
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Case-sensitive path matching (default false = case-insensitive).
    #[serde(default)]
    pub case_sensitive: bool,
    /// "name" | "mtime" | "size" | "none".
    #[serde(default = "default_sort")]
    pub sort: String,
    /// "asc" | "desc".
    #[serde(default = "default_order")]
    pub order: String,
}

fn default_kind() -> String {
    "any".into()
}
fn default_sort() -> String {
    "none".into()
}
fn default_order() -> String {
    "asc".into()
}

/// A hit row on the wire. `size`/`mtime` are populated only when a metadata sort
/// was requested (the daemon stats matched candidates lazily).
#[derive(Serialize, Deserialize, Clone)]
pub struct WireHit {
    pub path: String,
    pub is_dir: bool,
    #[serde(default)]
    pub size: Option<u64>,
    /// Last-modified time, Unix milliseconds.
    #[serde(default)]
    pub mtime: Option<i64>,
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
    /// True when a metadata sort could not see the full candidate set (it hit the
    /// lazy-stat cap), so top-k may be approximate.
    #[serde(default)]
    pub sort_approximate: bool,
}
