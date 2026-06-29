//! Query engine: caches a per-drive MFT index in memory, refreshes it
//! incrementally from the USN Journal, and answers glob queries.

use crate::mft::{build_index, Index};
use anyhow::{anyhow, Result};
use globset::{GlobBuilder, GlobMatcher};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, RwLock};
use std::time::Instant;

/// A single query result row.
#[derive(serde::Serialize, Clone)]
pub struct Hit {
    pub path: String,
    pub is_dir: bool,
}

/// Parameters for a fast_glob query.
pub struct Query<'a> {
    /// Root directory to scope results to, e.g. `D:\Workspace_local\project`.
    /// Empty means "no scoping" (whole indexed volume).
    pub root: &'a str,
    /// Glob pattern matched against the path relative to root, e.g. `**/*.swift`.
    pub pattern: &'a str,
    pub max_results: usize,
    pub respect_gitignore: bool,
}

/// Holds one cached index per drive letter.
pub struct Engine {
    indices: RwLock<HashMap<char, RwLock<Index>>>,
    /// Serializes (slow) full builds so two queries don't build the same drive twice.
    build_lock: Mutex<()>,
}

impl Engine {
    pub fn new() -> Self {
        Engine {
            indices: RwLock::new(HashMap::new()),
            build_lock: Mutex::new(()),
        }
    }

    /// Ensure the index for `drive` exists, building it once if needed.
    fn ensure_drive(&self, drive: char) -> Result<()> {
        if self.indices.read().unwrap().contains_key(&drive) {
            return Ok(());
        }
        // Only one builder at a time; re-check after acquiring the lock.
        let _g = self.build_lock.lock().unwrap();
        if self.indices.read().unwrap().contains_key(&drive) {
            return Ok(());
        }
        let idx = build_index(drive)?;
        self.indices
            .write()
            .unwrap()
            .insert(drive, RwLock::new(idx));
        Ok(())
    }

    /// Apply USN incremental updates to every cached drive. Returns total records seen.
    pub fn refresh_all(&self) -> usize {
        let mut total = 0;
        let map = self.indices.read().unwrap();
        for (drive, idx) in map.iter() {
            if let Ok(mut guard) = idx.write() {
                if let Ok(n) = guard.apply_journal_updates(*drive) {
                    total += n;
                }
            }
        }
        total
    }

    /// Build and cache the index for a drive up front (used by `bench`).
    pub fn warm(&self, drive: char) -> Result<()> {
        self.ensure_drive(drive)
    }

    /// Number of entries currently indexed across all drives.
    pub fn entry_count(&self) -> usize {
        self.indices
            .read()
            .unwrap()
            .values()
            .map(|i| i.read().unwrap().entries.len())
            .sum()
    }

