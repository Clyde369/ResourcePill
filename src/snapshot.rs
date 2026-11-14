//! `ResourcePill.exe --snapshots <folder>`: draws every view of the pill with sample data, off
//! screen, and saves each one as a PNG (on a dark backdrop) — to check the whole UI at a glance.
//! Nothing appears on screen, nothing is saved to the settings and no sounds play.

use std::path::{Path, PathBuf};

use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};

use crate::*;

/// Sample numbers that look like a busy afternoon.
fn sample_snap() -> stats::Snap {
    let mut s = stats::Snap::default();
    s.ready = true;
    s.frac = [0.64, 0.61, 0.22, 0.03, 0.4];
    s.text = ["64%".into(), "61%".into(), "22%".into(), "3%".into(), "158 Kbps".into()];
    s.detail = ["68 °C · 4.25 GHz · up 9h 10m".into(), "24.5 / 39.9 GB".into(), "48 °C · VRAM 1.6 GB".into(), "R 301 KB/s · W 6.6 MB/s".into(), "↑ 93.5 Kbps".into()];
    s.down = "158 Kbps".into();
    s.up = "93.5 Kbps".into();
    s.disk_label = "DISK C:".into();
    s.temp = [Some(68.0), None, Some(48.0), Some(43.0), None];
    s.top_for = 0;
    s.top = vec![
        stats::Top { name: "chrome".into(), value: "11.9%".into(), frac: 0.4 },
        stats::Top { name: "Discord".into(), value: "6.7%".into(), frac: 0.22 },
        stats::Top { name: "Spotify".into(), value: "2.1%".into(), frac: 0.07 },
    ];
    s
}

fn sample_now() -> media::Now {
    media::Now { present: true, title: "Lovers And Friends (Extended Mix)".into(), artist: "Usher, Lil Jon, Ludacris".into(), app: "Spotify".into(), playing: true, shuffle: Some(true), len: 240.0, pos: 83.0, volume: Some((0.85, true)), ..Default::default() }
}

/// Back to a plain pill: no panels, strips, modes or tints.
fn reset(a: &mut App) {
    a.cfg.card = false;
    a.cfg.vertical = false;
    a.cfg.size = 1;
    a.cfg.theme = 0;
    a.cfg.clock = true;
    a.cfg.pos = Some((300, 200));
    a.settings = None;
    a.music_open = false;
    a.mixer = None;
    a.sel = NONE;
    a.panel_sel = NONE;
    a.open = 0.0;
    a.hl_a = 0.0;
    a.job = None;
    a.rec.state = recorder::State::Idle;
    a.rec.lines.clear();
    a.tucked = false;
    a.tuck = 0.0;
    a.explain_mode = false;
    a.tint_a = 0.0;
    a.tint_a_to = 0.0;
    a.duck.ducking.store(false, Ordering::Relaxed);
    a.duck.factor.store(1f32.to_bits(), Ordering::Relaxed);
    a.now = sample_now();
    a.update_scale();
}

/// Open a panel fully (no animation in a snapshot).
fn open(a: &mut App) {
    a.open = 1.0;
    a.nav_t = 1.0;
}

fn select(a: &mut App, i: usize) {
    a.sel = i;
    a.panel_sel = i;
    a.snap.top_for = i;
    if let Some(c) = a.layout().cells.iter().find(|c| c.0 == i) {
        a.hl = [c.1.x, c.1.y, c.1.w, c.1.h];
    }
    a.hl_a = 1.0;
    open(a);
}

/// Index of a top-level settings group by its label.
fn group(a: &App, label: &str) -> Option<usize> {
    a.settings_cache.iter().position(|i| matches!(i, settings::MI::Sub { label: l, .. } if l == label))
}

/// Draw the current state and save it as `<dir>/<name>.png`.
unsafe fn shot(a: &mut App, dir: &Path, name: &str) {
    unsafe {
        a.render();
        let (w, h) = (a.pw, a.ph);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        GetDIBits(a.memdc, a.bmp, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &bi as *const _ as *mut _, DIB_RGB_COLORS);
        // onto a dark desktop-like backdrop (premultiplied BGRA "over" an opaque colour)
        let bg = [0x2a, 0x22, 0x1e]; // B, G, R
        for p in px.chunks_exact_mut(4) {
            let k = 1.0 - p[3] as f32 / 255.0;
            for c in 0..3 {
                p[c] = (p[c] as f32 + bg[c] as f32 * k).min(255.0) as u8;
            }
            p[3] = 255;
        }
        if let Err(e) = save_png(&dir.join(format!("{name}.png")), w as u32, h as u32, &px) {
            eprintln!("{name}: {e}");
        }
    }
}

fn save_png(path: &Path, w: u32, h: u32, bgra: &[u8]) -> windows::core::Result<()> {
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = wic.CreateStream()?;
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), 0x4000_0000)?; // GENERIC_WRITE
        let enc = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        enc.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(windows::core::Error::empty)?;
        frame.Initialize(None)?;
        frame.SetSize(w, h)?;
        let mut fmt = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut fmt)?;
        frame.WritePixels(h, w * 4, bgra)?;
        frame.Commit()?;
        enc.Commit()
    }
}

