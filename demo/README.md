# Demo

A head-to-head showing needle answering a **whole-drive** file search from its
warm NTFS MFT index in well under a millisecond, versus traversal-based tools
(`fd`, ripgrep, es.exe, or a PowerShell-native walk) that must crawl the tree.

## 1. Start the daemon (once)

```powershell
needle service install    # installs the LocalSystem service (prompts for UAC)
# or, ad-hoc from an elevated shell:
needle serve
```

The daemon reads the MFT once and keeps the index warm (USN-incremental).

## 2. Run the comparison

```powershell
./demo/compare.ps1                 # find *.rs across all of D:
./demo/compare.ps1 -Ext dll -Drive C
```

It auto-detects whichever competitors are installed and always includes the
cmd.exe and PowerShell recursive walks as baselines, then prints a timing table
with speedup factors versus needle:

- **es.exe** — Everything's CLI, also MFT-based (the apples-to-apples peer)
- **fd** — sharkdp/fd, parallel directory traversal
- **rg --files** — ripgrep file listing
- **cmd dir /s /b** and **Get-ChildItem -Recurse** — always-available baselines

> `fff` is intentionally not included: it is a library / MCP server, not a CLI,
> so it cannot be invoked from the shell for a fair head-to-head.

Example shape of the output:

```
needle vs traversal -- find *.rs across all of D:\

  needle (MFT index)        601 matches        0.85 ms
  fd (traversal)            601 matches       2900.00 ms
  PowerShell walk           601 matches       9100.00 ms

Speedup vs needle
  needle (MFT index)          0.85 ms   baseline
  fd (traversal)           2900.00 ms   3400x slower
  PowerShell walk          9100.00 ms   10700x slower
```

## 3. Record the GIF (optional)

Install [VHS](https://github.com/charmbracelet/vhs)
(`winget install charmbracelet.vhs`), keep the daemon running, then:

```powershell
vhs demo/needle-demo.tape      # writes demo/needle-demo.gif
```

Drop the resulting `needle-demo.gif` at the top of the root README to show the
speed difference at a glance.
