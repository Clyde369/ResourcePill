# ResourcePill

A tiny, always-on-top resource monitor for Windows, written in Rust (Win32 + Direct2D, no web runtime).
A pill-shaped bar shows CPU, RAM, GPU, disk and network; click any of them for the top 3 apps using it.

- **Accurate**: Task Manager's own data sources (kernel process snapshot, PDH, GPU engine counters, physical adapters)
- **Temperatures**: GPU (same source as Task Manager), NVMe drives, and in admin mode the CPU (AMD Ryzen Master SDK) and SATA drives (SMART)
- **Views**: horizontal pill, vertical capsule when docked to a screen edge, or a larger card with 60-second graphs
- **Clock** with an hourly sheen animation
- **Themes**: Graphite, Midnight, OLED, Glass, Frost, Light, Sunset, Mono
- **Customisable** from the right-click menu: metrics, size, opacity, refresh rate, smoothing, animations, snap positions, click-through, start with Windows
- **Light**: about 0.1% CPU and ~15 MB RAM

## Use

- Drag to move (snaps to edges and centre, never leaves the screen). Double-click switches pill ↔ card.
- `Ctrl+Alt+M` or the tray icon hides/shows it. Right-click for settings.
- `ResourcePill.exe --quit` closes a running copy.

## Build

Requires Rust (MSVC toolchain) on Windows 10/11.

```
cargo build --release
```

CPU temperature needs the AMD Ryzen Master SDK installed (`C:\Program Files\AMD\RyzenMasterSDK`) when building and running; without it the app builds and runs without CPU temperature. No AMD files are included in this repository.