/// Every view worth checking.
pub unsafe fn run(a: &mut App, dir: PathBuf) {
    unsafe {
        let _ = std::fs::create_dir_all(&dir);
        a.snap = sample_snap();
        a.disp = a.snap.frac; // rings at their values (no animation in a snapshot)
        a.vram_total = 8.0 * 1024.0 * 1024.0 * 1024.0;
        for (i, h) in a.hist.iter_mut().enumerate() {
            // a minute of plausible history for the card graphs
            *h = (0..HIST).map(|k| (0.35 + 0.25 * ((k as f32 * 0.37 + i as f32).sin()) + 0.1 * ((k as f32 * 1.7).sin())).clamp(0.02, 0.98) * if i == NET { 2.0e6 } else { 1.0 }).collect();
        }
        a.time_text = "18:46".into();
        a.date_text = "Fri 13 Nov".into();
        a.sheen = -1.0;
        a.fade = 1.0;
        a.want_visible = true;
        reset(a);
        let explorer = std::env::var("WINDIR").map(|w| format!("{w}\\explorer.exe")).unwrap_or_default();

        // every theme, plain pill
        for t in 0..THEMES.len() {
            reset(a);
            a.cfg.theme = t;
            shot(a, &dir, &format!("01-theme-{t}-{}", THEMES[t].name.to_lowercase()));
        }
        // sizes
        for s in 0..SIZES.len() {
            reset(a);
            a.cfg.size = s;
            a.update_scale();
            shot(a, &dir, &format!("02-size-{}", SIZES[s].0.to_lowercase()));
        }
        // song-cover colours (purple and black)
        for t in [0, 3, 5] {
            reset(a);
            a.cfg.theme = t;
            a.tint = [140.0, 40.0, 220.0, 12.0, 6.0, 20.0];
            a.tint_a = 1.0;
            shot(a, &dir, &format!("03-cover-colours-theme-{t}"));
        }
        // a metric panel
        reset(a);
        select(a, stats::CPU);
        shot(a, &dir, "04-panel-cpu");
        reset(a);
        a.cfg.pos = Some((300, 900)); // near the bottom: the panel opens upwards
        select(a, stats::NET);
        shot(a, &dir, "04-panel-net-upwards");
        // settings: the top level and each group
        reset(a);
        a.settings_cache = a.settings_items();
        a.settings = Some(Vec::new());
        open(a);
        shot(a, &dir, "05-settings");
        for label in ["Appearance", "Transcription", "AI & API keys", "Sounds", "Model library", "System"] {
            reset(a);
            a.settings_cache = a.settings_items();
            if let Some(g) = group(a, label) {
                a.settings = Some(vec![g]);
                open(a);
                shot(a, &dir, &format!("05-settings-{}", label.to_lowercase().replace([' ', '&'], "-")));
            }
        }
        // music: the player and the mixer
        reset(a);
        a.music_open = true;
        open(a);
        shot(a, &dir, "06-music-player");
        reset(a);
        a.mixer = Some(mixer::Mixer::load());
        open(a);
        shot(a, &dir, "06-mixer");
        // auto-lower at work: badge, note, mixer showing your level
        reset(a);
        a.duck.ducking.store(true, Ordering::Relaxed);
        a.duck.mode.store(1, Ordering::Relaxed);
        a.duck.factor.store(0.3f32.to_bits(), Ordering::Relaxed);
        *a.duck.own.lock().unwrap() = vec![("Spotify".into(), 0.85)];
        *a.duck.talker.lock().unwrap() = Some(("Discord".into(), explorer.clone()));
        if let Some(b) = a.app_icon(&explorer) {
            a.icons.insert(explorer.clone(), b);
        }
        shot(a, &dir, "07-auto-lower-pill");
        a.mixer = Some(mixer::Mixer::load());
        open(a);
        shot(a, &dir, "07-auto-lower-mixer");
        // strips: live transcript, an answer, a download
        reset(a);
        a.rec.state = recorder::State::Recording;
        a.rec.status = "Listening · 2 voices".into();
        a.rec.speakers = vec!["You".into(), "Speaker 2".into()];
        for (who, text) in [("You", "So the plan is to ship the mixer first."), ("Speaker 2", "Sounds good, and the auto-lower after that?"), ("You", "Yes, with the AI switch on by default.")] {
            a.rec.lines.push_back(recorder::Line { who: who.into(), text: text.into() });
        }
        shot(a, &dir, "08-strip-recording");
        reset(a);
        a.job = Some(recorder::Job::note(recorder::JobKind::Explain, "Explain · Gemini", "Photosynthesis is how plants turn light, water and carbon dioxide into sugar and oxygen. It happens in the chloroplasts, and it is the reason there is oxygen in the air we breathe."));
        shot(a, &dir, "08-strip-answer");
        // other layouts
        reset(a);
        a.cfg.vertical = true;
        a.cfg.pos = Some((20, 200));
        shot(a, &dir, "09-vertical");
        select(a, stats::RAM);
        shot(a, &dir, "09-vertical-panel");
        reset(a);
        a.cfg.vertical = true;
        a.cfg.pos = Some((20, 200));
        a.music_open = true;
        open(a);
        shot(a, &dir, "09-vertical-player");
        reset(a);
        a.cfg.card = true;
        select(a, stats::RAM);
        shot(a, &dir, "10-card");
        a.duck.ducking.store(true, Ordering::Relaxed);
        shot(a, &dir, "10-card-auto-lower");
        // settings from the card near the top of the screen (they must stay on screen)
        for (name, pos) in [("10-card-settings", (300, 10)), ("10-card-settings-right-edge", (1500, 10))] {
            reset(a);
            a.cfg.card = true;
            a.cfg.pos = Some(pos);
            select(a, stats::CPU);
            a.settings_cache = a.settings_items();
            a.settings = Some(Vec::new());
            shot(a, &dir, name);
        }
        reset(a);
        a.cfg.pos = Some((300, 980)); // pill near the bottom: settings open upwards, within the screen
        a.settings_cache = a.settings_items();
        a.settings = Some(Vec::new());
        open(a);
        shot(a, &dir, "05-settings-from-bottom");
        // tucked away, and explain mode
        reset(a);
        a.tucked = true;
        a.tuck = 1.0;
        shot(a, &dir, "11-tucked");
        reset(a);
        a.explain_mode = true;
        shot(a, &dir, "11-explain-mode");
        // the keyboard: Bluetooth, cable, not connected
        for (name, link) in [("13-keyboard-bluetooth", Some(keyboard::Link::Bluetooth)), ("13-keyboard-cable", Some(keyboard::Link::Cable)), ("13-keyboard-off", None)] {
            reset(a);
            a.kbd_state = Some(keyboard::Status { connected: link, last: keyboard::Link::Bluetooth, name: "Keychron K8".into() });
            shot(a, &dir, name);
        }
        a.job = Some(recorder::Job::note(recorder::JobKind::Explain, "Keyboard", "Keychron K8 · connected by Bluetooth"));
        shot(a, &dir, "13-keyboard-note");
        a.kbd_state = None;
        // first launch on a new PC: nothing installed yet (an empty models folder), voice detection
        // done, the speech model 42% through, the guide open at each step
        let real_models = std::env::var_os("LOCALAPPDATA");
        let fresh = std::env::temp_dir().join("rp-fresh-pc");
        let _ = std::fs::remove_dir_all(&fresh);
        let _ = std::fs::create_dir_all(fresh.join("ResourcePill").join("models").join("vad"));
        let _ = std::fs::write(fresh.join("ResourcePill").join("models").join("vad").join("silero_vad.onnx"), b"");
        std::env::set_var("LOCALAPPDATA", &fresh);
        let first_launch = |a: &mut App| {
            reset(a);
            a.cfg.asr = 0;
            a.cfg.llm = 0;
            a.cfg.tx_cloud = false;
            a.setup_current = Some("parakeet-v3");
            a.setup_queue = vec!["voiceprint", "segmentation"];
            a.setup_total = 4;
            a.job = Some(recorder::Job::preview_download("Getting ready · 2 of 4 · Parakeet TDT 0.6B v3 · 465 MB", 195_000_000, 465_000_000));
        };
        // a generic folder in the shot, not this PC's user name
        recorder::set_recordings_dir(Some(std::path::PathBuf::from(r"C:\Users\You\Documents\ResourcePill\Recordings")));
        for step in 0..5u8 {
            first_launch(a);
            a.guide = Some(step);
            open(a);
            shot(a, &dir, &format!("14-first-launch-guide-{step}"));
        }
        recorder::set_recordings_dir((!a.cfg.rec_dir.is_empty()).then(|| std::path::PathBuf::from(&a.cfg.rec_dir)));
        first_launch(a);
        a.guide = None;
        shot(a, &dir, "14-first-launch-downloading");
        first_launch(a);
        a.record_when_ready = true;
        shot(a, &dir, "14-first-launch-record-pressed");
        match real_models {
            Some(v) => std::env::set_var("LOCALAPPDATA", v),
            None => std::env::remove_var("LOCALAPPDATA"),
        }
        reset(a);
        a.setup_current = None;
        a.setup_queue.clear();
        a.job = Some(recorder::Job::note(recorder::JobKind::Download, "Ready", "Transcription is ready · press ● or Ctrl+Alt+R to record"));
        shot(a, &dir, "14-first-launch-ready");
        a.job = None;
        // no music playing (the cell goes away)
        reset(a);
        a.now = media::Now::default();
        shot(a, &dir, "12-no-music");
    }
}