    /// Run a glob query. Builds the drive index on first use.
    pub fn query(&self, q: &Query) -> Result<(Vec<Hit>, QueryStats)> {
        let started = Instant::now();

        // Determine which drive to search. Prefer the root's drive; else default C.
        let drive = drive_of(q.root).unwrap_or('C');
        self.ensure_drive(drive)?;

        // Normalize root for prefix comparison (lowercase, trailing slash trimmed).
        let root_norm = normalize_prefix(q.root);

        // Build the glob matcher. We match against the path *relative to root* so
        // patterns like `**/*.swift` behave intuitively; if no root, match full path.
        let matcher = build_matcher(q.pattern)?;

        // Cheap pre-filter on the leaf (basename) component of the pattern. Most
        // globs end in a filename pattern like `*.rs`, so we can reject the vast
        // majority of entries by matching their `name` field directly — avoiding
        // the expensive full-path reconstruction for non-candidates. If the last
        // segment is a `**` wildcard, we cannot pre-filter and fall back to full.
        let leaf_matcher = leaf_matcher(q.pattern)?;

        // Optional .gitignore scoping rooted at `root`.
        let gitignore = if q.respect_gitignore && !q.root.is_empty() {
            build_gitignore(q.root)
        } else {
            None
        };

        // If the pattern's leaf pins a fixed extension (e.g. `*.rs`, `*test*.rs`,
        // `Cargo.toml`), we only iterate that extension's bucket instead of every
        // entry — turning an O(all files) scan into O(matching extension). This is
        // the inverted-index trick that makes whole-volume queries sub-millisecond.
        let required_ext = required_ext(q.pattern);

        let map = self.indices.read().unwrap();
        let idx = map.get(&drive).ok_or_else(|| anyhow!("drive not indexed"))?;
        let idx = idx.read().unwrap();

        let mut hits = Vec::new();
        let mut scanned = 0usize;
        let mut truncated = false;

        // Per-entry match: reconstruct path, apply root scope, glob, gitignore.
        let try_one = |frn: u64, entry: &crate::mft::Entry| -> Option<Hit> {
            let full = idx.full_path(frn)?;

            let rel = if root_norm.is_empty() {
                full.as_str()
            } else {
                let full_lower = full.to_ascii_lowercase();
                if !full_lower.starts_with(&root_norm) {
                    return None;
                }
                let cut = root_norm.len();
                let bytes = full.as_bytes();
                let start = if cut < bytes.len() && bytes[cut] == b'\\' {
                    cut + 1
                } else {
                    cut
                };
                &full[start..]
            };
            if rel.is_empty() {
                return None;
            }

            let rel_fwd = rel.replace('\\', "/");
            if !matcher.is_match(&rel_fwd) {
                return None;
            }
            if let Some(gi) = &gitignore {
                if gi.matched(Path::new(&full), entry.is_dir).is_ignore() {
                    return None;
                }
            }
            Some(Hit {
                path: full,
                is_dir: entry.is_dir,
            })
        };

        match required_ext {
            // Fast path: only walk the extension bucket.
            Some(ext) => {
                if let Some(bucket) = idx.by_ext.get(&ext) {
                    for &frn in bucket {
                        scanned += 1;
                        if let Some(entry) = idx.entries.get(&frn) {
                            if let Some(hit) = try_one(frn, entry) {
                                hits.push(hit);
                                if hits.len() >= q.max_results {
                                    truncated = true;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            // Fallback: no fixed extension (e.g. `Makefile`, `foo*`, `**/x/**`).
            // Full scan with the cheap leaf pre-filter on the basename.
            None => {
                for (&frn, entry) in idx.entries.iter() {
                    scanned += 1;
                    if let Some(lm) = &leaf_matcher {
                        if !lm.is_match(&entry.name) {
                            continue;
                        }
                    }
                    if let Some(hit) = try_one(frn, entry) {
                        hits.push(hit);
                        if hits.len() >= q.max_results {
                            truncated = true;
                            break;
                        }
                    }
                }
            }
        }

        let stats = QueryStats {
            drive,
            scanned,
            returned: hits.len(),
            truncated,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        };
        Ok((hits, stats))
    }
}

/// Telemetry returned alongside results.
#[derive(serde::Serialize)]
pub struct QueryStats {
    pub drive: char,
    pub scanned: usize,
    pub returned: usize,
    pub truncated: bool,
    pub elapsed_ms: f64,
}

/// Extract the drive letter from a path like `D:\foo`.
fn drive_of(path: &str) -> Option<char> {
    let mut chars = path.chars();
    let c = chars.next()?;
    if chars.next() == Some(':') && c.is_ascii_alphabetic() {
        Some(c.to_ascii_uppercase())
    } else {
        None
    }
}

/// Lowercased root with trailing slashes trimmed, for prefix comparison.
fn normalize_prefix(root: &str) -> String {
    if root.is_empty() {
        return String::new();
    }
    root.trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

/// Build a case-insensitive glob matcher with `**` enabled across separators.
fn build_matcher(pattern: &str) -> Result<GlobMatcher> {
    let glob = GlobBuilder::new(pattern)
        .case_insensitive(true)
        .literal_separator(false)
        .build()?;
    Ok(glob.compile_matcher())
}

/// Build a matcher for the leaf (last) component of `pattern`, used as a cheap
/// pre-filter against an entry's basename. Returns `None` when the last segment
/// is a recursive `**` wildcard (it would match any name, so pre-filtering is
/// pointless and we must scan fully).
fn leaf_matcher(pattern: &str) -> Result<Option<GlobMatcher>> {
    // Normalize separators, then take the final path segment.
    let leaf = pattern.replace('\\', "/");
    let leaf = leaf.rsplit('/').next().unwrap_or(pattern);
    if leaf.is_empty() || leaf.contains("**") {
        return Ok(None);
    }
    let glob = GlobBuilder::new(leaf)
        .case_insensitive(true)
        .literal_separator(false)
        .build()?;
    Ok(Some(glob.compile_matcher()))
}

/// If the pattern's leaf pins a fixed file extension, return it (lowercased,
/// without dot). This holds when the substring after the leaf's last `.` has no
/// glob metacharacter — e.g. `*.rs`, `*test*.rs`, `Cargo.toml` all pin `rs`/`toml`.
/// Returns `None` for `*.{a,b}`, `Makefile`, `foo*`, dotfiles, or `**`-leaves.
fn required_ext(pattern: &str) -> Option<String> {
    let leaf = pattern.replace('\\', "/");
    let leaf = leaf.rsplit('/').next().unwrap_or(pattern);
    if leaf.contains("**") {
        return None;
    }
    let p = leaf.rfind('.')?;
    if p == 0 || p + 1 >= leaf.len() {
        return None;
    }
    let suffix = &leaf[p + 1..];
    if suffix.chars().any(is_glob_meta) {
        return None;
    }
    Some(suffix.to_ascii_lowercase())
}

fn is_glob_meta(c: char) -> bool {
    matches!(c, '*' | '?' | '[' | ']' | '{' | '}')
}

/// Build a Gitignore matcher rooted at `root`, merging root/.gitignore if present.
fn build_gitignore(root: &str) -> Option<Gitignore> {
    let mut b = GitignoreBuilder::new(root);
    // Add the project's own .gitignore if it exists.
    let gi_path = Path::new(root).join(".gitignore");
    if gi_path.exists() {
        b.add(gi_path);
    }
    // Always ignore the .git directory itself.
    let _ = b.add_line(None, ".git/");
    b.build().ok()
}
