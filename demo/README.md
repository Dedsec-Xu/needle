# Demo

A head-to-head showing needle answering a **whole-drive** file search from its
warm NTFS MFT index in well under a millisecond, versus traversal-based tools
(`fd`, `fff`, ripgrep, or a PowerShell-native walk) that must crawl the tree.

## 1. Start the daemon (once, elevated)

```powershell
./start-daemon.ps1
```

The daemon reads the MFT once and keeps the index warm (USN-incremental).

## 2. Run the comparison

```powershell
./demo/compare.ps1                 # find *.rs across all of D:
./demo/compare.ps1 -Ext dll -Drive C
```

It auto-detects whichever competitors are installed (`fd`, `fff`, `rg`) and
always includes a PowerShell recursive walk as a baseline, then prints a timing
table with speedup factors versus needle.

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
