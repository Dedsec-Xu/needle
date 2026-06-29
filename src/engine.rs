//! Query engine: caches a per-drive MFT index in memory, refreshes it
//! incrementally from the USN Journal, and answers glob queries.

use crate::mft::{build_index, ntfs_drives, watch_drive, Index};
use anyhow::Result;
use globset::{GlobBuilder, GlobMatcher};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Instant;

/// A single query result row. `size`/`mtime` are filled only when a metadata
/// sort was requested (the engine stats matched candidates lazily).
#[derive(serde::Serialize, Clone)]
pub struct Hit {
    pub path: String,
    pub is_dir: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
}

/// Which entries to return.
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Any,
    File,
    Dir,
}

/// Sort key for results.
#[derive(Clone, Copy, PartialEq)]
pub enum SortKey {
    None,
    Name,
    Mtime,
    Size,
}

/// Sort direction.
#[derive(Clone, Copy, PartialEq)]
pub enum Order {
    Asc,
    Desc,
}

impl Kind {
    pub fn parse(s: &str) -> Kind {
        match s.to_ascii_lowercase().as_str() {
            "file" => Kind::File,
            "dir" | "directory" => Kind::Dir,
            _ => Kind::Any,
        }
    }
    fn allows(self, is_dir: bool) -> bool {
        match self {
            Kind::Any => true,
            Kind::File => !is_dir,
            Kind::Dir => is_dir,
        }
    }
}

impl SortKey {
    pub fn parse(s: &str) -> SortKey {
        match s.to_ascii_lowercase().as_str() {
            "name" => SortKey::Name,
            "mtime" | "modified" | "date" => SortKey::Mtime,
            "size" => SortKey::Size,
            _ => SortKey::None,
        }
    }
    fn needs_stat(self) -> bool {
        matches!(self, SortKey::Mtime | SortKey::Size)
    }
}

impl Order {
    pub fn parse(s: &str) -> Order {
        match s.to_ascii_lowercase().as_str() {
            "desc" | "descending" => Order::Desc,
            _ => Order::Asc,
        }
    }
}

/// Max candidates the engine will lazily stat for a metadata sort. Beyond this,
/// top-k by mtime/size is approximate (computed over an arbitrary subset).
const STAT_CAP: usize = 5000;

/// Parameters for a fast_glob query.
pub struct Query<'a> {
    /// Root directory to scope results to, e.g. `D:\Workspace_local\project`.
    /// Empty means "no scoping" (whole indexed volume).
    pub root: &'a str,
    /// Glob pattern matched against the path relative to root, e.g. `**/*.swift`.
    pub pattern: &'a str,
    pub max_results: usize,
    pub respect_gitignore: bool,
    pub kind: Kind,
    pub case_sensitive: bool,
    pub sort: SortKey,
    pub order: Order,
}

/// Holds one cached index per drive letter. Each index is an `Arc<RwLock<_>>` so
/// its dedicated USN-watcher thread can hold a handle independent of the map lock.
pub struct Engine {
    indices: RwLock<HashMap<char, Arc<RwLock<Index>>>>,
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
        let idx = Arc::new(RwLock::new(build_index(drive)?));
        self.indices
            .write()
            .unwrap()
            .insert(drive, Arc::clone(&idx));
        // Spawn this drive's dedicated USN watcher: it blocks on the journal and
        // applies every change the instant NTFS records it (Everything-style),
        // and self-heals by rebuilding from the MFT if the journal goes stale.
        thread::spawn(move || watch_drive(idx));
        Ok(())
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

