# needle

**Find any file on your machine, instantly — a whole-machine file-search MCP for AI agents. Windows / NTFS.**

`needle` reads the NTFS **MFT** (Master File Table) directly and keeps a warm
in-memory index that is refreshed incrementally from the **USN Journal**. It
exposes a single `fast_glob` MCP tool so a coding agent (Claude Code, etc.) can
locate any file on the **entire machine** — every NTFS drive, millions of
files — in well under a millisecond, instead of walking the filesystem.

> Filenames and paths only — **not** file contents. Use Grep/ripgrep for content.

## Why needle exists

Most file-search tools optimize search **inside one project**: directory
traversal, fuzzy matching, ranking — tuned for a human in an editor who already
knows roughly where they are.

An **agent** has a different need: it often has to find things **across the whole
machine** with no prior knowledge of where they live — a config under
`C:\Users`, an SDK in `Program Files`, a sibling repo on another drive. Walking
the tree for that is slow and burns context. `needle` indexes **every NTFS
volume at once** by reading the MFT, so a whole-disk lookup is as cheap as an
in-memory hash scan — sub-millisecond, even across millions of files.

needle is the one you want when the agent's search space is "the computer," not
"the repo."

## Architecture

Reading the MFT requires administrator rights, but an MCP client launches its
servers non-elevated. So needle splits into two roles across that boundary:

```
needle serve   (ADMIN, long-running daemon)
  ├─ builds the MFT index per drive on first query
  ├─ FSCTL_READ_USN_JOURNAL every 2s -> incremental index updates
  └─ answers queries over loopback TCP 127.0.0.1:48923

needle mcp     (non-elevated, launched by the agent / MCP client)
  └─ exposes the `fast_glob` MCP tool, forwards queries to the daemon
```

The daemon exists purely to cross the privilege boundary — it is the only part
that needs admin. The MCP frontend is a thin, stateless forwarder. Installed as a
Windows service (`install-service.ps1`), the daemon runs as LocalSystem and
auto-starts at boot, so the privilege boundary is crossed once at install time
and never again — the agent's non-elevated MCP process just connects to it.

## Build

```sh
cargo build --release
```

## Run

1. **Install the daemon as a service (once).** It then runs as LocalSystem and
   auto-starts at every boot — no manual launch, no UAC prompt afterwards:

   ```powershell
   ./install-service.ps1       # self-elevates via UAC, registers + starts 'needled'
   ```

   Remove it later with `./uninstall-service.ps1`.

   <details><summary>Prefer not to install a service?</summary>

   Run the daemon manually instead (must stay open, re-run after each reboot):

   ```powershell
   ./start-daemon.ps1          # self-elevates via UAC
   # or, in an already-elevated shell:
   ./target/release/needle.exe serve
   ```
   </details>

2. **Register the MCP server.** A project-scoped `.mcp.json` is included; or add
   it globally to Claude Code:

   ```sh
   claude mcp add needle -- "D:\\path\\to\\needle\\target\\release\\needle.exe" mcp
   ```

3. The `fast_glob` tool becomes available. To make the agent prefer it over the
   built-in Glob, add to your `CLAUDE.md`:

   > To find files by name or path anywhere on this machine, use the `fast_glob`
   > MCP tool instead of the built-in Glob — it is far faster and sees every
   > drive. Use Grep for file-content search.

## CLI (ad-hoc; must itself be elevated)

```sh
# one-shot query (builds the index in-process)
needle query "**/*.rs" --root "D:\path\to\project" --max-results 50

# query the whole machine (no root scope)
needle query "**/appsettings.json"

# benchmark a full index build for a drive
needle index C
```

## Benchmark

Run the built-in benchmark (elevated) to time the index build and a set of
whole-volume queries on your own machine:

```powershell
./bench.ps1            # benchmark drive C (self-elevates)
./bench.ps1 D          # benchmark drive D
```

It prints a ready-to-paste Markdown table: the one-time index-build time plus the
per-query latency for several whole-volume globs. Queries run against the warm
in-memory index and stay fresh via the USN Journal.

### Measured results

On a drive with **2,265,224 indexed entries** (full index built once in ~4.3 s):

| query (whole volume) | matches | time      |
|----------------------|---------|-----------|
| `**/*.rs`            | 601     | **0.85 ms** |
| `**/Cargo.toml`      | 25      | **0.20 ms** |
| `**/*.exe`           | 928     | **0.89 ms** |
| `**/*.dll`           | 13,932  | 12.8 ms   |
| `**/package.json`    | 10,104  | 26.6 ms   |

Extension-pinned globs hit the inverted index and are answered in well under a
millisecond. Latency for the last two scales only because the benchmark returns
*every* match (`max_results = 1,000,000`); the `fast_glob` MCP tool defaults to
`max_results = 200`, so even very broad globs return sub-millisecond in practice.

For comparison, before the inverted index every query was a full ~2.3M-entry scan
at ~360 ms — the extension index is a ~400x speedup on the common cases.

### Head-to-head vs other tools

The same **whole-drive** search — every `*.rs` on a 2.27M-file `D:` drive (601
matches, identical for all tools) — measured end-to-end with `demo/compare.ps1`:

| tool                          | type            | time       | vs needle   |
|-------------------------------|-----------------|-----------:|-------------|
| **needle**                    | NTFS MFT index  | **8.4 ms** | baseline    |
| es.exe (Everything)           | NTFS MFT index  | 25.9 ms    | 3x slower   |
| fd                            | parallel walk   | 667 ms     | 79x slower  |
| cmd `dir /s /b`               | directory walk  | 4894 ms    | 583x slower |
| PowerShell `Get-ChildItem`    | directory walk  | 4733 ms    | 563x slower |
| ripgrep `--files`             | directory walk  | 8811 ms    | 1049x slower|

needle outpaces not just traversal tools (by ~80–1000x) but also Everything's own
CLI — both read the MFT, but needle answers from a warm in-process index. Times
are end-to-end wall-clock (incl. ~5–7 ms process startup); needle's pure in-index
query is sub-millisecond (see above). Reproduce with `demo/compare.ps1`.

## `fast_glob` tool parameters

| param               | default | meaning                                              |
|---------------------|---------|------------------------------------------------------|
| `pattern`           | —       | glob matched vs the path relative to `root`          |
| `root`              | `""`    | scope to this directory; empty = **whole volume**    |
| `max_results`       | `200`   | cap on returned paths                                |
| `respect_gitignore` | `true`  | apply `root/.gitignore`, always skip `.git`          |

Patterns are case-insensitive and matched against forward-slash-normalized paths,
so `**/*.swift`, `src/**/*.rs`, and `**/Cargo.toml` all behave as expected.

## Limitations

- **Windows + NTFS only.** The MFT/USN approach has no equivalent on
  ext4/APFS/etc.; non-NTFS volumes are skipped.
- The daemon requires administrator privileges.
- Extension-pinned globs use the inverted index; patterns without a fixed
  extension (e.g. `Makefile`, `foo*`, `**/x/**`) fall back to a full scan with a
  cheap basename pre-filter. Parallelizing that fallback (rayon) is a future win.
- Query latency scales with the number of returned matches (each match
  reconstructs its full path), so keep `max_results` modest for broad globs.

## License

MIT
