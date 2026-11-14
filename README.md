# Peter the Pill (ResourcePill)

A tiny, always-on-top pill for Windows, written in Rust (Win32 + Direct2D, no web runtime).
It shows CPU, RAM, GPU, disk and network, controls your music and volume, and records and transcribes meetings on your own PC.

![Glass pill with clock](docs/glass-pill.png)

**[Download ResourcePill.exe](https://github.com/Clyde369/ResourcePill/releases/latest)**: one file, nothing to install.

## How it works

### 1. Install

Download the exe and double-click it. Windows SmartScreen may warn about an unknown app: click **More info → Run anyway**.
Everything it needs is inside that one file and unpacks itself to `%LOCALAPPDATA%\ResourcePill` on first run.

### 2. First launch: it gets transcription ready by itself

The required speech models (about 570 MB, once) start downloading straight away. The strip under the pill shows progress.

![First launch downloading](docs/screens/first-launch.png)

### 3. The setup guide

A short guide opens with it. You can reopen it any time from **⚙ → Transcription → Setup guide**.

| Welcome | Speech to text |
|---|---|
| ![Guide: welcome](docs/screens/guide-1-welcome.png) | ![Guide: speech to text](docs/screens/guide-2-speech.png) |
| **Meeting summaries** | **Your recordings** |
| ![Guide: meeting summaries](docs/screens/guide-3-summaries.png) | ![Guide: recordings folder](docs/screens/guide-4-recordings.png) |
| **Getting ready** | |
| ![Guide: download checklist](docs/screens/guide-5-download.png) | |

- **Speech to text**: local (free, private) or Gemini with your own API key.
- **Your recordings**: pick where recordings and transcripts are saved (**Change folder…**). Change it later in **⚙ → Transcription → Recordings folder…**

### 4. Record with one press

Press **●** on the pill (or `Ctrl+Alt+R`). If the models are still downloading, it waits and starts recording by itself when they're ready.

| Pressed while downloading | Ready |
|---|---|
| ![Record pressed while downloading](docs/screens/record-pressed.png) | ![Ready to record](docs/screens/ready.png) |

While recording, the live transcript appears under the pill with each speaker labelled:

![Live transcript](docs/screens/recording.png)

### 5. Find your transcripts

Every recording, transcript and summary is in one window:

- **⚙ (gear on the pill) → Recordings**, or
- after a meeting, click the strip under the pill.

The files are also in your recordings folder (default `Documents\ResourcePill\Recordings`).

## Features

| Music player (any app: Spotify, YouTube, …) | Volume mixer |
|---|---|
| ![Music player](docs/screens/music-player.png) | ![Volume mixer](docs/screens/mixer.png) |

- **Music**: play/pause, previous/next and progress right on the pill. Click the song for the full player, with volume. No sign-in, it uses Windows media controls.
- **Volume mixer**: every app's volume in one place. Click the speaker icon on the pill.
- **Lower music when people talk**: when someone speaks on Discord, a call or a video, your music dips smoothly and comes back after. Optional AI voice detection runs on your PC. A badge on the pill shows which app it's lowering for.

  ![Auto-lower badge](docs/screens/auto-lower.png)

- **Cover colours**: the pill picks up a moving gradient from the song cover.

  ![Cover colours](docs/screens/cover-colours.png)

- **Keyboard status**: shows whether your keyboard is connected by Bluetooth, by cable, or disconnected.

  ![Keyboard over Bluetooth](docs/screens/keyboard.png)

- **Explain mode**: highlight text anywhere and ask Peter to explain or search it (local or Gemini).
- **Edge tuck**: drag the pill against a screen edge to tuck it away, tap the edge to bring it back.
- **Accurate stats**: Task Manager's own data sources. Top 3 apps for CPU, RAM, GPU and disk; temperatures for GPU and NVMe (CPU and SATA in admin mode).
- **Views**: pill, vertical capsule when docked to a side, or a card with 60-second graphs (double-click).
- **Themes**: Graphite, Midnight, OLED, Glass, Frost, Light, Sunset, Mono.
- **Performance**: High / Medium / Low and 15–60 fps. Low uses almost nothing and turns off animations.

| Card | Docked to a side | Settings |
|---|---|---|
| ![Card view](docs/screens/card.png) | ![Vertical capsule](docs/screens/vertical.png) | ![Settings](docs/screens/settings.png) |

## Use

- Drag to move: it snaps to edges, corners and centre, and never leaves the screen.
- Double-click switches between the pill and the card.
- `Ctrl+Alt+M` or the tray icon hides and shows it. Click the gear or right-click for settings.
- **Admin mode** (settings) adds CPU and SATA temperatures. It asks for administrator rights once, then starts at login through a scheduled task.
- `ResourcePill.exe --quit` closes a running copy.

## Build

Requires Rust (MSVC toolchain) on Windows 10/11.

```
cargo build --release
```

`target\release\resource-pill.exe --make-bundle "Peter the Pill.exe"` packs the app and the transcription helper into one exe.
`resource-pill.exe --snapshots <folder>` renders every view to PNGs (that's how the screenshots above are made).

CPU temperature needs the [AMD Ryzen Master SDK](https://www.amd.com/en/products/software/ryzen-master.html) installed (`C:\Program Files\AMD\RyzenMasterSDK`) when building and running. Without it the app works normally, just without CPU temperature. No AMD files are included in this repository.

## License

[MIT](LICENSE)
