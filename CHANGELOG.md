# Changelog

All notable changes to needle are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and versions follow SemVer.

## [0.1.3]

### Added
- `fast_glob` query controls for agents: `kind` (`file`/`dir`/`any`),
  `case_sensitive`, `sort` (`name`/`mtime`/`size`), and `order` (`asc`/`desc`).
- Metadata sorting via a hybrid model: the index finds candidates, then needle
  lazily `stat`s them to fill `size`/`mtime`, sorts locally, and returns top-k.
  Over 5000 matched candidates the result is flagged `sort_approximate`.
- `--kind`, `--case-sensitive`, `--sort`, `--order` flags on `needle find` and
  `needle query`.

### Notes
- `sort=none` (default) keeps the sub-millisecond streaming fast path.
- `size`/`mtime` are intentionally **not** stored in the index — needle stays a
  search primitive, so no raw-MFT metadata parser is required.

## [0.1.2]

### Added
- `needle service install` / `uninstall` now self-elevate via UAC.
- Demo GIF in the README.

### Removed
- `start-daemon.ps1`, `install-service.ps1`, `uninstall-service.ps1` wrapper
  scripts (replaced by the self-elevating `service` subcommand).

## [0.1.1]

### Added
- Real multi-drive: empty-root queries fan out across every NTFS volume.
- USN self-heal: a deleted/recreated/wrapped journal triggers an index rebuild.
- Per-drive USN watcher threads that apply changes the instant NTFS records them.
- CI (fmt + clippy + build + test) and a tag-triggered release workflow that
  publishes a Windows binary; Scoop manifest and `cargo install` support.

## [0.1.0]

### Added
- Initial release: NTFS MFT index + USN incremental updates, exposed as the
  `fast_glob` MCP tool. Extension inverted index for sub-millisecond
  extension-pinned globs. Split elevated daemon + non-elevated MCP frontend.

[0.1.3]: https://github.com/Dedsec-Xu/needle/releases/tag/v0.1.3
[0.1.2]: https://github.com/Dedsec-Xu/needle/releases/tag/v0.1.2
[0.1.1]: https://github.com/Dedsec-Xu/needle/releases/tag/v0.1.1
[0.1.0]: https://github.com/Dedsec-Xu/needle/releases/tag/v0.1.0