    /// Run a glob query. Builds drive indices on first use.
    ///
    /// With a non-empty `root`, only that root's drive is searched. With an empty
    /// `root` the query fans out across **every NTFS volume** on the machine —
    /// this is the whole-machine search that is needle's core promise.
    pub fn query(&self, q: &Query) -> Result<(Vec<Hit>, QueryStats)> {
        let started = Instant::now();

        // Decide the drive set. A scoped root pins one drive; an empty root means
        // "the whole machine", so we enumerate every NTFS volume.
        let drives: Vec<char> = match drive_of(q.root) {
            Some(d) => vec![d],
            None => {
                let mut d = ntfs_drives();
                if d.is_empty() {
                    d.push('C'); // last-resort fallback if enumeration found nothing
                }
                d
            }
        };
        for &d in &drives {
            self.ensure_drive(d)?;
        }

        // Normalize root for prefix comparison (lowercase, trailing slash trimmed).
        let root_norm = normalize_prefix(q.root);

        // Build the glob matcher. We match against the path *relative to root* so
        // patterns like `**/*.swift` behave intuitively; if no root, match full path.
        let matcher = build_matcher(q.pattern, q.case_sensitive)?;

        // Cheap pre-filter on the leaf (basename) component of the pattern. Most
        // globs end in a filename pattern like `*.rs`, so we can reject the vast
        // majority of entries by matching their `name` field directly — avoiding
        // the expensive full-path reconstruction for non-candidates. If the last
        // segment is a `**` wildcard, we cannot pre-filter and fall back to full.
        let leaf_matcher = leaf_matcher(q.pattern, q.case_sensitive)?;

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

        // Without a sort we stream and stop at `max_results` (the sub-millisecond
        // fast path). With a sort we must see the whole candidate set before
        // picking top-k, so we collect up to STAT_CAP and sort afterwards.
        let collect_cap = if q.sort == SortKey::None {
            q.max_results
        } else {
            STAT_CAP.max(q.max_results)
        };

        let mut hits = Vec::new();
        let mut scanned = 0usize;
        let mut cap_hit = false;

        let map = self.indices.read().unwrap();
        for &drive in &drives {
            if hits.len() >= collect_cap {
                cap_hit = true;
                break;
            }
            let Some(idx) = map.get(&drive) else { continue };
            let idx = idx.read().unwrap();
            let remaining = collect_cap - hits.len();
            let (s, t) = scan_index(
                &idx,
                &root_norm,
                &matcher,
                &leaf_matcher,
                &gitignore,
                &required_ext,
                q.kind,
                remaining,
                &mut hits,
            );
            scanned += s;
            cap_hit |= t;
        }
        drop(map);

        let mut truncated = cap_hit;
        let mut sort_approximate = false;
        if q.sort != SortKey::None {
            // A metadata sort needs file size/mtime, fetched lazily per candidate.
            if q.sort.needs_stat() {
                for h in hits.iter_mut() {
                    if let Ok(md) = std::fs::metadata(&h.path) {
                        h.size = Some(md.len());
                        if let Ok(t) = md.modified() {
                            if let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) {
                                h.mtime = Some(d.as_millis() as i64);
                            }
                        }
                    }
                }
            }
            sort_hits(&mut hits, q.sort, q.order);
            // If we filled the candidate cap, top-k is over an arbitrary subset.
            sort_approximate = cap_hit;
            truncated = false;
            if hits.len() > q.max_results {
                hits.truncate(q.max_results);
                truncated = true;
            }
        }

        let drive = drives
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let stats = QueryStats {
            drive,
            scanned,
            returned: hits.len(),
            truncated,
            sort_approximate,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        };
        Ok((hits, stats))
    }
}

/// Sort hits in place by the requested key and order.
fn sort_hits(hits: &mut [Hit], key: SortKey, order: Order) {
    match key {
        SortKey::Name => hits.sort_by(|a, b| {
            basename(&a.path)
                .to_ascii_lowercase()
                .cmp(&basename(&b.path).to_ascii_lowercase())
        }),
        SortKey::Mtime => hits.sort_by(|a, b| {
            a.mtime
                .unwrap_or(i64::MIN)
                .cmp(&b.mtime.unwrap_or(i64::MIN))
        }),
        SortKey::Size => hits.sort_by(|a, b| a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0))),
        SortKey::None => {}
    }
    if order == Order::Desc {
        hits.reverse();
    }
}

