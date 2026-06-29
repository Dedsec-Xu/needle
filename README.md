# needle

[![CI](https://github.com/Dedsec-Xu/needle/actions/workflows/ci.yml/badge.svg)](https://github.com/Dedsec-Xu/needle/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Dedsec-Xu/needle?sort=semver)](https://github.com/Dedsec-Xu/needle/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platform: Windows/NTFS](https://img.shields.io/badge/platform-Windows%20%2F%20NTFS-0078D6)

**Find any file on your machine, instantly — a whole-machine file-search MCP for AI agents. Windows / NTFS.**

![needle demo](demo/needle-demo.gif)

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

## Example agent workflows

Things an agent can do in milliseconds with `fast_glob` that are slow or
impossible with a project-scoped, traversal-based Glob:

- **"Find every MP3 scattered across my drives and move them into `D:\Music`."**
  `fast_glob("**/*.mp3")` returns every match on every NTFS volume instantly; the
  agent then organizes them. A built-in Glob can't even see outside the project.

- **"Open that tax PDF from last year — it has `2024` in the name."**
  `fast_glob("**/*2024*.pdf")` across the whole machine finds it in <1 ms, no
  "which folder did I save it in?" hunting.

- **"Security sweep: list every private key and `.env` on this box."**
  `fast_glob("**/*.pem")`, `fast_glob("**/*.key")`, `fast_glob("**/.env")` —
  whole-machine, in milliseconds, instead of crawling every directory.

- **"Inventory every project on this machine."**
  `fast_glob("**/Cargo.toml")`, `fast_glob("**/package.json")`,
  `fast_glob("**/*.sln")` to map all repos/solutions across drives at once.

- **"I have my résumé saved in five places — find them all."**
  `fast_glob("**/*resume*")` / `fast_glob("**/*résumé*")` surfaces every copy
  machine-wide so the agent can dedupe.

- **"Reclaim space: where are all the `node_modules` / `target` build dirs?"**
  `fast_glob("**/node_modules")`, `fast_glob("**/target")` finds build junk
  across every repo on every drive in one shot.

Because needle returns absolute paths from a warm whole-machine index, the agent
spends its turn *acting* on the files rather than walking the tree to find them.

## Architecture

Reading the MFT requires administrator rights, but an MCP client launches its
servers non-elevated. So needle splits into two roles across that boundary:

```
needle serve   (ADMIN, long-running daemon)
  ├─ builds the MFT index per drive on first query
  ├─ one watcher thread per drive BLOCKS on FSCTL_READ_USN_JOURNAL and applies
  │  each change the instant NTFS records it (Everything-style; 0 idle CPU)
  └─ answers queries over loopback TCP 127.0.0.1:48923

needle mcp     (non-elevated, launched by the agent / MCP client)
  └─ exposes the `fast_glob` MCP tool, forwards queries to the daemon
```

The daemon exists purely to cross the privilege boundary — it is the only part
that needs admin. The MCP frontend is a thin, stateless forwarder. Installed as a
Windows service (`needle service install`), the daemon runs as LocalSystem and
auto-starts at boot, so the privilege boundary is crossed once at install time
and never again — the agent's non-elevated MCP process just connects to it.

## Install

**Prebuilt binary** — grab the latest `needle-vX.Y.Z-x86_64-pc-windows-msvc.zip`
from [Releases](https://github.com/Dedsec-Xu/needle/releases), unzip, done. Each
release ships a `.sha256` to verify the download.

**Scoop**

```powershell
scoop install https://raw.githubusercontent.com/Dedsec-Xu/needle/main/packaging/scoop/needle.json
```

**Cargo** (builds from source; needs the MSVC toolchain)

```sh
cargo install --git https://github.com/Dedsec-Xu/needle
```

After installing, register the daemon as a service once — it prompts for
elevation (UAC) automatically, so any shell works:

```powershell
needle service install
```

## Build (from source)

```sh
cargo build --release
```

## Run

1. **Install the daemon as a service (once).** It runs as LocalSystem and
   auto-starts at every boot — no manual launch, no recurring UAC prompt:

   ```powershell
   needle service install      # prompts for elevation (UAC) once
   ```

   Remove it later with `needle service uninstall`.

   <details><summary>Prefer not to install a service?</summary>

   Run the daemon manually instead (must stay open, re-run after each reboot)
   from an elevated shell:

   ```powershell
   needle serve
   ```
   </details>

2. **Register the MCP server** with your agent. needle speaks plain
   [MCP](https://modelcontextprotocol.io) over stdio, so any MCP-capable agent
   can use it — the command is always `needle.exe mcp`. A project-scoped
   `.mcp.json` is included; pick your agent below.

   <details open><summary><b>Claude Code</b></summary>

   ```sh
   claude mcp add needle -- "D:\\path\\to\\needle\\target\\release\\needle.exe" mcp
   ```

   Or drop it into `.mcp.json` (project) / `~/.claude.json` (global):

   ```json
   {
     "mcpServers": {
       "needle": {
         "command": "D:\\path\\to\\needle\\target\\release\\needle.exe",
         "args": ["mcp"]
       }
     }
   }
   ```
   </details>

   <details><summary><b>Codex</b> (OpenAI Codex CLI)</summary>

   Add to `~/.codex/config.toml`:

   ```toml
   [mcp_servers.needle]
   command = "D:\\path\\to\\needle\\target\\release\\needle.exe"
   args = ["mcp"]
   ```
   </details>

   <details><summary><b>Hermes</b> (NousResearch Hermes Agent)</summary>

   Add to the `mcp_servers` section of Hermes' `config.yaml`:

   ```yaml
   mcp_servers:
     needle:
       command: "D:\\path\\to\\needle\\target\\release\\needle.exe"
       args: ["mcp"]
   ```
   </details>

   <details><summary><b>OpenClaw</b></summary>

   Add to `~/.openclaw/openclaw.json` under `mcp.servers` (or use
   `openclaw mcp add`):

   ```json
   {
     "mcp": {
       "servers": {
         "needle": {
           "command": "D:\\path\\to\\needle\\target\\release\\needle.exe",
           "args": ["mcp"]
         }
       }
     }
   }
   ```
   </details>

   <details><summary><b>Any other MCP client</b></summary>

   Point your client at the stdio command below — that's the whole contract:

   ```
   command: D:\path\to\needle\target\release\needle.exe
   args:    ["mcp"]
   ```
   </details>

3. The `fast_glob` tool becomes available. To make the agent prefer it over the
   built-in Glob, add to your `CLAUDE.md`:

   > To find files by name or path anywhere on this machine, use the `fast_glob`
   > MCP tool instead of the built-in Glob — it is far faster and sees every
   > drive. Use Grep for file-content search.

## CLI

Once the service (or daemon) is running, query it with `needle find` — it just
forwards to the daemon over loopback, so it is **fast and needs no admin**:

```sh
# query the running daemon (no admin; the service answers from its warm index)
needle find "**/*.rs" --root "D:\path\to\project" --max-results 50

# query the whole machine (no root scope)
needle find "**/appsettings.json"
```

If the daemon isn't running, `needle query` does a one-shot in-process build
instead — handy for ad-hoc use, but it **must itself be elevated** and pays the
full index-build cost each time:

```sh
# one-shot query (builds the index in-process; requires admin)
needle query "**/*.rs" --root "D:\path\to\project" --max-results 50

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
in-memory index, which a per-drive watcher thread keeps live by blocking on the
USN Journal — any file created, deleted, renamed, or moved shows up in the very
next query, with no rescan and no polling delay.

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

### vs what your agent searches with today

Coding agents find files with their **built-in Glob/Grep tools** — ripgrep-style
**directory traversal**. Two things make that a poor fit for an autonomous agent:

1. **It walks the tree on every call.** No persistent index; each search re-crawls
   the filesystem and the cost grows with the tree.
2. **It can't see past the working directory.** The agent is blind to anything
   outside the project root — other repos, SDKs in `Program Files`, configs under
   `C:\Users`. For those, traversal isn't slow, it simply returns nothing.

needle replaces both: a warm MFT index over the **whole machine**, answered from
memory. Same whole-drive search — every `*.rs` on a 2.27M-file drive, 601 matches,
identical results for every tool — measured end-to-end with `demo/compare.ps1`:

| tool                                   | how it works     | time       | vs needle    |
|----------------------------------------|------------------|-----------:|--------------|
| **needle**                             | NTFS MFT index   | **8.4 ms** | baseline     |
| **ripgrep** (what Glob/Grep run on)    | directory walk   | 8811 ms    | **1049x slower** |
| PowerShell `Get-ChildItem`             | directory walk   | 4733 ms    | 563x slower  |
| cmd `dir /s /b`                        | directory walk   | 4894 ms    | 583x slower  |
| fd (parallel)                          | directory walk   | 667 ms     | 79x slower   |
| es.exe (Everything)                    | NTFS MFT index   | 25.9 ms    | 3x slower    |

The tool your agent already uses — **ripgrep-based traversal — is ~1000x slower**
here, and that's only for files *inside* the project; whole-machine lookups it
can't do at all. needle even edges out Everything's own CLI (both read the MFT,
but needle answers from a warm in-process index). Times are end-to-end wall-clock
(incl. ~5–7 ms process startup); needle's pure in-index query is sub-millisecond.
Reproduce with `demo/compare.ps1`.

## `fast_glob` tool parameters

| param               | default | meaning                                              |
|---------------------|---------|------------------------------------------------------|
| `pattern`           | —       | glob matched vs the path relative to `root`          |
| `root`              | `""`    | scope to this directory; empty = **whole machine** (all NTFS volumes) |
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