/// Final path component (filename) of a Windows path.
fn basename(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Scan a single drive's index, appending up to `remaining` hits. Returns
/// `(entries_scanned, truncated)` where `truncated` means the `remaining` cap
/// was reached before the scan finished.
#[allow(clippy::too_many_arguments)]
fn scan_index(
    idx: &Index,
    root_norm: &str,
    matcher: &GlobMatcher,
    leaf_matcher: &Option<GlobMatcher>,
    gitignore: &Option<Gitignore>,
    required_ext: &Option<String>,
    kind: Kind,
    remaining: usize,
    hits: &mut Vec<Hit>,
) -> (usize, bool) {
    let mut scanned = 0usize;
    let start_len = hits.len();
    let limit = start_len + remaining;

    // Per-entry match: reconstruct path, apply root scope, glob, gitignore.
    let try_one = |frn: u64, entry: &crate::mft::Entry| -> Option<Hit> {
        if !kind.allows(entry.is_dir) {
            return None;
        }
        let full = idx.full_path(frn)?;

        let rel = if root_norm.is_empty() {
            full.as_str()
        } else {
            let full_lower = full.to_ascii_lowercase();
            if !full_lower.starts_with(root_norm) {
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
        if let Some(gi) = gitignore {
            if gi.matched(Path::new(&full), entry.is_dir).is_ignore() {
                return None;
            }
        }
        Some(Hit {
            path: full,
            is_dir: entry.is_dir,
            size: None,
            mtime: None,
        })
    };

    match required_ext {
        // Fast path: only walk the extension bucket.
        Some(ext) => {
            if let Some(bucket) = idx.by_ext.get(ext) {
                for &frn in bucket {
                    scanned += 1;
                    if let Some(entry) = idx.entries.get(&frn) {
                        if let Some(hit) = try_one(frn, entry) {
                            hits.push(hit);
                            if hits.len() >= limit {
                                return (scanned, true);
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
                if let Some(lm) = leaf_matcher {
                    if !lm.is_match(&entry.name) {
                        continue;
                    }
                }
                if let Some(hit) = try_one(frn, entry) {
                    hits.push(hit);
                    if hits.len() >= limit {
                        return (scanned, true);
                    }
                }
            }
        }
    }
    (scanned, false)
}

/// Telemetry returned alongside results.
#[derive(serde::Serialize)]
pub struct QueryStats {
    /// Drive(s) searched, e.g. `"C"` or `"C,D,E"` for a whole-machine query.
    pub drive: String,
    pub scanned: usize,
    pub returned: usize,
    pub truncated: bool,
    /// True when a metadata sort hit the lazy-stat cap, so top-k is approximate.
    pub sort_approximate: bool,
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
    root.trim_end_matches(['\\', '/']).to_ascii_lowercase()
}

/// Build a glob matcher with `**` enabled across separators.
fn build_matcher(pattern: &str, case_sensitive: bool) -> Result<GlobMatcher> {
    let glob = GlobBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .literal_separator(false)
        .build()?;
    Ok(glob.compile_matcher())
}

/// Build a matcher for the leaf (last) component of `pattern`, used as a cheap
/// pre-filter against an entry's basename. Returns `None` when the last segment
/// is a recursive `**` wildcard (it would match any name, so pre-filtering is
/// pointless and we must scan fully).
fn leaf_matcher(pattern: &str, case_sensitive: bool) -> Result<Option<GlobMatcher>> {
    // Normalize separators, then take the final path segment.
    let leaf = pattern.replace('\\', "/");
    let leaf = leaf.rsplit('/').next().unwrap_or(pattern);
    if leaf.is_empty() || leaf.contains("**") {
        return Ok(None);
    }
    let glob = GlobBuilder::new(leaf)
        .case_insensitive(!case_sensitive)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_ext_pins_extension() {
        assert_eq!(required_ext("**/*.rs").as_deref(), Some("rs"));
        assert_eq!(required_ext("src/**/*.rs").as_deref(), Some("rs"));
        assert_eq!(required_ext("Cargo.toml").as_deref(), Some("toml"));
        assert_eq!(required_ext("*test*.rs").as_deref(), Some("rs"));
        assert_eq!(required_ext("**/*.DLL").as_deref(), Some("dll"));
    }

    #[test]
    fn required_ext_none_when_unpinnable() {
        assert_eq!(required_ext("*.{rs,toml}"), None); // alternation in suffix
        assert_eq!(required_ext("Makefile"), None); // no extension
        assert_eq!(required_ext("foo*"), None); // no dot
        assert_eq!(required_ext("**/.gitignore"), None); // dotfile, dot at index 0
        assert_eq!(required_ext("**/x/**"), None); // recursive leaf
    }

    #[test]
    fn drive_of_parses_letter() {
        assert_eq!(drive_of("D:\\foo\\bar"), Some('D'));
        assert_eq!(drive_of("c:\\x"), Some('C'));
        assert_eq!(drive_of("/unix/path"), None);
        assert_eq!(drive_of(""), None);
    }

    #[test]
    fn normalize_prefix_lowercases_and_trims() {
        assert_eq!(normalize_prefix("D:\\Foo\\"), "d:\\foo");
        assert_eq!(normalize_prefix("D:/Foo/"), "d:/foo");
        assert_eq!(normalize_prefix(""), "");
    }

    #[test]
    fn leaf_matcher_filters_basename() {
        let m = leaf_matcher("**/*.rs", false).unwrap().unwrap();
        assert!(m.is_match("main.rs"));
        assert!(!m.is_match("main.toml"));
        assert!(leaf_matcher("**/x/**", false).unwrap().is_none());
    }

    #[test]
    fn case_sensitive_matcher() {
        assert!(build_matcher("**/*.RS", false)
            .unwrap()
            .is_match("a/main.rs"));
        assert!(!build_matcher("**/*.RS", true)
            .unwrap()
            .is_match("a/main.rs"));
    }

    #[test]
    fn parsers_default_safely() {
        assert!(matches!(Kind::parse("dir"), Kind::Dir));
        assert!(matches!(Kind::parse("xyz"), Kind::Any));
        assert!(matches!(SortKey::parse("size"), SortKey::Size));
        assert!(matches!(SortKey::parse(""), SortKey::None));
        assert!(matches!(Order::parse("desc"), Order::Desc));
        assert!(matches!(Order::parse("whatever"), Order::Asc));
    }

    #[test]
    fn sort_hits_orders_by_size_desc() {
        let mut hits = vec![
            Hit {
                path: "a".into(),
                is_dir: false,
                size: Some(10),
                mtime: None,
            },
            Hit {
                path: "b".into(),
                is_dir: false,
                size: Some(99),
                mtime: None,
            },
            Hit {
                path: "c".into(),
                is_dir: false,
                size: Some(50),
                mtime: None,
            },
        ];
        sort_hits(&mut hits, SortKey::Size, Order::Desc);
        assert_eq!(
            hits.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
            ["b", "c", "a"]
        );
    }
}
