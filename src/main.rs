#![windows_subsystem = "windows"]
//! ResourcePill: an always-on-top, pill-shaped resource monitor.

mod stats;
mod temps;
mod admin;
mod recorder;
mod extras;
mod settings;
mod edge;
mod explainui;
mod media;
mod music;
mod mixer;
mod duck;
mod snapshot;
mod keyboard;
mod bundle;
mod setup;
mod update;
mod video;
mod lists;
#[allow(dead_code)] // the helper's HTTPS code; the lists use only post_json
#[path = "../scribe/src/net.rs"]
mod net;
#[allow(dead_code)] // download fields are used by the helper
mod catalog;

use std::cell::RefCell;
use std::f32::consts::PI;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use stats::{NET, NONE, Shared, Snap};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

// ---------- layout constants (DIPs) ----------
const M: f32 = 14.0; // transparent margin for the shadow
const PAD: f32 = 6.0;
const H_PILL: f32 = 42.0; // horizontal pill height
const H_SEG: [f32; 5] = [84.0, 80.0, 84.0, 100.0, 98.0]; // CPU/GPU/DISK carry a temperature in their label
const V_PILL: f32 = 68.0; // vertical pill width
const V_SEG: f32 = 60.0;
const CLOCK_W: f32 = 70.0; // clock cell in the horizontal pill
const CLOCK_V: f32 = 50.0; // clock cell height in the vertical pill
const REC_W: f32 = 38.0; // record button in the horizontal pill
const REC_V: f32 = 36.0; // record button height in the vertical pill
const STRIP_H: f32 = 100.0; // live transcript strip (3 lines)
const STRIP_LINES_BIG: usize = 10; // lines shown when the strip is clicked open
const MASCOT_W: f32 = 38.0; // the living helper icon in the horizontal pill
const GEAR_W: f32 = 30.0; // settings button at the start of the pill
const GEAR_V: f32 = 30.0;
const MASCOT_V: f32 = 38.0;
const GAP: f32 = 8.0;
const TIP_W: f32 = 176.0; // explain tooltip by the highlight
const TIP_H: f32 = 32.0;
const MUSIC_W: f32 = 216.0; // now-playing cell at the end of the pill
const MUSIC_V: f32 = 48.0;
const MIXER_W: f32 = 32.0; // volume mixer button
const MIXER_V: f32 = 32.0;
const MUSIC_PANEL_H: f32 = 196.0;
const PANEL_W: f32 = 330.0;
const WIDE_PANEL_W: f32 = 390.0; // settings and top-apps lists (they carry longer text)
const TOP_MIN_PANEL: f32 = 120.0; // a panel squeezed by the screen edge never gets shorter than this
const ROW_H: f32 = 28.0; // top-list row height; panels grow with their rows (min 3)
const CARD_W: f32 = 380.0;
const CARD_HEAD: f32 = 46.0;
const CARD_ROW: f32 = 62.0;
const HIST: usize = 60; // history samples per graph
const EDGE: f32 = 8.0; // distance kept from screen edges
const SNAP: f32 = 22.0; // magnetic snap distance

const LABELS: [&str; 5] = ["CPU", "RAM", "GPU", "DISK", "NET"];
const TITLES: [&str; 5] = ["Processor", "Memory", "Graphics", "Disk I/O", "Network adapters"];

const WM_APP_DATA: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const WM_APP_DPI: u32 = WM_APP + 3;
const WM_APP_SHOW: u32 = WM_APP + 4;
const WM_APP_QUIT: u32 = WM_APP + 5;
const WM_APP_SCRIBE: u32 = WM_APP + 6; // output from the transcriber helper
const WM_APP_JOB: u32 = WM_APP + 7; // output from an explain/download job
const WM_APP_MENU: u32 = WM_APP + 8; // open the settings menu (from the gear button)
const WM_APP_MEDIA: u32 = WM_APP + 12; // now-playing changed (song, state, cover)
const WM_APP_DUCK: u32 = WM_APP + 13; // auto-lower started or ended (the badge)
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_ANIM: usize = 1;
const TIMER_PULSE: usize = 2; // ~20 fps while something is working (pulse, mascot)
const TIMER_BLINK: usize = 3; // the mascot blinks every few seconds
const TIMER_TIP: usize = 4; // hides the explain tooltip after a while
const TIMER_EDGE: usize = 5; // edge auto-hide: watches the mouse (cheap, ~12 checks a second)
const TIMER_KBD: usize = 6; // a moment after a device change: check the keyboard
const KBD_W: f32 = 32.0; // keyboard connection cell
const KBD_V: f32 = 32.0;
const EDGE_LINE: i32 = 3; // px of the glowing line left on screen when tucked
const HOTKEY: i32 = 1;
const HOTKEY_REC: i32 = 2; // Ctrl+Alt+R
const HOTKEY_EXPLAIN: i32 = 3; // Ctrl+Alt+E
const HOTKEY_MIXER: i32 = 4; // Ctrl+Alt+V
const HOTKEY_VIDEO: i32 = 5; // Ctrl+Alt+Y
const HOTKEY_GHOST: i32 = 6; // Ctrl+Alt+G
const HOTKEY_LISTS: i32 = 7; // Ctrl+Alt+L
const CLASS: PCWSTR = w!("ResourcePill.Window");
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_NAME: PCWSTR = w!("ResourcePill");

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

// ---------- themes ----------

struct Theme {
    name: &'static str,
    bg: u32,
    bg_a: f32,
    fg: u32,
    edge_a: f32,
    shadow_a: f32,
    accent: [u32; 5],
    hot: u32,
    glass: bool, // glass styling: light catch, gradient edge, text shadows
}

const THEMES: [Theme; 8] = [
    Theme { name: "Graphite", bg: 0x16161B, bg_a: 0.94, fg: 0xFFFFFF, edge_a: 0.10, shadow_a: 0.075, accent: [0x5AC8FA, 0xBF5AF2, 0x30D158, 0xFF9F0A, 0x64D2FF], hot: 0xFF453A, glass: false },
    Theme { name: "Midnight", bg: 0x0B1222, bg_a: 0.95, fg: 0xE8F0FF, edge_a: 0.12, shadow_a: 0.085, accent: [0x4CC9F0, 0x8B7BFF, 0x2EE6A6, 0xFFB547, 0x6BD6FF], hot: 0xFF5C7A, glass: false },
    Theme { name: "OLED", bg: 0x000000, bg_a: 1.0, fg: 0xFFFFFF, edge_a: 0.16, shadow_a: 0.06, accent: [0x0A84FF, 0xBF5AF2, 0x32D74B, 0xFF9F0A, 0x64D2FF], hot: 0xFF453A, glass: false },
    Theme { name: "Glass", bg: 0x14161E, bg_a: 0.55, fg: 0xFFFFFF, edge_a: 0.30, shadow_a: 0.05, accent: [0x7FDBFF, 0xD7A6FF, 0x7CF29C, 0xFFC56B, 0x9BE7FF], hot: 0xFF6B6B, glass: true },
    Theme { name: "Frost (light glass)", bg: 0xF4F6FA, bg_a: 0.62, fg: 0x0F1116, edge_a: 0.45, shadow_a: 0.04, accent: [0x0071E3, 0x9F44D3, 0x1E9E40, 0xD97A00, 0x0A84FF], hot: 0xE5332A, glass: true },
    Theme { name: "Light", bg: 0xF7F7FA, bg_a: 0.96, fg: 0x141418, edge_a: 0.10, shadow_a: 0.05, accent: [0x007AFF, 0xAF52DE, 0x28B04A, 0xF08A00, 0x0A84FF], hot: 0xE5332A, glass: false },
    Theme { name: "Sunset", bg: 0x1E1220, bg_a: 0.95, fg: 0xFFF1F2, edge_a: 0.12, shadow_a: 0.08, accent: [0xFF6B9A, 0xFFB86B, 0x7CF29C, 0xFFD166, 0x6BCBFF], hot: 0xFF3D5A, glass: false },
    Theme { name: "Mono", bg: 0x18181A, bg_a: 0.94, fg: 0xFFFFFF, edge_a: 0.12, shadow_a: 0.075, accent: [0xE6E6E6; 5], hot: 0xFF453A, glass: false },
];

const SIZES: [(&str, f32); 3] = [("Compact", 0.85), ("Normal", 1.0), ("Large", 1.2)];
const OPACITY: [(&str, f32); 4] = [("100%", 1.0), ("85%", 0.85), ("70%", 0.7), ("55%", 0.55)];
const INTERVALS: [(&str, u32); 3] = [("0.5 s", 500), ("1 s", 1000), ("2 s", 2000)];
const SMOOTH: [&str; 3] = ["Off (raw, like Task Manager)", "Light", "Strong"];
const ANIMS: [&str; 2] = ["Full (fluid)", "Reduced (lowest CPU)"];
const FPS: [u32; 4] = [15, 25, 30, 60]; // frame rate choices for every animation
const PLACES: [&str; 8] = ["Top centre", "Top left", "Top right", "Bottom centre", "Bottom left", "Bottom right", "Left side", "Right side"];

// ---------- settings ----------

#[derive(Clone)]
struct Settings {
    theme: usize,
    shown: [bool; 5],
    size: usize,
    opacity: usize,
    interval: usize,
    side_layout: bool,
    vertical: bool,
    click_through: bool,
    card: bool,
    smooth: usize,
    admin_mode: bool,
    clock: bool,
    anim: usize,
    asr: usize, // index into recorder::ASR
    llm: usize, // index into recorder::LLM; == LLM.len() means no summary
    chime: bool,
    explain: usize, // 0 = local model, 1 = Claude, 2 = Gemini (cloud ones need a stored API key)
    final_pass: bool, // re-transcribe accurately after stopping
    half_chime: bool,
    cues: bool, // soft sounds for recording and highlights
    tx_cloud: bool, // transcribe + summarise with Gemini after stopping (needs a Google key)
    voice: bool, // read explanations aloud (Windows voice)
    fps: usize, // index into FPS
    duck: bool, // lower music while someone talks in a voice app
    duck_ai: bool, // …deciding "someone is talking" with the AI voice detector
    duck_roles: String, // your auto-lower roles per app, e.g. "chrome=l;spotify=i" (the rest: automatic)
    art_tint: bool, // tint the pill with the song cover's colours
    kbd_show: bool, // show how the keyboard is connected (Bluetooth, cable, disconnected)
    guide_done: bool, // the first-run setup guide has been seen
    rec_dir: String, // where recordings go (empty: the default, Documents/ResourcePill/Recordings)
    auto_hide: bool, // tuck into the screen edge when the mouse is away
    keep_audio: bool,
    gpu: bool, // run the accurate pass on the NVIDIA GPU (downloads the GPU speech engine)
    gpu_asked: bool, // asked once whether to use the GPU (no nagging after)
    video_url: String, // last YouTube link played under the pill
    video_w: u32, // its width in DIPs (0: as wide as the pill)
    video_alpha: usize, // index into video::ALPHAS
    video_chat: usize, // live chat: 0 off, 1 beside the video, 2 over it
    video_ghost: bool, // clicks go through the video
    pos: Option<(i32, i32)>, // pill top-left, physical px
}

impl Default for Settings {
    fn default() -> Self {
        Settings { theme: 0, shown: [true; 5], size: 1, opacity: 0, interval: 1, side_layout: true, vertical: false, click_through: false, card: false, smooth: 0, admin_mode: false, clock: true, anim: 0, asr: 0, llm: 0, chime: true, explain: 0, final_pass: true, keep_audio: true, half_chime: true, cues: true, tx_cloud: false, voice: true, fps: 3, duck: true, duck_ai: true, duck_roles: String::new(), art_tint: true, kbd_show: true, guide_done: false, rec_dir: String::new(), auto_hide: false, gpu: false, gpu_asked: false, video_url: String::new(), video_w: 0, video_alpha: 0, video_chat: 1, video_ghost: false, pos: None }
    }
}

impl Settings {
    fn path() -> Option<std::path::PathBuf> {
        Some(std::path::PathBuf::from(std::env::var_os("APPDATA")?).join("ResourcePill").join("settings.ini"))
    }

    fn load() -> Settings {
        let mut s = Settings::default();
        let Some(text) = Settings::path().and_then(|p| std::fs::read_to_string(p).ok()) else { return s };
        let mut pos = (None, None);
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let n = v.parse::<i64>().ok();
            let idx = |max: usize| n.filter(|&n| n >= 0 && (n as usize) < max).map(|n| n as usize);
            match k {
                "theme" => s.theme = idx(THEMES.len()).unwrap_or(0),
                "size" => s.size = idx(SIZES.len()).unwrap_or(1),
                "opacity" => s.opacity = idx(OPACITY.len()).unwrap_or(0),
                "interval" => s.interval = idx(INTERVALS.len()).unwrap_or(1),
                "side_layout" => s.side_layout = v == "1",
                "vertical" => s.vertical = v == "1",
                "click_through" => s.click_through = v == "1",
                "card" => s.card = v == "1",
                "smooth" => s.smooth = idx(SMOOTH.len()).unwrap_or(0),
                "admin_mode" => s.admin_mode = v == "1",
                "clock" => s.clock = v == "1",
                "anim" => s.anim = idx(ANIMS.len()).unwrap_or(0),
                "asr" => s.asr = idx(recorder::ASR.len()).unwrap_or(0),
                "llm" => s.llm = idx(recorder::LLM.len() + 1).unwrap_or(0),
                "chime" => s.chime = v == "1",
                "explain" => s.explain = idx(3).unwrap_or(0),
                "final_pass" => s.final_pass = v == "1",
                "keep_audio" => s.keep_audio = v == "1",
                "half_chime" => s.half_chime = v == "1",
                "cues" => s.cues = v == "1",
                "tx_cloud" => s.tx_cloud = v == "1",
                "voice" => s.voice = v == "1",
                "fps" => s.fps = idx(FPS.len()).unwrap_or(3),
                "duck" => s.duck = v == "1",
                "duck_ai" => s.duck_ai = v == "1",
                "duck_roles" => s.duck_roles = duck::format_roles(&duck::parse_roles(v)),
                "art_tint" => s.art_tint = v == "1",
                "kbd_show" => s.kbd_show = v == "1",
                "guide_done" => s.guide_done = v == "1",
                "rec_dir" => s.rec_dir = v.trim().to_string(),
                "auto_hide" => s.auto_hide = v == "1",
                "gpu" => s.gpu = v == "1",
                "gpu_asked" => s.gpu_asked = v == "1",
                "video_url" => s.video_url = v.to_string(),
                "video_w" => s.video_w = n.filter(|&n| (0..=10_000).contains(&n)).unwrap_or(0) as u32,
                "video_alpha" => s.video_alpha = idx(video::ALPHAS.len()).unwrap_or(0),
                "video_chat" => s.video_chat = idx(video::CHATS.len()).unwrap_or(1),
                "video_ghost" => s.video_ghost = v == "1",
                "shown" if v.len() == 5 => {
                    for (i, c) in v.chars().enumerate() {
                        s.shown[i] = c == '1';
                    }
                }
                "x" => pos.0 = n.map(|n| n as i32),
                "y" => pos.1 = n.map(|n| n as i32),
                _ => {}
            }
        }
        if !s.shown.iter().any(|&b| b) {
            s.shown = [true; 5];
        }
        s.vertical &= s.side_layout;
        if let (Some(x), Some(y)) = pos {
            s.pos = Some((x, y));
        }
        s
    }

    fn save(&self) {
        let Some(p) = Settings::path() else { return };
        let b = |v: bool| if v { 1 } else { 0 };
        let shown: String = self.shown.iter().map(|&v| if v { '1' } else { '0' }).collect();
        let (x, y) = self.pos.unwrap_or((0, 0));
        let text = format!(
            "theme={}\nsize={}\nopacity={}\ninterval={}\nside_layout={}\nvertical={}\nclick_through={}\ncard={}\nsmooth={}\nadmin_mode={}\nclock={}\nanim={}\nasr={}\nllm={}\nchime={}\nexplain={}\nfinal_pass={}\nkeep_audio={}\nhalf_chime={}\ncues={}\ntx_cloud={}\nvoice={}\nfps={}\nduck={}\nduck_ai={}\nduck_roles={}\nart_tint={}\nkbd_show={}\nguide_done={}\nrec_dir={}\nauto_hide={}\ngpu={}\ngpu_asked={}\nvideo_url={}\nvideo_w={}\nvideo_alpha={}\nvideo_chat={}\nvideo_ghost={}\nshown={}\nx={}\ny={}\n",
            self.theme, self.size, self.opacity, self.interval, b(self.side_layout), b(self.vertical), b(self.click_through), b(self.card), self.smooth, b(self.admin_mode), b(self.clock), self.anim, self.asr, self.llm, b(self.chime), self.explain, b(self.final_pass), b(self.keep_audio), b(self.half_chime), b(self.cues), b(self.tx_cloud), b(self.voice), self.fps, b(self.duck), b(self.duck_ai), self.duck_roles, b(self.art_tint), b(self.kbd_show), b(self.guide_done), self.rec_dir, b(self.auto_hide), b(self.gpu), b(self.gpu_asked), self.video_url.replace(['\r', '\n'], ""), self.video_w, self.video_alpha, self.video_chat, b(self.video_ghost), shown, x, y
        );
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
        // write-then-rename so a crash mid-write never leaves a corrupt file
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

// ---------- geometry ----------

#[derive(Clone, Copy, Default, PartialEq)]
struct R {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl R {
    fn d2d(self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
    fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Dir {
    Down,
    Up,
    Right,
    Left,
}

/// Everything positioned in window DIPs.
/// Something that moves every frame, recorded by the still pass and drawn by the moving pass.
#[derive(Clone, Copy)]
enum Moving {
    Eq(f32, f32, f32), // music bars: x, baseline, height
    Progress(R),       // song progress
    Mascot(R, R),      // Peter: his cell, the pill
    Badge(R),          // the auto-lower badge on the speaker button
    Ring(usize, f32, f32, f32), // a metric's ring: which, centre x, y, radius
    Spotlight(R),      // a pulsing ring pointing something out (the setup guide)
}

struct Layout {
    clock: Option<R>, // clock cell (pill) or clock area (card header)
    rec: R,           // record button
    mascot: R,        // the living helper icon (click: explain mode)
    gear: R,          // settings button (same menu as right-click)
    strip: Option<R>, // live transcript strip (while recording / until dismissed)
    music: Option<R>, // now-playing cell (only while something plays or is paused)
    mixer: R, // volume mixer button
    kbd: Option<R>, // keyboard connection (when an external keyboard is known)
    vertical: bool,
    pill: R,
    cells: Vec<(usize, R)>,
    panel: R,
    dir: Dir,
    win_w: f32,
    win_h: f32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn main() {
    unsafe {
        let arg = |a: &str| std::env::args().any(|x| x == a);
        let elevated = admin::is_elevated();
        // Elevated helper launched from the menu to turn admin mode off.
        // `ResourcePill.exe --quit` closes a running copy (also an elevated one), e.g. before updating the exe.
        // `--where-helper`: unpack the recording helper if this is a one-file build, print where it is
        if arg("--where-helper") {
            println!("{}", recorder::helper().map_or("not found".into(), |p| p.display().to_string()));
            return;
        }
        // `--make-bundle <out.exe>`: one exe with the recording helper packed inside (see bundle.rs)
        if let Some(out) = std::env::args().skip_while(|a| a != "--make-bundle").nth(1) {
            std::process::exit(match bundle::make(std::path::Path::new(&out)) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            });
        }
        if arg("--quit") {
            if let Ok(h) = FindWindowW(CLASS, None) {
                let _ = PostMessageW(Some(h), WM_APP_QUIT, WPARAM(0), LPARAM(0));
            }
            return;
        }
        if arg("--disable-admin") {
            std::process::exit(if elevated && admin::remove_task() { 0 } else { 1 });
        }
        // `--snapshots <folder>`: draw every view to PNGs and exit (nothing on screen, settings untouched)
        if let Some(dir) = std::env::args().skip_while(|a| a != "--snapshots").nth(1) {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            let Ok(hinst) = GetModuleHandleW(None) else { return };
            let class = w!("ResourcePill.Snapshot");
            RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() });
            let Ok(hwnd) = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW, class, w!(""), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None) else { return };
            if let Some(mut app) = App::new(hwnd, Settings::load()) {
                snapshot::run(&mut app, dir.into());
            }
            return;
        }
        // One instance only; a second launch just shows the running one. A copy started by the
        // admin task (or the enable flow) waits briefly for the previous copy to finish exiting.
        // `--after-update <pid>`: started by the copy it replaced; wait (up to a minute) for that one to exit
        let after_update = std::env::args().skip_while(|a| a != "--after-update").nth(1).and_then(|p| p.parse::<u32>().ok());
        if let Some(pid) = after_update {
            use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
            if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                WaitForSingleObject(h, 60_000);
                let _ = CloseHandle(h);
            }
        }
        let handover = arg("--from-task") || arg("--enable-admin") || after_update.is_some();
        let mut guard = None;
        for _ in 0..if handover { 100 } else { 1 } {
            let m = CreateMutexW(None, true, w!("Local\\ResourcePill.Single"));
            if GetLastError() != ERROR_ALREADY_EXISTS {
                guard = Some(m);
                break;
            }
            if let Ok(m) = m {
                let _ = CloseHandle(m);
            }
            if handover {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        if guard.is_none() {
            if let Ok(h) = FindWindowW(CLASS, None) {
                let _ = PostMessageW(Some(h), WM_APP_SHOW, WPARAM(0), LPARAM(0));
            }
            return;
        }
        let mut settings = Settings::load();
        if arg("--enable-admin") && elevated {
            if admin::install_task() {
                settings.admin_mode = true;
                set_autostart(false); // the task starts it at login now
                settings.save();
            }
        }
        // Admin mode on but launched normally (e.g. double-clicked): hand over to the elevated task.
        if settings.admin_mode && !elevated && !handover {
            if let Some(Ok(m)) = guard.take() {
                let _ = CloseHandle(m);
            }
            if admin::run_task() {
                return;
            }
            // task missing or blocked: carry on without admin
            guard = Some(CreateMutexW(None, true, w!("Local\\ResourcePill.Single")));
        }
        let _guard = guard;
        explainui::repair_cursors(); // last run ended with glowing cursors (crash): restore them
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED); // cover art + voice
        admin::spawn_cpu_temp();
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        dark_menus();
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);

        let Ok(hinst) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            style: CS_DBLCLKS,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let (x, y) = settings.pos.unwrap_or((0, 0));
        let mut ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        if settings.click_through {
            ex |= WS_EX_TRANSPARENT;
        }
        let Ok(hwnd) = CreateWindowExW(ex, CLASS, w!("ResourcePill"), WS_POPUP, x, y, 1, 1, None, None, Some(hinst.into()), None) else {
            return;
        };
        // When elevated, let the (non-elevated) shell and second launches still reach us.
        for m in [WM_APP_SHOW, WM_APP_QUIT, WM_TRAY, TASKBAR_CREATED.load(Ordering::Relaxed)] {
            let _ = ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None);
        }

        let Some(app) = App::new(hwnd, settings) else {
            MessageBoxW(None, w!("Could not initialise Direct2D."), w!("ResourcePill"), MB_ICONERROR);
            return;
        };
        APP.with(|c| *c.borrow_mut() = Some(app));
        with(|a| a.start());

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        APP.with(|c| c.borrow_mut().take()); // release GDI/COM objects before exit
    }
}

/// Runs `f` on the app unless it is already borrowed (re-entrant message).
fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_RBUTTONUP | WM_APP_MENU => {
                with(|a| a.toggle_settings()); // the in-pill settings panel
                return LRESULT(0);
            }
            WM_TRAY => {
                match lp.0 as u32 {
                    WM_LBUTTONUP => {
                        with(|a| a.set_visible(!a.want_visible));
                    }
                    WM_RBUTTONUP | WM_CONTEXTMENU => settings::native_menu(hwnd), // tray: works while the pill is hidden
                    _ => {}
                }
                return LRESULT(0);
            }
            WM_DPICHANGED => {
                // May arrive re-entrantly from our own move; handle it afterwards.
                let _ = PostMessageW(Some(hwnd), WM_APP_DPI, WPARAM(0), LPARAM(0));
                return LRESULT(0);
            }
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_APP_QUIT => {
                with(|a| a.quit());
                return LRESULT(0);
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                return LRESULT(0);
            }
            m if m == TASKBAR_CREATED.load(Ordering::Relaxed) && m != 0 => {
                with(|a| a.tray(NIM_ADD));
                return LRESULT(0);
            }
            _ => {}
        }
        with(|a| a.handle(msg, wp, lp)).flatten().unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
    }
}

struct Fonts {
    label: IDWriteTextFormat,
    value: IDWriteTextFormat,
    net: IDWriteTextFormat,
    c_label: IDWriteTextFormat,
    c_value: IDWriteTextFormat,
    c_net: IDWriteTextFormat,
    title: IDWriteTextFormat,
    detail: IDWriteTextFormat,
    small: IDWriteTextFormat,
    wrap: IDWriteTextFormat, // multi-line text (explanations)
    clock: IDWriteTextFormat,
    c_clock: IDWriteTextFormat,
    clock_big: IDWriteTextFormat,
    big: IDWriteTextFormat,
    row: IDWriteTextFormat,
    row_val: IDWriteTextFormat,
    icon: IDWriteTextFormat, // Segoe Fluent Icons (MDL2 Assets on Windows 10)
    icon_big: IDWriteTextFormat,
}

/// Centred icon-font format: Segoe Fluent Icons on Windows 11, MDL2 Assets on 10 (same code points).
fn icon_fmt(dw: &IDWriteFactory, size: f32) -> Option<IDWriteTextFormat> {
    unsafe {
        let mut fonts = None;
        let _ = dw.GetSystemFontCollection(&mut fonts, false);
        let (mut idx, mut found) = (0u32, BOOL(0));
        let fluent = fonts.is_some_and(|c| c.FindFamilyName(w!("Segoe Fluent Icons"), &mut idx, &mut found).is_ok() && found.as_bool());
        let family = if fluent { w!("Segoe Fluent Icons") } else { w!("Segoe MDL2 Assets") };
        let f = dw.CreateTextFormat(family, None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us")).ok()?;
        let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        Some(f)
    }
}

struct App {
    hwnd: HWND,
    cfg: Settings,
    shared: Arc<Shared>,
    wake: Option<mpsc::Sender<()>>,
    snap: Snap,
    hist: [std::collections::VecDeque<f32>; 5], // card graphs (NET holds raw bytes/s)
    icon: HICON,

    d2d: ID2D1Factory,
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    round: ID2D1StrokeStyle,
    f: Fonts,
    dw: IDWriteFactory,

    memdc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    scale: f32, // physical px per DIP (monitor DPI × size setting)
    pw: i32,
    ph: i32,

    // animation state
    disp: [f32; 5],
    sel: usize,
    panel_sel: usize,
    hover: usize,
    open: f32,
    hl: [f32; 4],
    hl_a: f32,
    rec: recorder::Recorder,
    job: Option<recorder::Job>,
    explain_mode: bool,
    strip_expanded: bool, // click the live transcript to see more lines
    menu_open: bool,
    gear_hover: bool,
    settings: Option<Vec<usize>>, // open settings panel: path into the item tree
    settings_cache: Vec<settings::MI>,
    settings_scroll: f32,
    settings_hover: Option<usize>,
    nav_t: f32, // 0..1 slide-in after navigating
    tuck: f32, // 0 = shown, 1 = tucked into the screen edge (animated)
    tucked: bool, // target of `tuck`
    near_at: Instant, // last time the mouse was at the pill (auto-hide waits a moment after)
    pop: Option<explainui::Pop>, // explain tooltip by the highlight
    media: Option<(Arc<Mutex<media::Now>>, mpsc::Sender<media::Cmd>)>,
    now: media::Now, // copy for drawing
    art: Option<ID2D1Bitmap>, // decoded cover of `now`
    music_open: bool,
    mixer: Option<mixer::Mixer>, // open volume mixer
    mix_drag: Option<usize>, // mixer row whose slider is held
    duck: Arc<duck::Shared>, // auto-lower: switches and state shared with its thread
    duck_thread: Option<std::thread::JoinHandle<()>>,
    icons: std::collections::HashMap<String, ID2D1Bitmap>, // app icons by program path (the auto-lower badge)
    voice: Option<windows::Win32::Media::Speech::ISpVoice>,
    blink_at: Option<Instant>,
    vram_total: f64,
    fade: f32,
    want_visible: bool,
    last_frame: Instant,
    anim_on: bool,
    alpha: f32, // multiplier applied by `color`
    pass: u8, // 0 draw everything; 1 the still layer (moving pieces only recorded); 2 the moving pieces
    rings_only: bool, // the current animation is just the rings gliding to new readings
    beat: std::cell::Cell<f32>, // how loud the PC's sound is right now, smoothed (the aurora breathes with it)
    beat_meter: RefCell<Option<windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation>>,
    moving: RefCell<Vec<(Moving, f32)>>, // moving pieces recorded by the still pass (with their alpha)
    content: Option<(ID2D1Bitmap, i32, i32)>, // cached content layer (text, rings, art…), ready to draw
    back: Option<(HDC, HBITMAP, HGDIOBJ, i32, i32)>, // cached background layer: the surfaces under the content
    auroras: RefCell<Vec<(R, f32)>>, // surfaces that get the drifting aurora (between background and content)
    tint: [f32; 6], // song-cover colours now (two RGB colours), gliding towards `tint_to`
    tint_to: [f32; 6],
    tint_a: f32, // how strongly they show (0 none … 1 full), gliding towards `tint_a_to`
    tint_a_to: f32,
    kbd_state: Option<keyboard::Status>, // your external keyboard (None: never had one)
    guide: Option<u8>, // the setup guide, open at this step
    setup_queue: Vec<&'static str>, // models still to download for transcription
    setup_total: usize, // how many this round (for "2 of 4")
    setup_current: Option<&'static str>, // the one downloading now
    record_when_ready: bool, // record was pressed while getting ready
    updating: Option<update::Release>, // the update being downloaded
    video: Option<video::Video>, // YouTube under the pill (while open)

    // dragging: cursor offset inside the pill as a fraction, so it survives orientation changes
    press: Option<(POINT, (f32, f32))>,
    dragging: bool,
    vol_drag: bool, // pressing on the player's volume slider
    tracking: bool,

    // clock + hourly sheen
    time_text: String,
    date_text: String,
    hour: Option<u16>,
    half_past: Option<bool>, // was the clock past :30 at the last check
    sheen: f32, // progress 0..1 while playing, < 0 when idle

}

impl App {
    unsafe fn new(hwnd: HWND, cfg: Settings) -> Option<App> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                // Software: tiny surface, no GPU wake-ups, no device-lost handling needed.
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                ..Default::default()
            };
            let rt = d2d.CreateDCRenderTarget(&props).ok()?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = rt.CreateSolidColorBrush(&color(0xFFFFFF, 1.0), None).ok()?;
            let round = d2d
                .CreateStrokeStyle(&D2D1_STROKE_STYLE_PROPERTIES { startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, ..Default::default() }, None)
                .ok()?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
            let fmt = |size: f32, weight: DWRITE_FONT_WEIGHT, align: DWRITE_TEXT_ALIGNMENT| -> Option<IDWriteTextFormat> {
                let f = dw
                    .CreateTextFormat(w!("Segoe UI Variable Text"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))
                    .ok()?;
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                let _ = f.SetTextAlignment(align);
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
                if let Ok(sign) = dw.CreateEllipsisTrimmingSign(&f) {
                    let _ = f.SetTrimming(&trim, &sign);
                }
                Some(f)
            };
            let (l, c, t) = (DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_TRAILING);
            let (semi, normal) = (DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_WEIGHT_NORMAL);
            let f = Fonts {
                label: fmt(10.0, semi, l)?,
                value: fmt(14.0, semi, l)?,
                net: fmt(11.0, semi, l)?,
                c_label: fmt(9.0, semi, c)?,
                c_value: fmt(13.0, semi, c)?,
                c_net: fmt(9.5, semi, c)?,
                title: fmt(13.5, semi, l)?,
                detail: fmt(11.0, normal, t)?,
                small: fmt(10.5, normal, l)?,
                wrap: {
                    let w = fmt(12.5, normal, l)?;
                    let _ = w.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
                    let _ = w.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
                    w
                },
                clock: fmt(15.0, semi, l)?,
                c_clock: fmt(13.5, semi, c)?,
                clock_big: fmt(22.0, semi, l)?,
                big: fmt(18.0, semi, l)?,
                row: fmt(12.0, normal, l)?,
                row_val: fmt(12.0, semi, t)?,
                icon: icon_fmt(&dw, 13.0)?,
                icon_big: icon_fmt(&dw, 18.0)?,
            };
            let shared = Arc::new(Shared {
                snap: Mutex::new(Snap::default()),
                selected: NONE.into(),
                visible: true.into(),
                interval_ms: INTERVALS[cfg.interval].1.into(),
                smoothing: (cfg.smooth as u32).into(),
            });
            Some(App {
                hwnd,
                cfg,
                shared,
                wake: None,
                snap: Snap::default(),
                hist: Default::default(),
                icon: make_icon(),
                d2d,
                rt,
                brush,
                round,
                f,
                dw: dw.clone(),
                memdc: CreateCompatibleDC(None),
                bmp: HBITMAP::default(),
                old: HGDIOBJ::default(),
                scale: 1.0,
                pw: 0,
                ph: 0,
                disp: [0.0; 5],
                sel: NONE,
                panel_sel: NONE,
                hover: NONE,
                open: 0.0,
                hl: [0.0; 4],
                hl_a: 0.0,
                rec: recorder::Recorder::new(),
                job: None,
                explain_mode: false,
                strip_expanded: false,
                menu_open: false,
                gear_hover: false,
                settings: None,
                settings_cache: Vec::new(),
                settings_scroll: 0.0,
                settings_hover: None,
                nav_t: 1.0,
                tuck: 0.0,
                tucked: false,
                near_at: Instant::now(),
                pop: None,
                media: None,
                now: media::Now::default(),
                art: None,
                music_open: false,
                mixer: None,
                mix_drag: None,
                duck: Arc::new(duck::Shared::new()), // switches set from the settings in start()
                duck_thread: None,
                icons: Default::default(),
                voice: None,
                blink_at: None,
                vram_total: 0.0,
                fade: 0.0,
                want_visible: true,
                last_frame: Instant::now(),
                anim_on: false,
                alpha: 1.0,
                pass: 0,
                rings_only: false,
                beat: std::cell::Cell::new(0.0),
                beat_meter: RefCell::new(None),
                moving: RefCell::new(Vec::new()),
                content: None,
                back: None,
                auroras: RefCell::new(Vec::new()),
                tint: [0.0; 6],
                tint_to: [0.0; 6],
                tint_a: 0.0,
                tint_a_to: 0.0,
                kbd_state: None,
                guide: None,
                setup_queue: Vec::new(),
                setup_total: 0,
                setup_current: None,
                record_when_ready: false,
                updating: None,
                video: None,
                press: None,
                dragging: false,
                vol_drag: false,
                tracking: false,
                time_text: String::new(),
                date_text: String::new(),
                hour: None,
                half_past: None,
                sheen: 0.0, // play once at startup
            })
        }
    }

    unsafe fn start(&mut self) {
        unsafe {
            self.update_scale();
            match self.cfg.pos {
                Some((x, y)) if on_screen(x, y) => self.place_at(x, y, None),
                _ => self.place(0),
            }
            // Ctrl+Alt+M toggles. If another app owns it, the tray icon still works.
            let _ = RegisterHotKey(Some(self.hwnd), HOTKEY, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'M' as u32);
            let _ = RegisterHotKey(Some(self.hwnd), HOTKEY_REC, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'R' as u32);
            let _ = RegisterHotKey(Some(self.hwnd), HOTKEY_EXPLAIN, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'E' as u32);
            let _ = RegisterHotKey(Some(self.hwnd), HOTKEY_MIXER, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'V' as u32);
            let _ = RegisterHotKey(Some(self.hwnd), HOTKEY_VIDEO, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'Y' as u32);
            let _ = RegisterHotKey(Some(self.hwnd), HOTKEY_GHOST, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'G' as u32);
            let _ = RegisterHotKey(Some(self.hwnd), HOTKEY_LISTS, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'L' as u32);
            SetTimer(Some(self.hwnd), TIMER_BLINK, 3000, None);
            self.sync_edge_timer();
            self.vram_total = recorder::vram_total();
            self.tray(NIM_ADD);
            if autostart() {
                set_autostart(true); // keep the path current if the exe moved
            }
            let h = self.hwnd.0 as isize;
            self.wake = Some(stats::spawn(self.shared.clone(), move || {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_DATA, WPARAM(0), LPARAM(0));
            }));
            self.media = Some(media::spawn(self.hwnd, WM_APP_MEDIA));
            self.check_keyboard(false);
            update::spawn_checks();
            recorder::set_recordings_dir((!self.cfg.rec_dir.is_empty()).then(|| std::path::PathBuf::from(&self.cfg.rec_dir)));
            // out of the box: fetch what recording needs right away, and show the guide once
            for id in self.missing_for_recording() {
                self.setup_queue.push(id);
                self.setup_total += 1;
            }
            if !self.setup_queue.is_empty() {
                self.setup_next();
            }
            if !self.cfg.guide_done {
                self.open_guide(0);
            }
            self.duck.on.store(self.cfg.duck, Ordering::Relaxed);
            self.duck.ai.store(self.cfg.duck_ai, Ordering::Relaxed);
            self.duck.set_roles(duck::parse_roles(&self.cfg.duck_roles));
            self.duck.notify.store(self.hwnd.0 as isize, Ordering::Relaxed);
            self.duck_thread = duck::spawn(self.duck.clone());
            if self.cfg.card {
                self.select(self.shown()[0]);
            }
            self.update_clock();
            self.render();
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            self.kick();
        }
    }

    /// Refresh the clock text (your Windows time/date format); on a new hour, play the sheen.
    fn update_clock(&mut self) {
        use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS};
        use windows::Win32::System::SystemInformation::GetLocalTime;
        let now = unsafe { GetLocalTime() };
        let mut buf = [0u16; 64];
        let n = unsafe { GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, Some(&now), PCWSTR::null(), Some(&mut buf)) };
        self.time_text = if n > 1 { String::from_utf16_lossy(&buf[..n as usize - 1]) } else { format!("{:02}:{:02}", now.wHour, now.wMinute) };
        let n = unsafe { GetDateFormatEx(PCWSTR::null(), Default::default(), Some(&now), w!("ddd d MMM"), Some(&mut buf), PCWSTR::null()) };
        self.date_text = if n > 1 { String::from_utf16_lossy(&buf[..n as usize - 1]) } else { String::new() };
        if self.hour.is_some_and(|h| h != now.wHour) {
            self.sheen = 0.0; // on the hour
            if self.cfg.chime {
                extras::play_chime();
            }
        }
        // half past: a softer, different ding (the hourly chime covers :00)
        let half = now.wMinute >= 30;
        if self.half_past.is_some_and(|h| !h) && half && self.hour == Some(now.wHour) && self.cfg.half_chime {
            extras::play_half_chime();
        }
        self.half_past = Some(half);
        self.hour = Some(now.wHour);
    }

    /// Height of a top list (panel or card section): grows with its rows, never below 3.
    fn list_h(&self) -> f32 {
        let ps = if self.cfg.card { self.sel } else { self.panel_sel };
        let rows = if ps != NONE && self.snap.top_for == ps { self.snap.top.len() } else { 0 };
        54.0 + rows.max(3) as f32 * ROW_H
    }

    /// Segment label; the disk segment names the busiest drive.
    fn label(&self, i: usize) -> String {
        let base = if i == stats::DISK && !self.snap.disk_label.is_empty() { self.snap.disk_label.as_str() } else { LABELS[i] };
        match self.snap.temp[i] {
            Some(t) => format!("{base} {t:.0}°"),
            None => base.to_string(),
        }
    }

    /// Label colour: the theme's hot colour once a component runs hot, else muted text.
    fn label_color(&self, i: usize) -> (u32, f32) {
        let limit = [90.0, f64::MAX, 85.0, 70.0, f64::MAX][i];
        match self.snap.temp[i] {
            Some(t) if t >= limit => (self.theme().hot, 1.0),
            _ => (self.theme().fg, 0.5),
        }
    }

    fn theme(&self) -> &'static Theme {
        &THEMES[self.cfg.theme.min(THEMES.len() - 1)]
    }

    fn update_scale(&mut self) {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        self.scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 } * SIZES[self.cfg.size].1;
    }

    // ---------- layout & placement ----------

    fn shown(&self) -> Vec<usize> {
        (0..5).filter(|&i| self.cfg.shown[i]).collect()
    }

    /// Pill size in DIPs for an orientation.
    fn pill_size(&self, vertical: bool) -> (f32, f32) {
        let s = self.shown();
        if self.cfg.card {
            (CARD_W, CARD_HEAD + s.len() as f32 * CARD_ROW + self.list_h())
        } else if vertical {
            (V_PILL, s.len() as f32 * V_SEG + PAD * 2.0 + GEAR_V + REC_V + MASCOT_V + if self.cfg.clock { CLOCK_V } else { 0.0 } + MIXER_V + if self.show_kbd() { KBD_V } else { 0.0 } + if self.now.present { MUSIC_V } else { 0.0 })
        } else {
            (s.iter().map(|&i| H_SEG[i]).sum::<f32>() + PAD * 2.0 + GEAR_W + REC_W + MASCOT_W + if self.cfg.clock { CLOCK_W } else { 0.0 } + MIXER_W + if self.show_kbd() { KBD_W } else { 0.0 } + if self.now.present { MUSIC_W } else { 0.0 }, H_PILL)
        }
    }

    fn strip_visible(&self) -> bool {
        self.rec.state != recorder::State::Idle || self.job.is_some()
    }

    fn layout(&self) -> Layout {
        let vertical = self.cfg.vertical && !self.cfg.card;
        let (pw, ph) = self.pill_size(vertical);
        let mut cells = Vec::new();
        let mut clock = None;
        let rec;
        let mascot;
        let gear;
        let mut music = None;
        let mixer_btn;
        let mut kbd = None;
        if self.cfg.card {
            for (n, i) in self.shown().into_iter().enumerate() {
                cells.push((i, R { x: 8.0, y: CARD_HEAD + n as f32 * CARD_ROW, w: pw - 16.0, h: CARD_ROW }));
            }
            clock = Some(R { x: 20.0, y: 8.0, w: 200.0, h: CARD_HEAD - 12.0 });
            rec = R { x: pw - 48.0, y: 8.0, w: 34.0, h: 32.0 };
            mascot = R { x: pw - 86.0, y: 8.0, w: 36.0, h: 32.0 };
            gear = R { x: pw - 122.0, y: 8.0, w: 34.0, h: 32.0 };
            mixer_btn = R { x: pw - 156.0, y: 8.0, w: 34.0, h: 32.0 };
        } else {
            let mut off = PAD;
            gear = if vertical { R { x: 0.0, y: off, w: pw, h: GEAR_V } } else { R { x: off, y: 0.0, w: GEAR_W, h: ph } };
            off += if vertical { GEAR_V } else { GEAR_W };
            if self.cfg.clock {
                clock = Some(if vertical { R { x: 0.0, y: off, w: pw, h: CLOCK_V } } else { R { x: off, y: 0.0, w: CLOCK_W, h: ph } });
                off += if vertical { CLOCK_V } else { CLOCK_W };
            }
            rec = if vertical { R { x: 0.0, y: off, w: pw, h: REC_V } } else { R { x: off, y: 0.0, w: REC_W, h: ph } };
            off += if vertical { REC_V } else { REC_W };
            mascot = if vertical { R { x: 0.0, y: off, w: pw, h: MASCOT_V } } else { R { x: off, y: 0.0, w: MASCOT_W, h: ph } };
            off += if vertical { MASCOT_V } else { MASCOT_W };
            for i in self.shown() {
                if vertical {
                    cells.push((i, R { x: 0.0, y: off, w: pw, h: V_SEG }));
                    off += V_SEG;
                } else {
                    cells.push((i, R { x: off, y: 0.0, w: H_SEG[i], h: ph }));
                    off += H_SEG[i];
                }
            }
            mixer_btn = if vertical { R { x: 0.0, y: off, w: pw, h: MIXER_V } } else { R { x: off, y: 0.0, w: MIXER_W, h: ph } };
            off += if vertical { MIXER_V } else { MIXER_W };
            if self.show_kbd() {
                kbd = Some(if vertical { R { x: 0.0, y: off, w: pw, h: KBD_V } } else { R { x: off, y: 0.0, w: KBD_W, h: ph } });
                off += if vertical { KBD_V } else { KBD_W };
            }
            if self.now.present {
                music = Some(if vertical { R { x: 0.0, y: off, w: pw, h: MUSIC_V } } else { R { x: off, y: 0.0, w: MUSIC_W, h: ph } });
            }
        }
        // Extras (transcript strip, then the details panel) go toward the side of the screen with room.
        let (px, py) = self.cfg.pos.unwrap_or((0, 0));
        let wa = work_area(POINT { x: px + (pw * self.scale) as i32 / 2, y: py + (ph * self.scale) as i32 / 2 });
        let rel = |v: i32, o: i32| (v - o) as f32 / self.scale; // screen px → DIPs relative to pill
        let (wl, wr, wt, wb) = (rel(wa.left, px), rel(wa.right, px), rel(wa.top, py), rel(wa.bottom, py));
        let dir = if vertical {
            if (pw / 2.0 - wl) < (wr - pw / 2.0) { Dir::Right } else { Dir::Left }
        } else if (ph / 2.0 - wt) < (wb - ph / 2.0) {
            Dir::Down
        } else {
            Dir::Up
        };
        let side_w = if vertical { PANEL_W } else { pw.max(PANEL_W) };
        let clamp_x = |x: f32, w: f32| x.clamp(wl + EDGE, (wr - EDGE - w).max(wl + EDGE));
        let strip = self.strip_visible().then(|| {
            let (x, y) = match dir {
                Dir::Down => ((pw - side_w) / 2.0, ph + GAP),
                Dir::Up => ((pw - side_w) / 2.0, -GAP - self.strip_h(side_w)),
                Dir::Right => (pw + GAP, 0.0),
                Dir::Left => (-GAP - side_w, 0.0),
            };
            R { x: clamp_x(x, side_w), y, w: side_w, h: self.strip_h(side_w) }
        });
        let used = strip.map_or(0.0, |s| s.h + GAP); // the panel goes beyond the strip
        let anchor = if self.mixer.is_some() { Some(mixer_btn) } else if self.music_open { music } else { None }.or(cells.iter().find(|c| c.0 == self.panel_sel).or(cells.first()).map(|c| c.1)).unwrap_or_default();
        let lh = if self.guide.is_some() {
            setup::GUIDE_H
        } else if self.settings.is_some() {
            self.settings_h()
        } else if let Some(m) = &self.mixer {
            m.height()
        } else if self.music_open {
            MUSIC_PANEL_H
        } else {
            self.list_h()
        };
        let mut panel = match dir {
            Dir::Down => R { x: (pw - side_w) / 2.0, y: ph + GAP + used, w: side_w, h: lh },
            Dir::Up => R { x: (pw - side_w) / 2.0, y: -GAP - used - lh, w: side_w, h: lh },
            Dir::Right | Dir::Left => {
                let x = if dir == Dir::Right { pw + GAP } else { -GAP - side_w };
                R { x, y: (anchor.y + anchor.h / 2.0 - lh / 2.0).max(used), w: side_w, h: lh }
            }
        };
        // horizontal pill: a panel opens under whatever opened it, at a comfortable width (not
        // stretched across the whole pill): settings under the gear, a list under its metric, the
        // player and the mixer under their own buttons
        if !vertical && !self.cfg.card {
            let (w, under) = if self.settings.is_some() || self.guide.is_some() {
                (WIDE_PANEL_W, Some(gear))
            } else if self.mixer.is_some() {
                (PANEL_W, Some(mixer_btn))
            } else if self.music_open {
                (PANEL_W, music)
            } else {
                (WIDE_PANEL_W, cells.iter().find(|c| c.0 == self.panel_sel).map(|c| c.1))
            };
            if let Some(u) = under {
                panel.w = w;
                panel.x = if self.settings.is_some() || self.guide.is_some() { u.x - 10.0 } else { u.x + u.w / 2.0 - w / 2.0 };
            }
        }
        // the card is tall: settings below it would run off the screen, so they open beside it,
        // on whichever side has room, lined up with the card's top
        if self.cfg.card && (self.settings.is_some() || self.guide.is_some()) {
            let room_right = wr - EDGE - (pw + GAP);
            let room_left = -GAP - (wl + EDGE);
            if room_right >= panel.w || room_left >= panel.w {
                panel.x = if room_right >= panel.w { pw + GAP } else { -GAP - panel.w };
                panel.y = 0.0;
            }
        }
        panel.x = clamp_x(panel.x, panel.w);
        // never past the top or bottom of the screen: a panel that doesn't fit gets shorter (the
        // settings scroll, with a fade showing there's more)
        let (lo, hi) = (wt + EDGE, wb - EDGE);
        let beside = panel.x >= pw || panel.x + panel.w <= 0.0;
        if panel.y < lo {
            panel.h -= lo - panel.y;
            panel.y = lo;
        }
        if panel.y + panel.h > hi {
            if beside {
                panel.y = (hi - panel.h).max(lo); // slide up first, then shorten if still too tall
            }
            panel.h = hi - panel.y;
        }
        panel.h = panel.h.max(TOP_MIN_PANEL);
        if vertical {
            panel.y = panel.y.clamp((wt + EDGE).max(used), (wb - EDGE - panel.h).max(wt + EDGE));
        }
        // window = union(pill, strip, panel if it can show) + margin; shift everything into window space
        let mut parts = vec![R { x: 0.0, y: 0.0, w: pw, h: ph }];
        parts.extend(strip);
        // the panel's space only while one shows (or is still closing): a bare pill keeps a small
        // window, so every frame has far fewer pixels to paint and hand to Windows
        let panel_showing = self.open > 0.003 || self.sel != NONE || self.settings.is_some() || self.guide.is_some() || self.music_open || self.mixer.is_some();
        if if self.cfg.card { self.settings.is_some() || self.guide.is_some() } else { panel_showing } {
            parts.push(panel);
        }
        let l = parts.iter().map(|r| r.x).fold(0.0f32, f32::min);
        let t = parts.iter().map(|r| r.y).fold(0.0f32, f32::min);
        let r = parts.iter().map(|r| r.x + r.w).fold(pw, f32::max);
        let b = parts.iter().map(|r| r.y + r.h).fold(ph, f32::max);
        let (ox, oy) = (M - l, M - t);
        let mv = |r: R| R { x: r.x + ox, y: r.y + oy, ..r };
        Layout {
            clock: clock.map(mv),
            rec: mv(rec),
            mascot: mv(mascot),
            gear: mv(gear),
            strip: strip.map(mv),
            music: music.map(mv),
            mixer: mv(mixer_btn),
            kbd: kbd.map(mv),
            vertical,
            pill: R { x: ox, y: oy, w: pw, h: ph },
            cells: cells.into_iter().map(|(i, r)| (i, mv(r))).collect(),
            panel: mv(panel),
            dir,
            win_w: r - l + M * 2.0,
            win_h: b - t + M * 2.0,
        }
    }

    /// Clamp the pill inside a work area; while dragging (`drag` = monitor under cursor) also snap to edges/centre.
    fn place_at(&mut self, x: i32, y: i32, drag: Option<RECT>) {
        let (w, h) = self.pill_size(self.cfg.vertical);
        let (w, h) = ((w * self.scale).round() as i32, (h * self.scale).round() as i32);
        // stay on the current monitor (or the one under the cursor while dragging), whatever the new size
        let anchor = self.cfg.pos.unwrap_or((x, y));
        let wa = drag.unwrap_or_else(|| work_area(POINT { x: anchor.0 + 20, y: anchor.1 + 20 }));
        let e = (EDGE * self.scale) as i32;
        let (l, r, t, b) = (wa.left + e, (wa.right - e - w).max(wa.left + e), wa.top + e, (wa.bottom - e - h).max(wa.top + e));
        let (mut x, mut y) = (x, y);
        if drag.is_some() {
            let s = (SNAP * self.scale) as i32;
            let (cx, cy) = ((wa.left + wa.right) / 2 - w / 2, (wa.top + wa.bottom) / 2 - h / 2);
            for (v, targets) in [(&mut x, [l, r, cx]), (&mut y, [t, b, cy])] {
                if let Some(&tv) = targets.iter().find(|&&tv| (*v - tv).abs() < s) {
                    *v = tv;
                }
            }
        }
        self.cfg.pos = Some((x.clamp(l, r), y.clamp(t, b)));
    }

    /// Named placements from the Position menu.
    fn place(&mut self, which: usize) {
        let wa = work_area(self.cfg.pos.map_or(POINT::default(), |(x, y)| POINT { x, y }));
        self.cfg.vertical = which >= 6 && self.cfg.side_layout && !self.cfg.card;
        let (w, h) = self.pill_size(self.cfg.vertical);
        let (w, h) = ((w * self.scale) as i32, (h * self.scale) as i32);
        let (cx, cy) = ((wa.left + wa.right - w) / 2, (wa.top + wa.bottom - h) / 2);
        let (l, r, t, b) = (wa.left, wa.right - w, wa.top, wa.bottom - h);
        let (x, y) = match which {
            1 => (l, t),
            2 => (r, t),
            3 => (cx, b),
            4 => (l, b),
            5 => (r, b),
            6 => (l, cy),
            7 => (r, cy),
            _ => (cx, t),
        };
        self.place_at(x, y, None); // clamp applies the edge gap
        self.cfg.save();
    }

    // ---------- messages ----------

    unsafe fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        unsafe {
            match msg {
                WM_APP_DATA => {
                    self.snap = self.shared.snap.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    self.refresh_mixer(); // apps come and go
                    // short notes ("highlight some text first", "key saved") clear themselves after 8 s
                    if self.job.as_ref().is_some_and(|j| j.note && j.at.elapsed().as_secs() >= 8) {
                        self.job = None;
                        self.after_rec_change();
                    }
                    // the card's list can grow (e.g. one row per drive): keep it on screen
                    if self.cfg.card && self.press.is_none() {
                        if let Some((x, y)) = self.cfg.pos {
                            self.place_at(x, y, None);
                        }
                    }
                    if self.want_visible {
                        if !self.menu_open {
                            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                            self.video_raise();
                        }
                        // Full animations: rings and bars glide to the new values (~0.3 s), then the timer stops.
                        // Reduced: one frame per data tick, the lowest possible CPU.
                        self.update_clock();
                        let full = self.cfg.anim == 0;
                        if !full {
                            self.disp = self.snap.frac;
                        }
                        for i in 0..5 {
                            let v = if i == NET { self.snap.net_rate as f32 } else { self.snap.frac[i] };
                            let h = &mut self.hist[i];
                            if h.len() == HIST {
                                h.pop_front();
                            }
                            h.push_back(v);
                        }
                        // new numbers: one full frame (refreshes the cached still picture), then the
                        // rings glide to them on cheap moving-only frames
                        self.render();
                        if full || self.sheen >= 0.0 {
                            self.kick();
                        }
                    }
                    let s = &self.snap;
                    let tip = format!("ResourcePill\nCPU {} · RAM {} · GPU {}\nDisk {} · Net ↓{} ↑{}", s.text[0], s.text[1], s.text[2], s.text[3], s.down, s.up);
                    self.tip(&tip);
                }
                WM_TIMER if wp.0 == TIMER_ANIM => {
                    let moving = self.step();
                    if self.fade > 0.0 {
                        if self.rings_only {
                            self.render_moving(); // just the rings gliding: the rest stays cached
                        } else {
                            self.render();
                        }
                    }
                    if !moving {
                        let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
                        self.anim_on = false;
                        if !self.want_visible {
                            let _ = ShowWindow(self.hwnd, SW_HIDE);
                        }
                    }
                }
                WM_HOTKEY if wp.0 as i32 == HOTKEY => self.toggle_tuck(), // tuck to the edge / bring back
                WM_HOTKEY if wp.0 as i32 == HOTKEY_REC => self.toggle_record(),
                WM_HOTKEY if wp.0 as i32 == HOTKEY_EXPLAIN => self.explain_now(), // highlight, then press
                WM_HOTKEY if wp.0 as i32 == HOTKEY_MIXER => self.toggle_mixer(),
                WM_HOTKEY if wp.0 as i32 == HOTKEY_VIDEO => self.video_hotkey(),
                WM_HOTKEY if wp.0 as i32 == HOTKEY_LISTS => self.video_lists(),
                WM_HOTKEY if wp.0 as i32 == HOTKEY_GHOST => self.video_set_ghost(!self.cfg.video_ghost), // the way out of ghost from anywhere
                video::WM_APP_VIDEO => self.video_inbox(),
                WM_TIMER if wp.0 == video::TIMER_VIDEO => self.video_tick(),
                WM_MOUSEWHEEL if self.settings.is_none() => {
                    let notches = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as f32 / 120.0;
                    // over the music cell or the player: volume
                    let mut c = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
                    let _ = ScreenToClient(self.hwnd, &mut c);
                    let (x, y) = (c.x as f32 / self.scale, c.y as f32 / self.scale);
                    let lay = self.layout();
                    if self.mixer.is_some() && lay.panel.contains(x, y) {
                        self.mixer_wheel(x, y, lay.panel, notches); // the row under the mouse
                    } else if lay.mixer.contains(x, y) {
                        // the speaker button: the PC's volume, mixer open or not
                        if let Some(m) = media::volume::master() {
                            let v = (m.GetMasterVolumeLevelScalar().unwrap_or(0.0) + notches * 0.05).clamp(0.0, 1.0);
                            let _ = m.SetMasterVolumeLevelScalar(v, std::ptr::null());
                            if let Some(mx) = self.mixer.as_mut() {
                                mx.rows[0].vol = v;
                            }
                            self.render();
                        }
                    } else if lay.music.is_some_and(|m| m.contains(x, y)) || (self.music_open && lay.panel.contains(x, y)) {
                        self.volume_step(notches);
                    }
                }
                WM_MOUSEWHEEL if self.settings.is_some() => {
                    let panel = self.layout().panel;
                    self.settings_wheel(((wp.0 >> 16) & 0xFFFF) as u16 as i16, panel);
                }
                WM_APP_JOB => {
                    let was_running = self.job.as_ref().is_some_and(|j| j.state == recorder::JobState::Running);
                    if self.job.as_mut().is_some_and(|j| j.pump()) {
                        let answer = self.job.as_ref().filter(|j| was_running && j.kind == recorder::JobKind::Explain && j.state == recorder::JobState::Done).map(|j| j.text.clone());
                        if let Some(a) = answer.filter(|_| self.cfg.voice) {
                            self.speak(&a);
                        }
                        self.after_rec_change();
                        self.setup_job_done(); // a setup download finished: on to the next
                        self.update_job_done();
                    }
                }
                WM_TIMER if wp.0 == TIMER_TIP => self.hide_tip(),
                explainui::WM_APP_SELECT if self.explain_mode => self.show_tip(explainui::unpack(lp)),
                explainui::WM_APP_DOWN => {
                    let (x, y) = explainui::unpack(lp);
                    let r = self.pop.as_ref().map(|p| p.rect()).unwrap_or_default();
                    if !(x >= r.left && x < r.right && y >= r.top && y < r.bottom) {
                        self.hide_tip(); // clicked elsewhere
                    }
                }
                explainui::WM_APP_POPCLICK => self.tip_click(explainui::unpack(lp).0),
                WM_TIMER if wp.0 == TIMER_EDGE => self.edge_tick(),
                // a device came or went (plug, unplug, Bluetooth): check the keyboard once things settle
                WM_DEVICECHANGE => {
                    SetTimer(Some(self.hwnd), TIMER_KBD, 700, None);
                    return None;
                }
                WM_TIMER if wp.0 == TIMER_KBD => {
                    let _ = KillTimer(Some(self.hwnd), TIMER_KBD);
                    self.check_keyboard(true);
                }
                WM_TIMER if wp.0 == TIMER_BLINK => {
                    // blink, then schedule the next one 2.5-7 s later
                    self.blink_at = Some(Instant::now());
                    let next = 2500 + (windows::Win32::System::SystemInformation::GetTickCount() % 4500);
                    SetTimer(Some(self.hwnd), TIMER_BLINK, next, None);
                    self.kick();
                }
                WM_APP_MEDIA => self.media_changed(),
                WM_APP_DUCK => self.duck_changed(),
                WM_APP_SCRIBE => {
                    let was_done = matches!(self.rec.state, recorder::State::Done);
                    if self.rec.pump() {
                        let saved = !was_done && matches!(self.rec.state, recorder::State::Done);
                        if saved {
                            self.cue(extras::Cue::Saved);
                        }
                        self.after_rec_change();
                        if saved && !self.use_gpu() {
                            self.ask_gpu();
                        }
                    }
                }
                WM_TIMER if wp.0 == TIMER_PULSE => {
                    if self.fade > 0.0 && !self.anim_on {
                        self.render_moving(); // only the small areas that move
                    }
                }
                WM_APP_SHOW => self.set_visible(true),
                WM_APP_DPI | WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
                    // monitor, DPI, resolution or taskbar changed: rescale and pull back on screen
                    self.update_scale();
                    if let Some((x, y)) = self.cfg.pos {
                        if on_screen(x, y) {
                            self.place_at(x, y, None);
                        } else {
                            self.place(0);
                        }
                    }
                    self.render();
                    return if msg == WM_SETTINGCHANGE { None } else { Some(LRESULT(0)) };
                }
                WM_LBUTTONDOWN if self.tucked => self.toggle_tuck(), // tap the edge line: back
                WM_LBUTTONDOWN if self.mixer.is_some() && { let (x, y) = self.dip(lp); self.mixer_slider_at(x, y, self.layout().panel).is_some() } => {
                    let (x, y) = self.dip(lp);
                    let panel = self.layout().panel;
                    self.mix_drag = self.mixer_slider_at(x, y, panel);
                    SetCapture(self.hwnd);
                    if let Some(i) = self.mix_drag {
                        self.mixer_drag(i, x, panel);
                    }
                }
                WM_MOUSEMOVE if self.mix_drag.is_some() => {
                    let (x, _) = self.dip(lp);
                    let panel = self.layout().panel;
                    if let Some(i) = self.mix_drag {
                        self.mixer_drag(i, x, panel);
                    }
                }
                WM_LBUTTONUP if self.mix_drag.is_some() => {
                    self.mix_drag = None;
                    let _ = ReleaseCapture();
                }
                WM_LBUTTONDOWN if self.music_open && { let (x, y) = self.dip(lp); self.volume_hit(x, y, self.layout().panel) } => {
                    self.vol_drag = true;
                    SetCapture(self.hwnd);
                    let (x, _) = self.dip(lp);
                    self.volume_at(x, self.layout().panel);
                }
                WM_MOUSEMOVE if self.vol_drag => {
                    let (x, _) = self.dip(lp);
                    self.volume_at(x, self.layout().panel);
                }
                WM_LBUTTONUP if self.vol_drag => {
                    self.vol_drag = false;
                    let _ = ReleaseCapture();
                }
                WM_LBUTTONDOWN => {
                    let mut c = POINT::default();
                    let _ = GetCursorPos(&mut c);
                    let lay = self.layout();
                    let (x, y) = self.dip(lp);
                    let p = lay.pill;
                    let frac = (((x - p.x) / p.w).clamp(0.0, 1.0), ((y - p.y) / p.h).clamp(0.0, 1.0));
                    self.press = Some((c, frac));
                    self.dragging = false;
                    SetCapture(self.hwnd);
                }
                WM_MOUSEMOVE => {
                    if !self.tracking {
                        let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
                        self.tracking = TrackMouseEvent(&mut t).is_ok();
                    }
                    if let Some((p0, frac)) = self.press {
                        let mut c = POINT::default();
                        let _ = GetCursorPos(&mut c);
                        if self.dragging || (c.x - p0.x).abs() + (c.y - p0.y).abs() > 4 {
                            self.dragging = true;
                            self.drag_to(c, frac);
                        }
                    } else {
                        let h = self.hit(lp);
                        let (x, y) = self.dip(lp);
                        let lay = self.layout();
                        let g = lay.gear.contains(x, y);
                        let sh = self.settings.is_some() && self.settings_hover_at(x, y, lay.panel);
                        if h != self.hover || g != self.gear_hover || sh {
                            self.hover = h;
                            self.gear_hover = g;
                            self.render();
                        }
                    }
                }
                WM_MOUSELEAVE => {
                    self.tracking = false;
                    let was_gear = std::mem::take(&mut self.gear_hover);
                    if self.hover != NONE || was_gear {
                        self.hover = NONE;
                        self.render();
                    }
                }
                WM_LBUTTONUP => {
                    let _ = ReleaseCapture();
                    if self.press.take().is_some() {
                        if self.dragging {
                            self.cfg.save();
                            // thrown against the screen edge: tuck it there
                            let mut c = POINT::default();
                            let _ = GetCursorPos(&mut c);
                            let m = monitor_rect(c);
                            let z = (6.0 * self.scale) as i32;
                            if c.y - m.top < z || m.bottom - 1 - c.y < z || c.x - m.left < z || m.right - 1 - c.x < z {
                                self.toggle_tuck();
                            }
                        } else {
                            let (x, y) = self.dip(lp);
                            let lay = self.layout();
                            if self.guide.is_some() && lay.panel.contains(x, y) {
                                self.guide_click(x, y, lay.panel);
                                self.dragging = false;
                                return Some(LRESULT(0));
                            }
                            if self.settings.is_some() && lay.panel.contains(x, y) {
                                self.settings_click(x, y, lay.panel);
                                self.dragging = false;
                                return Some(LRESULT(0));
                            } else if self.mixer.is_some() && lay.panel.contains(x, y) {
                                self.mixer_click(x, y, lay.panel);
                            } else if lay.mixer.contains(x, y) {
                                self.toggle_mixer();
                            } else if self.music_open && lay.panel.contains(x, y) {
                                self.music_panel_click(x, y, lay.panel);
                            } else if let Some(m) = lay.music.filter(|m| m.contains(x, y)) {
                                self.music_cell_click(x, y, m, lay.vertical);
                            } else if lay.gear.contains(x, y) {
                                let _ = PostMessageW(Some(self.hwnd), WM_APP_MENU, WPARAM(0), LPARAM(0));
                            } else if lay.mascot.contains(x, y) {
                                self.mascot_click();
                            } else if lay.rec.contains(x, y) {
                                self.toggle_record();
                            } else if let Some(s) = lay.strip.filter(|s| s.contains(x, y)) {
                                self.strip_click(x, y, s);
                            }
                            let h = self.hit(lp);
                            if h != NONE {
                                // the card always shows a top-apps list; the pill toggles its panel
                                self.select(if h == self.sel && !self.cfg.card { NONE } else { h });
                            }
                        }
                    }
                    self.dragging = false;
                }
                WM_LBUTTONDBLCLK => {
                    // not on the record button or transcript: those are plain buttons
                    let (x, y) = self.dip(lp);
                    let lay = self.layout();
                    let on_panel = (self.settings.is_some() || self.guide.is_some() || self.music_open || self.mixer.is_some()) && lay.panel.contains(x, y);
                    if !on_panel && !lay.music.is_some_and(|m| m.contains(x, y)) && !lay.rec.contains(x, y) && !lay.mascot.contains(x, y) && !lay.gear.contains(x, y) && !lay.strip.is_some_and(|s| s.contains(x, y)) {
                        self.set_card(!self.cfg.card);
                    }
                }
                WM_CAPTURECHANGED => {
                    self.vol_drag = false;
                    if self.press.take().is_some() && self.dragging {
                        self.cfg.save();
                    }
                    self.dragging = false;
                }
                _ => return None,
            }
            Some(LRESULT(0))
        }
    }

    /// Follow the cursor; near a left/right screen edge the pill turns vertical.
    fn drag_to(&mut self, c: POINT, frac: (f32, f32)) {
        let wa = work_area(c);
        let zone = (56.0 * self.scale) as i32;
        let vertical = !self.cfg.card && self.cfg.side_layout && (c.x - wa.left < zone || wa.right - c.x < zone);
        if vertical != self.cfg.vertical {
            self.cfg.vertical = vertical;
            self.sel = NONE; // panel would jump sides mid-drag
            self.open = 0.0;
            self.hl_a = 0.0;
            self.shared.selected.store(NONE, Ordering::Relaxed);
        }
        let (w, h) = self.pill_size(vertical);
        let (w, h) = ((w * self.scale) as i32, (h * self.scale) as i32);
        // keep the pill's centre on the monitor under the cursor
        let x = (c.x - (frac.0 * w as f32) as i32).clamp(wa.left - w / 2 + 1, wa.right - w / 2 - 1);
        let y = (c.y - (frac.1 * h as f32) as i32).clamp(wa.top - h / 2 + 1, wa.bottom - h / 2 - 1);
        self.place_at(x, y, Some(wa));
        unsafe { self.render() };
        unsafe { let _ = PostMessageW(Some(self.hwnd), WM_APP_DPI, WPARAM(0), LPARAM(0)); } // monitor may have changed
    }

    fn dip(&self, lp: LPARAM) -> (f32, f32) {
        let x = (lp.0 & 0xFFFF) as i16 as f32 / self.scale;
        let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / self.scale;
        (x, y)
    }

    fn hit(&self, lp: LPARAM) -> usize {
        let (x, y) = self.dip(lp);
        self.layout().cells.iter().find(|c| c.1.contains(x, y)).map_or(NONE, |c| c.0)
    }

    /// Record button / Ctrl+Alt+R: start, or stop (the helper then saves and summarises).
    fn toggle_record(&mut self) {
        use recorder::State;
        // not ready yet (models still downloading or missing): record the moment it is
        if matches!(self.rec.state, State::Idle | State::Done | State::Failed) && !self.recording_ready() {
            self.record_when_ready();
            return;
        }
        match self.rec.state {
            State::Recording => {
                self.rec.stop();
                self.cue(extras::Cue::RecStop);
            }
            State::Finishing => {} // already wrapping up
            _ => {
                let asr = recorder::ASR[self.cfg.asr.min(recorder::ASR.len() - 1)].0;
                let llm = recorder::LLM.get(self.cfg.llm).map(|(d, f, _)| recorder::models_dir().join(d).join(f)).filter(|p| p.exists());
                let h = self.hwnd.0 as isize;
                let notify = move || unsafe {
                    let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_SCRIBE, WPARAM(0), LPARAM(0));
                };
                let final_asr = if self.cfg.final_pass { recorder::final_model() } else { None };
                let gemini = if self.cfg.tx_cloud { extras::load_key("gemini") } else { None };
                let gpu = self.use_gpu() && setup::gpu_download_mb() == 0;
                match self.rec.start(asr, llm, final_asr, self.cfg.keep_audio, gpu, gemini, notify) {
                    Ok(_) => self.cue(extras::Cue::RecStart),
                    Err(e) => {
                        self.rec.state = State::Failed;
                        self.rec.status = e;
                    }
                }
            }
        }
        self.after_rec_change();
    }

    /// The GPU is chosen for the accurate pass. Never in admin mode: the GPU engine's DLLs live in
    /// the user-writable models folder, which an elevated process must not load.
    fn use_gpu(&self) -> bool {
        self.cfg.gpu && !admin::is_elevated()
    }

    /// Once, after the first recording improved on the CPU: offer the NVIDIA GPU instead.
    fn ask_gpu(&mut self) {
        if self.cfg.gpu_asked || !self.cfg.final_pass || self.cfg.tx_cloud || admin::is_elevated() || !recorder::nvidia_gpu() {
            return;
        }
        self.cfg.gpu_asked = true;
        self.cfg.save();
        let mb = setup::gpu_download_mb();
        let download = if mb > 0 { format!("It downloads the GPU speech engine once ({mb} MB).") } else { "The GPU speech engine is already downloaded.".into() };
        let text = format!(
            "Your transcript is saved. Peter is now making it more accurate on the CPU, which can take a while.\n\nUse your NVIDIA graphics card for this from now on (about 3× faster)?\n{download}\n\nYou can change this any time in Settings → Transcription."
        );
        if unsafe { MessageBoxW(Some(self.hwnd), &HSTRING::from(text), w!("Use your GPU for transcription?"), MB_YESNO | MB_ICONQUESTION) } == IDYES {
            self.command(1207);
        }
    }

    fn cue(&self, c: extras::Cue) {
        if self.cfg.cues {
            extras::play_cue(c);
        }
    }

    /// Milliseconds per frame for the chosen frame rate (Appearance → Frame rate).
    fn frame_ms(&self) -> u32 {
        1000 / FPS[self.cfg.fps.min(FPS.len() - 1)]
    }

    /// Smooth frames (at the chosen frame rate) only while something on screen moves: work in progress (pulse,
    /// lively mascot), music (dancing bars, progress, the shimmering edge line) or Peter's sparkle.
    /// Nothing moving (or the pill hidden): no timer at all.
    pub fn sync_pulse(&self) {
        // "Reduced" animations: only work in progress animates (as before); music and the sparkle hold still
        let ambient = self.cfg.anim == 0 && (self.now.playing || (!self.tucked && (self.cloud_ai() || self.tint_a > 0.01)));
        let ducking = self.duck.ducking.load(Ordering::Relaxed); // the auto-lower badge breathes
        let spotlight = self.guide == Some(setup::RECORDINGS_STEP); // the gear pulses while the guide points at it
        let moving = self.working() || ambient || spotlight || (ducking && !self.tucked && self.cfg.anim == 0);
        unsafe {
            if moving && self.want_visible {
                SetTimer(Some(self.hwnd), TIMER_PULSE, self.frame_ms(), None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_PULSE);
            }
        }
    }

    /// Something is working: recording, explaining, downloading, or waiting for a highlight.
    fn working(&self) -> bool {
        self.rec.busy() || self.job.as_ref().is_some_and(|j| j.state == recorder::JobState::Running)
    }

    /// Keep timers, size and position in step with the recorder, jobs and explain mode.
    fn after_rec_change(&mut self) {
        self.sync_pulse();
        unsafe {
            if let Some((x, y)) = self.cfg.pos {
                self.place_at(x, y, None); // the strip changes the footprint
            }
            self.render();
        }
    }

    /// Strip height: the transcript is fixed; an explanation grows with its text.
    fn strip_h(&self, w: f32) -> f32 {
        let Some(job) = &self.job else { return if self.strip_expanded { 32.0 + STRIP_LINES_BIG as f32 * 21.0 + 10.0 } else { STRIP_H } };
        if job.kind == recorder::JobKind::Download {
            return 76.0;
        }
        let text: Vec<u16> = job.text.encode_utf16().collect();
        let h = unsafe {
            self.dw
                .CreateTextLayout(&text, &self.f.wrap, w - 28.0, 1000.0)
                .ok()
                .and_then(|l| {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    l.GetMetrics(&mut m).ok().map(|_| m.height)
                })
                .unwrap_or(40.0)
        };
        (58.0 + h + 12.0).clamp(84.0, 320.0)
    }

    fn job_notify(&self) -> impl Fn() + Send + 'static {
        let h = self.hwnd.0 as isize;
        move || unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_JOB, WPARAM(0), LPARAM(0));
        }
    }

    /// Mascot click: explain mode on/off. While on, cursors glow and each highlight gets a tooltip.
    fn mascot_click(&mut self) {
        if self.explain_mode {
            self.end_explain_mode();
        } else {
            self.explain_mode = true;
            explainui::cursors_on(self.theme().accent[0]);
            explainui::hook_on(self.hwnd);
        }
        self.after_rec_change();
    }

    fn end_explain_mode(&mut self) {
        if std::mem::take(&mut self.explain_mode) {
            explainui::hook_off();
            explainui::cursors_off();
        }
        self.hide_tip();
    }

    fn hide_tip(&mut self) {
        if let Some(p) = &self.pop {
            p.hide();
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_TIP);
        }
    }

    /// The little "✦ Explain | ⌕ Search" tooltip, just below where the highlight ended.
    fn show_tip(&mut self, (x, y): (i32, i32)) {
        if self.pop.is_none() {
            self.pop = explainui::Pop::create();
        }
        let s = self.scale;
        let (w, h) = ((TIP_W * s).ceil() as i32, (TIP_H * s).ceil() as i32);
        let Some(pop) = self.pop.as_mut() else { return };
        if !pop.ensure(w, h) {
            return;
        }
        let memdc = pop.memdc;
        let th = self.theme();
        unsafe {
            self.rt.SetDpi(96.0 * s, 96.0 * s);
            if self.rt.BindDC(memdc, &RECT { left: 0, top: 0, right: w, bottom: h }).is_err() {
                return;
            }
            self.rt.BeginDraw();
            self.rt.Clear(Some(&color(0, 0.0)));
            self.alpha = 1.0;
            let r = R { x: 1.0, y: 1.0, w: TIP_W - 2.0, h: TIP_H - 2.0 };
            let bg = if th.glass { 0x1C1C22 } else { th.bg };
            self.rt.FillRoundedRectangle(&self.rrect(r, r.h / 2.0), self.color(bg, 0.96));
            self.rt.DrawRoundedRectangle(&self.rrect(r, r.h / 2.0), self.color(th.accent[0], 0.9), 1.4, None);
            let half = TIP_W / 2.0;
            self.rt.DrawLine(pt(half, 8.0), pt(half, TIP_H - 8.0), self.color(th.fg, 0.25), 1.0, None);
            let fg = if th.glass { 0xFFFFFF } else { th.fg };
            self.text("✦ Explain", &self.f.c_value, R { x: 0.0, y: 0.0, w: half, h: TIP_H }, fg, 1.0);
            self.text("⌕ Search", &self.f.c_value, R { x: half, y: 0.0, w: half, h: TIP_H }, fg, 1.0);
            let _ = self.rt.EndDraw(None, None);
            // below the end of the highlight, kept on its monitor
            let m = work_area(POINT { x, y });
            let at = POINT { x: (x - w / 2).clamp(m.left, m.right - w), y: if y + 18 + h < m.bottom { y + 18 } else { y - 18 - h } };
            if let Some(pop) = &self.pop {
                pop.present(at);
            }
            self.cue(extras::Cue::Tip);
            SetTimer(Some(self.hwnd), TIMER_TIP, 6000, None);
        }
        unsafe { self.render() }; // the shared render target goes back to the pill
    }

    /// Tooltip click: left half explains, right half searches the web.
    fn tip_click(&mut self, x: i32) {
        let w = self.pop.as_ref().map_or(0, |p| p.size.0);
        self.hide_tip();
        if x < w / 2 {
            self.explain_now();
        } else if let Some(text) = extras::grab_selection(self.hwnd) {
            let q: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(300).collect();
            let url = format!("https://www.google.com/search?q={}", explainui::url_encode(&q));
            unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL) };
        }
    }

    /// A cloud AI is chosen and its key is stored (Peter looks different then).
    fn cloud_ai(&self) -> bool {
        (self.cfg.explain == 1 && extras::has_key("anthropic")) || ((self.cfg.explain == 2 || self.cfg.tx_cloud) && extras::has_key("gemini"))
    }

    /// Peter's voice: Windows' built-in text-to-speech (free, offline), replacing anything still being said.
    fn speak(&mut self, text: &str) {
        use windows::Win32::Media::Speech::*;
        if self.voice.is_none() {
            self.voice = unsafe { windows::Win32::System::Com::CoCreateInstance(&SpVoice, None, windows::Win32::System::Com::CLSCTX_ALL) }.ok();
        }
        if let Some(v) = &self.voice {
            let flags = (SPF_ASYNC.0 | SPF_PURGEBEFORESPEAK.0 | if text.is_empty() { 0 } else { SPF_IS_NOT_XML.0 }) as u32;
            unsafe {
                let _ = v.Speak(&HSTRING::from(text), flags, None);
            }
        }
    }

    fn hush(&mut self) {
        if self.voice.is_some() {
            self.speak("");
        }
    }

    /// Grab the highlighted text from whichever app is in front and explain it.
    fn explain_now(&mut self) {
        self.hush();
        let text = extras::grab_selection(self.hwnd);
        let fail = |a: &mut App, msg: &str| {
            a.job = Some(recorder::Job::note(recorder::JobKind::Explain, "Explain", msg));
            a.after_rec_change();
        };
        let Some(text) = text else {
            fail(self, "Highlight some text first, then click the helper (or press Ctrl+Alt+E).");
            return;
        };
        let text: String = text.chars().take(4000).collect(); // a highlight, not a document
        let cloud = match self.cfg.explain {
            1 => extras::load_key("anthropic").map(|k| ("claude", k, "Explain · Claude")),
            2 => extras::load_key("gemini").map(|k| ("gemini", k, "Explain · Gemini")),
            _ => None,
        };
        if cloud.is_none() && self.cfg.explain != 0 {
            let who = if self.cfg.explain == 2 { "Gemini" } else { "Claude" };
            fail(self, &format!("{who} is selected but no {who} key is stored. Copy your key, then Settings → AI & API keys → Paste API key."));
            return;
        }
        let (how, input, title) = match cloud {
            Some((how, k, title)) => (how, format!("{k}\n{text}"), title),
            None => ("local", format!("\n{text}"), "Explain · on this PC"),
        };
        let mut args = vec!["--explain".to_string(), how.to_string()];
        if how == "local" {
            let Some(gguf) = recorder::LLM.iter().map(|(d, f, _)| recorder::models_dir().join(d).join(f)).find(|p| p.exists()) else {
                fail(self, "No local text model installed. Add one from Model library → Text.");
                return;
            };
            args.extend(["--llm".to_string(), gguf.to_string_lossy().into_owned()]);
        }
        let notify = self.job_notify();
        self.job = match recorder::Job::start(recorder::JobKind::Explain, title.into(), text, &args, Some(input), notify) {
            Ok(j) => Some(j),
            Err(e) => {
                fail(self, &e);
                return;
            }
        };
        self.after_rec_change();
    }

    fn start_download(&mut self, id: &'static str) {
        let Some(m) = catalog::find(id) else { return };
        if self.job.as_ref().is_some_and(|j| j.state == recorder::JobState::Running) {
            return; // one at a time
        }
        let notify = self.job_notify();
        let args = vec!["--download".to_string(), id.to_string()];
        self.job = recorder::Job::start(recorder::JobKind::Download, format!("{} · {} MB", m.name, m.size_mb), String::new(), &args, None, notify).ok();
        self.after_rec_change();
    }

    /// "New update available": the helper downloads it in the strip, then `update_job_done` installs it.
    fn start_update(&mut self) {
        let Some(r) = update::available() else { return };
        if !update::safe_to_replace() {
            // elevated, from a folder the user can write to: let them download it by hand
            unsafe { ShellExecuteW(None, w!("open"), w!("https://github.com/Clyde369/ResourcePill/releases/latest"), None, None, SW_SHOWNORMAL) };
            self.job = Some(recorder::Job::note(recorder::JobKind::Download, "Update", &format!("Update available ({}) · download it from the releases page", r.tag)));
        } else if self.rec.busy() {
            self.job = Some(recorder::Job::note(recorder::JobKind::Download, "Update", "Stop the recording first, then update"));
        } else if self.job.as_ref().is_some_and(|j| j.state == recorder::JobState::Running) {
            return; // one at a time
        } else if let Some(dest) = update::download_path() {
            let _ = std::fs::remove_file(&dest); // a fresh download, never a resumed leftover
            let args = vec!["--update-get".to_string(), r.url.clone(), dest.display().to_string()];
            self.job = recorder::Job::start(recorder::JobKind::Download, format!("Updating to {}", r.tag), String::new(), &args, None, self.job_notify()).ok();
            self.updating = Some(r);
        }
        self.after_rec_change();
    }

    /// The update download finished: check and install it, then hand over to the new exe.
    fn update_job_done(&mut self) {
        let Some(job) = self.job.as_ref().filter(|j| j.state != recorder::JobState::Running && j.title.starts_with("Updating to ")) else { return };
        let Some(r) = self.updating.take() else { return };
        let msg = if job.state == recorder::JobState::Failed {
            job.text.clone()
        } else if self.rec.busy() {
            "Stop the recording first, then update".into()
        } else {
            match update::install(&r) {
                Ok(()) => return unsafe { self.quit() },
                Err(e) => e,
            }
        };
        self.job = Some(recorder::Job::note(recorder::JobKind::Download, "Update", &msg));
        self.after_rec_change();
    }

    /// Strip clicks: × dismisses; after a recording, clicking opens the summary (or transcript).
    fn strip_click(&mut self, x: f32, y: f32, s: R) {
        if self.job.is_some() {
            let close = x > s.x + s.w - 30.0 && y < s.y + 28.0;
            if close || self.job.as_ref().is_some_and(|j| j.state != recorder::JobState::Running) {
                self.job = None; // closing a running download cancels it; it resumes next time
                self.hush();
                self.after_rec_change();
            }
            return;
        }
        let close = x > s.x + s.w - 30.0 && y < s.y + 28.0;
        if self.rec.busy() && !close {
            self.strip_expanded = !self.strip_expanded; // see more (or less) of the live transcript
            self.after_rec_change();
            return;
        }
        if !close {
            // the Recordings window: transcript, summary and audio in one place
            recorder::open_viewer(self.rec.folder.as_deref());
        }
        if !self.rec.busy() {
            self.rec.state = recorder::State::Idle;
            self.after_rec_change();
        }
    }

    fn select(&mut self, i: usize) {
        if i != NONE {
            self.settings = None; // details, settings and the player share the panel
            self.music_open = false;
            self.mixer = None;
        }
        if i != NONE {
            if self.hl_a < 0.05 {
                if let Some(c) = self.layout().cells.iter().find(|c| c.0 == i) {
                    self.hl = [c.1.x, c.1.y, c.1.w, c.1.h]; // appear in place, slide afterwards
                }
            }
            self.panel_sel = i;
        }
        self.sel = i;
        self.shared.selected.store(i, Ordering::Relaxed);
        if let Some(w) = &self.wake {
            let _ = w.send(());
        }
        self.kick();
    }

    /// Switch between the pill and the larger card, keeping the window's centre where it was.
    fn set_card(&mut self, card: bool) {
        let ow = self.pill_size(self.cfg.vertical).0;
        self.cfg.card = card;
        if card {
            self.cfg.vertical = false;
        }
        let nw = self.pill_size(self.cfg.vertical).0;
        let (x, y) = self.cfg.pos.unwrap_or((0, 0));
        let dx = ((ow - nw) * self.scale / 2.0) as i32;
        // top edge stays put; re-centre horizontally
        self.place_at(x + dx, y, None);
        self.hl_a = 0.0;
        self.open = 0.0;
        let first = self.shown().first().copied().unwrap_or(0);
        self.select(if card { if self.sel != NONE { self.sel } else { first } } else { NONE });
        self.fade = 0.35; // quick fade-in reads as a morph
        self.cfg.save();
        self.kick();
    }

    fn set_visible(&mut self, v: bool) {
        self.want_visible = v;
        self.shared.visible.store(v, Ordering::Relaxed);
        unsafe {
            if v {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                if let Some(w) = &self.wake {
                    let _ = w.send(());
                }
            }
        }
        self.kick();
        self.sync_pulse();
    }

    /// Start the ~60 fps animation timer; it stops itself once everything settles.
    fn kick(&mut self) {
        if !self.anim_on {
            self.last_frame = Instant::now();
            self.anim_on = unsafe { SetTimer(Some(self.hwnd), TIMER_ANIM, self.frame_ms(), None) } != 0;
            if !self.anim_on {
                self.step();
                unsafe { self.render() };
            }
        }
    }

    fn step(&mut self) -> bool {
        let dt = self.last_frame.elapsed().as_secs_f32().min(0.05);
        self.last_frame = Instant::now();
        // the rings glide to new readings on their own (they are moving pieces: cheap to redraw)
        let mut rings = false;
        for i in 0..5 {
            let d = self.snap.frac[i] - self.disp[i];
            if d.abs() < 0.003 {
                self.disp[i] = self.snap.frac[i];
            } else {
                self.disp[i] += d * (1.0 - (-dt * 16.0).exp());
                rings = true;
            }
        }
        let mut moving = false;
        let mut go = |v: &mut f32, t: f32, speed: f32, eps: f32| {
            let d = t - *v;
            if d.abs() < eps {
                *v = t;
            } else {
                *v += d * (1.0 - (-dt * speed).exp());
                moving = true;
            }
        };
        let open = self.sel != NONE || self.settings.is_some() || self.guide.is_some() || self.music_open || self.mixer.is_some();
        go(&mut self.open, if open { 1.0 } else { 0.0 }, 14.0, 0.003);
        go(&mut self.hl_a, if self.sel != NONE { 1.0 } else { 0.0 }, 14.0, 0.003);
        go(&mut self.nav_t, 1.0, 16.0, 0.003);
        go(&mut self.tuck, if self.tucked { 1.0 } else { 0.0 }, 9.0, 0.003);
        // song-cover colours glide to the new song's in about a second
        let tint_to = self.tint_to;
        for (k, v) in self.tint.iter_mut().enumerate() {
            go(v, tint_to[k], 3.5, 0.5);
        }
        go(&mut self.tint_a, self.tint_a_to, 3.5, 0.003);
        if open {
            if let Some(c) = self.layout().cells.iter().find(|c| c.0 == self.sel) {
                let t = [c.1.x, c.1.y, c.1.w, c.1.h];
                for k in 0..4 {
                    go(&mut self.hl[k], t[k], 18.0, 0.05);
                }
            }
        }
        go(&mut self.fade, if self.want_visible { 1.0 } else { 0.0 }, 12.0, 0.003);
        if self.blink_at.is_some_and(|t| t.elapsed().as_secs_f32() < 0.24) {
            moving = true; // keep frames coming for the blink
        }
        if self.sheen >= 0.0 {
            self.sheen += dt / 1.6; // 1.6 s sweep
            if self.sheen >= 1.0 {
                self.sheen = -1.0;
            }
            moving = true;
        }
        self.rings_only = rings && !moving; // only the rings glide: moving-only frames will do
        moving || rings
    }

    // ---------- drawing ----------

    fn color(&self, rgb: u32, a: f32) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&color(rgb, a * self.alpha)) };
        &self.brush
    }

    fn text(&self, s: &str, f: &IDWriteTextFormat, r: R, rgb: u32, a: f32) {
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            // on glass, a faint shadow keeps text readable over any background
            if self.theme().glass && rgb != 0 {
                let dark = self.theme().fg > 0x808080;
                let sh = R { x: r.x, y: r.y + 0.8, ..r };
                self.rt.DrawText(&w, f, &sh.d2d(), self.color(if dark { 0 } else { 0xFFFFFF }, a * 0.45), D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            }
            self.rt.DrawText(&w, f, &r.d2d(), self.color(rgb, a), D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    fn rrect(&self, r: R, rad: f32) -> D2D1_ROUNDED_RECT {
        let rad = rad.min(r.w / 2.0).min(r.h / 2.0);
        D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: rad, radiusY: rad }
    }

    unsafe fn ensure_surface(&mut self, pw: i32, ph: i32) -> bool {
        if pw == self.pw && ph == self.ph && !self.bmp.is_invalid() {
            return true;
        }
        unsafe {
            if !self.bmp.is_invalid() {
                SelectObject(self.memdc, self.old);
                let _ = DeleteObject(self.bmp.into());
                self.bmp = HBITMAP::default();
            }
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: pw,
                    biHeight: -ph,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let Ok(bmp) = CreateDIBSection(Some(self.memdc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) else { return false };
            self.bmp = bmp;
            self.old = SelectObject(self.memdc, bmp.into());
            self.pw = pw;
            self.ph = ph;
        }
        true
    }

    /// Draw everything.
    unsafe fn render(&mut self) {
        unsafe { self.render_frame(false) };
    }

    /// Gentle ongoing animations (music bars and progress, Peter, the auto-lower badge): reuse the
    /// cached still picture and paint only the moving pieces on top. Painting the whole pill in
    /// software costs far more than these few shapes, so this keeps 60 fps light.
    unsafe fn render_moving(&mut self) {
        unsafe { self.render_frame(true) };
    }

    unsafe fn render_frame(&mut self, moving_only: bool) {
        let t0 = Instant::now();
        let partial = unsafe { self.render_frame_inner(moving_only) };
        profile(partial, t0.elapsed().as_secs_f64() * 1000.0);
    }

    /// A frame, in one of three ways (returns true when only the moving pieces were painted):
    /// - nothing else animating: the still layer (moving pieces only recorded) is painted and
    ///   cached, then the moving pieces go on top;
    /// - `moving_only` with a cached still layer: copy it back, paint the moving pieces;
    /// - otherwise (panels opening, tucking, recording, the card view): everything at once.
    unsafe fn render_frame_inner(&mut self, moving_only: bool) -> bool {
        unsafe {
            let lay = self.layout();
            let (pw, ph) = ((lay.win_w * self.scale).ceil() as i32, (lay.win_h * self.scale).ceil() as i32);
            if !self.ensure_surface(pw, ph) {
                return false;
            }
            let layered = !((self.anim_on && !self.rings_only) || self.working() || self.tuck > 0.001 || self.cfg.card);
            let cached = self.content.as_ref().is_some_and(|c| (c.1, c.2) == (pw, ph)) && self.back.is_some_and(|s| (s.3, s.4) == (pw, ph));
            let moving_only = moving_only && layered && cached;
            self.rt.SetDpi(96.0 * self.scale, 96.0 * self.scale);
            let rc = RECT { left: 0, top: 0, right: pw, bottom: ph };
            if self.rt.BindDC(self.memdc, &rc).is_err() {
                return false;
            }
            if layered && !moving_only {
                // paint and cache both layers: the surfaces (background), then everything on them
                // (content, on a clear background so it can go over the aurora)
                Self::ensure_layer(&mut self.back, pw, ph);
                self.moving.borrow_mut().clear();
                self.auroras.borrow_mut().clear();
                for (pass, layer) in [(3u8, self.back.map(|l| l.0)), (1u8, None)] {
                    self.rt.BeginDraw();
                    self.rt.Clear(Some(&color(0, 0.0)));
                    self.pass = pass;
                    if pass == 3 {
                        self.draw_backgrounds(&lay);
                    } else {
                        self.draw_scene(&lay);
                    }
                    self.pass = 0;
                    if self.rt.EndDraw(None, None).is_err() {
                        return false;
                    }
                    if let Some(dc) = layer {
                        let _ = BitBlt(dc, 0, 0, pw, ph, Some(self.memdc), 0, 0, SRCCOPY);
                    }
                }
                // the content layer as a ready-to-draw image (one drawing pass per frame from here)
                self.content = self.snapshot_bitmap(pw, ph).map(|b| (b, pw, ph));
            }
            if layered {
                // compose: background → drifting aurora → content → moving pieces
                if let Some((back, ..)) = self.back {
                    let _ = BitBlt(self.memdc, 0, 0, pw, ph, Some(back), 0, 0, SRCCOPY);
                }
                self.rt.BeginDraw();
                for (r, rad) in self.auroras.borrow().clone() {
                    self.draw_aurora(r, rad);
                }
                if let Some((b, ..)) = &self.content {
                    let full = D2D_RECT_F { left: 0.0, top: 0.0, right: pw as f32 / self.scale, bottom: ph as f32 / self.scale };
                    self.rt.DrawBitmap(b, Some(&full), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
                }
                self.draw_moving();
            } else {
                self.drop_layers(); // out of date now; made again when things settle
                self.rt.BeginDraw();
                self.rt.Clear(Some(&color(0, 0.0)));
                self.draw_scene(&lay);
            }
            if self.rt.EndDraw(None, None).is_err() {
                return false;
            }
            self.present(&lay, pw, ph);
            self.video_follow(&lay);
            moving_only
        }
    }

    /// The background layer: every visible surface (pill, strip, open panel), nothing on them.
    unsafe fn draw_backgrounds(&mut self, lay: &Layout) {
        unsafe {
            self.alpha = 1.0;
            let p = lay.pill;
            self.surface(p, if lay.vertical { p.w / 2.0 } else { p.h / 2.0 });
            if let Some(s) = lay.strip {
                self.surface(s, 16.0);
            }
            let metric_panel = self.panel_sel != NONE && self.cfg.shown[self.panel_sel];
            if self.open > 0.003 && (self.settings.is_some() || self.guide.is_some() || self.music_open || self.mixer.is_some() || metric_panel) {
                self.surface(lay.panel, 18.0);
                if self.settings.is_some() || self.guide.is_some() {
                    self.rt.FillRoundedRectangle(&self.rrect(lay.panel, 18.0), self.color(self.theme().bg, 0.94));
                }
            }
        }
    }

    /// Record a moving piece during the still pass (true: recorded, so don't draw it now).
    fn record(&self, m: Moving) -> bool {
        if self.pass == 1 {
            self.moving.borrow_mut().push((m, self.alpha));
        }
        self.pass == 1
    }

    /// Paint the recorded moving pieces where the still pass left them.
    unsafe fn draw_moving(&mut self) {
        let list = self.moving.borrow().clone();
        self.pass = 2;
        for (m, a) in list {
            self.alpha = a;
            match m {
                Moving::Eq(x, base, h) => self.draw_eq(x, base, h),
                Moving::Progress(r) => self.draw_progress(r),
                Moving::Mascot(r, pill) => self.draw_mascot(r, pill),
                Moving::Badge(r) => self.draw_mixer_button(r),
                Moving::Ring(i, x, y, rad) => self.ring(i, x, y, rad),
                Moving::Spotlight(r) => self.draw_spotlight(r),
            }
        }
        self.alpha = 1.0;
        self.pass = 0;
    }

    /// A cached layer: a second bitmap the size of the window.
    unsafe fn ensure_layer(slot: &mut Option<(HDC, HBITMAP, HGDIOBJ, i32, i32)>, pw: i32, ph: i32) {
        if slot.is_some_and(|s| (s.3, s.4) == (pw, ph)) {
            return;
        }
        unsafe {
            Self::free_layer(slot);
            let dc = CreateCompatibleDC(None);
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: pw, biHeight: -ph, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            match CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) => {
                    let old = SelectObject(dc, b.into());
                    *slot = Some((dc, b, old, pw, ph));
                }
                Err(_) => {
                    let _ = DeleteDC(dc);
                }
            }
        }
    }

    unsafe fn free_layer(slot: &mut Option<(HDC, HBITMAP, HGDIOBJ, i32, i32)>) {
        if let Some((dc, b, old, ..)) = slot.take() {
            unsafe {
                SelectObject(dc, old);
                let _ = DeleteObject(b.into());
                let _ = DeleteDC(dc);
            }
        }
    }

    unsafe fn drop_layers(&mut self) {
        unsafe {
            self.content = None;
            Self::free_layer(&mut self.back);
        }
    }

    /// What was just painted into the window bitmap, as an image the renderer can draw again.
    unsafe fn snapshot_bitmap(&self, pw: i32, ph: i32) -> Option<ID2D1Bitmap> {
        unsafe {
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: pw, biHeight: -ph, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                ..Default::default()
            };
            let mut px = vec![0u8; (pw * ph * 4) as usize];
            GetDIBits(self.memdc, self.bmp, 0, ph as u32, Some(px.as_mut_ptr() as *mut _), &bi as *const _ as *mut _, DIB_RGB_COLORS);
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0 * self.scale,
                dpiY: 96.0 * self.scale,
            };
            self.rt.CreateBitmap(D2D_SIZE_U { width: pw as u32, height: ph as u32 }, Some(px.as_ptr() as *const _), (pw * 4) as u32, &props).ok()
        }
    }

    /// The whole picture: pill (or card), strip, open panel, and the edge line when tucked.
    unsafe fn draw_scene(&mut self, lay: &Layout) {
        unsafe {
            self.alpha = 1.0 - self.tuck; // the pill fades as it tucks away, leaving the edge line
            if self.alpha > 0.005 {
                // fully tucked: only the edge line is visible, so skip drawing the rest
                if self.cfg.card {
                    self.draw_card(lay);
                } else {
                    self.draw_pill(lay);
                }
                if let Some(s) = lay.strip {
                    if self.job.is_some() {
                        self.draw_job_strip(s);
                    } else {
                        self.draw_strip(s);
                    }
                }
                let metric_panel = !self.cfg.card && self.panel_sel != NONE && self.cfg.shown[self.panel_sel];
                if self.open > 0.003 && (self.settings.is_some() || self.guide.is_some() || self.music_open || self.mixer.is_some() || metric_panel) {
                    self.draw_panel(lay);
                }
            }
            self.alpha = 1.0;
            self.draw_edge_line(lay);
        }
    }

    /// Hand the finished frame to Windows, at the pill's place (slid away when tucked).
    unsafe fn present(&self, lay: &Layout, pw: i32, ph: i32) {
        unsafe {
            // window position derives from the pill position so the pill never moves when the panel flips sides
            let (px, py) = self.cfg.pos.unwrap_or((0, 0));
            let (sx, sy) = self.tuck_shift(lay); // edge tuck slides the whole window
            let dst = POINT { x: px - (lay.pill.x * self.scale).round() as i32 + (sx as f32 * self.tuck) as i32, y: py - (lay.pill.y * self.scale).round() as i32 + (sy as f32 * self.tuck) as i32 };
            let size = SIZE { cx: pw, cy: ph };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                SourceConstantAlpha: (self.fade.clamp(0.0, 1.0) * 255.0) as u8,
                AlphaFormat: AC_SRC_ALPHA as u8,
                ..Default::default()
            };
            let _ = UpdateLayeredWindow(self.hwnd, None, Some(&dst), Some(&size), Some(self.memdc), Some(&POINT::default()), COLORREF(0), Some(&blend), ULW_ALPHA);
        }
    }


    /// A rounded surface: shadow, fill, cover colours (the background layer, pass 3), then its
    /// edge, glass shine and hourly sheen (the content layer, pass 1). Pass 0 draws all of it.
    unsafe fn surface(&self, r: R, rad: f32) {
        let th = self.theme();
        unsafe {
            if self.pass != 1 {
                // soft shadow
                for i in 0..7 {
                    let g = i as f32 * 1.6;
                    let s = R { x: r.x - g, y: r.y - g + 3.0, w: r.w + g * 2.0, h: r.h + g * 2.0 };
                    self.rt.FillRoundedRectangle(&self.rrect(s, rad + g), self.color(0, th.shadow_a * (1.0 - i as f32 / 7.0)));
                }
                self.rt.FillRoundedRectangle(&self.rrect(r, rad), self.color(th.bg, th.bg_a * OPACITY[self.cfg.opacity].1));
                // the song cover's colours, as a soft gradient across the surface
                if self.tint_a > 0.01 {
                    let c = |k: usize| ((self.tint[k] as u32) << 16) | ((self.tint[k + 1] as u32) << 8) | self.tint[k + 2] as u32;
                    if let Some(b) = self.lin(r.x, r.y, r.x + r.w, r.y + r.h, &[(0.0, c(0), 0.44 * self.tint_a), (0.55, mix(c(0), c(3), 0.5), 0.32 * self.tint_a), (1.0, c(3), 0.42 * self.tint_a)]) {
                        self.rt.FillRoundedRectangle(&self.rrect(r, rad), &b);
                    }
                    // the aurora drifts over the cover colours, always under the text and art: drawn
                    // here in a full frame, or between the cached layers each moving frame
                    if self.cfg.anim == 0 {
                        if self.pass == 3 {
                            self.auroras.borrow_mut().push((r, rad));
                        } else {
                            self.draw_aurora(r, rad);
                        }
                    }
                }
            }
            if self.pass == 3 {
                return; // the background layer stops here
            }
            let e = R { x: r.x + 0.5, y: r.y + 0.5, w: r.w - 1.0, h: r.h - 1.0 };
            if th.glass {
                // glass: a light catch across the top, and an edge that is bright on top and fades down
                if let Some(b) = self.lin(r.x, r.y, r.x, r.y + r.h.min(60.0), &[(0.0, 0xFFFFFF, 0.16), (0.55, 0xFFFFFF, 0.04), (1.0, 0xFFFFFF, 0.0)]) {
                    self.rt.FillRoundedRectangle(&self.rrect(r, rad), &b);
                }
                if let Some(b) = self.lin(r.x, r.y, r.x, r.y + r.h, &[(0.0, 0xFFFFFF, th.edge_a + 0.2), (0.5, 0xFFFFFF, th.edge_a * 0.45), (1.0, 0xFFFFFF, th.edge_a * 0.7)]) {
                    self.rt.DrawRoundedRectangle(&self.rrect(e, rad - 0.5), &b, 1.0, None);
                }
            } else {
                self.rt.DrawRoundedRectangle(&self.rrect(e, rad - 0.5), self.color(th.fg, th.edge_a), 1.0, None);
            }
            // hourly sheen: a diagonal band of light sweeping across the shape
            if self.sheen >= 0.0 {
                let t = self.sheen;
                let ease = 1.0 - (1.0 - t).powi(3);
                let band = 90.0;
                let x0 = r.x - band + (r.w + band * 2.0) * ease;
                let a = 0.30 * (std::f32::consts::PI * t).sin();
                if let Some(b) = self.lin(x0, r.y, x0 + band, r.y + r.h * 0.6, &[(0.0, 0xFFFFFF, 0.0), (0.5, 0xFFFFFF, a), (1.0, 0xFFFFFF, 0.0)]) {
                    self.rt.FillRoundedRectangle(&self.rrect(r, rad), &b);
                }
            }
        }
    }

    /// Record button: ring + dot at rest, a pulsing red dot while recording, a spinner while finishing.
    fn draw_rec(&self, r: R) {
        use recorder::State;
        let th = self.theme();
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let t = self.rec.started.elapsed().as_secs_f32();
        let circle = |rad: f32| D2D1_ELLIPSE { point: pt(cx, cy), radiusX: rad, radiusY: rad };
        unsafe {
            match self.rec.state {
                State::Recording => {
                    let p = ((t * std::f32::consts::TAU / 1.4).sin() + 1.0) / 2.0;
                    self.rt.FillEllipse(&circle(8.0 + 4.0 * p), self.color(th.hot, 0.28 * (1.0 - p)));
                    self.rt.FillEllipse(&circle(6.5), self.color(th.hot, 1.0));
                }
                State::Finishing => {
                    self.rt.DrawEllipse(&circle(7.5), self.color(th.fg, 0.12), 2.2, None);
                    // a quarter arc turning once a second
                    if let Some(g) = self.arc_from(cx, cy, 7.5, t * std::f32::consts::TAU, 0.28) {
                        self.rt.DrawGeometry(&g, self.color(th.accent[0], 1.0), 2.2, &self.round);
                    }
                }
                _ if self.setup_current.is_some() || !self.recording_ready() => {
                    // getting ready: how far the downloads are, as a ring (a faint dot: press to record when ready)
                    let f = self.setup_progress().unwrap_or(0.0);
                    self.rt.DrawEllipse(&circle(8.0), self.color(th.fg, 0.15), 2.0, None);
                    if let Some(g) = self.arc_from(cx, cy, 8.0, -std::f32::consts::FRAC_PI_2, f.max(0.02)) {
                        self.rt.DrawGeometry(&g, self.color(th.accent[0], 1.0), 2.0, &self.round);
                    }
                    self.rt.FillEllipse(&circle(3.2), self.color(th.hot, if self.record_when_ready { 0.9 } else { 0.35 }));
                }
                _ => {
                    self.rt.DrawEllipse(&circle(8.0), self.color(th.fg, 0.45), 1.6, None);
                    self.rt.FillEllipse(&circle(4.0), self.color(th.hot, 0.9));
                }
            }
        }
    }

    /// The living helper: a glossy blob with eyes. Idle it blinks now and then; while working it
    /// bobs and looks around; in explain mode its eyes follow your cursor.
    fn draw_mascot(&self, r: R, pill: R) {
        if self.record(Moving::Mascot(r, pill)) {
            return; // Peter is always a little alive: drawn by the moving pass
        }
        let th = self.theme();
        let t = (unsafe { windows::Win32::System::SystemInformation::GetTickCount() } % 3_600_000) as f32 / 1000.0;
        let working = self.working();
        let bob = if working { (t * 5.0).sin() * 1.3 } else { 0.0 };
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0 + bob);
        let rad = 11.0;
        // where the eyes look
        let (lx, ly) = if self.explain_mode {
            let mut c = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut c);
            }
            let (px, py) = self.cfg.pos.unwrap_or((0, 0));
            let lay_x = px as f32 + (cx - pill.x) * self.scale;
            let lay_y = py as f32 + (cy - pill.y) * self.scale;
            let (dx, dy) = (c.x as f32 - lay_x, c.y as f32 - lay_y);
            let d = (dx * dx + dy * dy).sqrt().max(1.0);
            (dx / d, dy / d)
        } else if working {
            ((t * 1.7).sin(), (t * 1.1).cos() * 0.4)
        } else {
            (0.0, 0.0)
        };
        let blink = self.blink_at.map_or(1.0, |b| {
            let e = b.elapsed().as_secs_f32();
            if e < 0.24 { (1.0 - (e / 0.12 - 1.0).abs()).mul_add(-0.85, 1.0) } else { 1.0 }
        });
        let body = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: rad, radiusY: rad * 0.94 };
        unsafe {
            if working || self.explain_mode {
                let glow = 0.5 + 0.5 * (t * 3.0).sin();
                self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: rad + 3.5, radiusY: rad + 3.5 }, self.color(th.accent[0], 0.10 + 0.12 * glow));
            }
            // two solid fills instead of a per-frame gradient brush: same look, far cheaper
            self.rt.FillEllipse(&body, self.color(th.accent[0], 1.0));
            self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx + 3.0, cy + 3.0), radiusX: rad * 0.72, radiusY: rad * 0.68 }, self.color(th.accent[1], 0.55));
            // glossy highlight
            self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx - 4.0, cy - 5.5), radiusX: 4.0, radiusY: 2.2 }, self.color(0xFFFFFF, 0.35));
            for side in [-1.0f32, 1.0] {
                let ex = cx + side * 3.8;
                let ey = cy - 0.5;
                self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(ex, ey), radiusX: 2.7, radiusY: 3.3 * blink }, self.color(0xFFFFFF, 0.97));
                if blink > 0.4 {
                    self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(ex + lx * 1.1, ey + ly * 1.3), radiusX: 1.35, radiusY: 1.5 * blink }, self.color(0x101018, 1.0));
                }
            }
            // cloud AI on (a stored key in use): a halo ring and a twinkling sparkle
            if self.cloud_ai() {
                self.rt.DrawEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: rad + 2.2, radiusY: rad * 0.94 + 2.2 }, self.color(th.accent[1], 0.85), 1.2, None);
                let tw = 0.75 + 0.25 * (t * 2.3).sin();
                let (sx, sy, s) = (cx + rad * 0.85, cy - rad * 0.95, 3.6 * tw);
                if let Ok(g) = self.d2d.CreatePathGeometry() {
                    if let Ok(sink) = g.Open() {
                        // four-point star
                        sink.BeginFigure(pt(sx, sy - s), D2D1_FIGURE_BEGIN_FILLED);
                        for (x, y) in [(0.28, -0.28), (1.0, 0.0), (0.28, 0.28), (0.0, 1.0), (-0.28, 0.28), (-1.0, 0.0), (-0.28, -0.28)] {
                            sink.AddLine(pt(sx + x * s, sy + y * s));
                        }
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        if sink.Close().is_ok() {
                            self.rt.FillGeometry(&g, self.color(0xFFFFFF, 0.95), None);
                        }
                    }
                }
            }
            // mouth: a small smile, an "o" while working
            if working {
                self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx, cy + 5.2), radiusX: 1.4, radiusY: 1.2 + 0.4 * (t * 6.0).sin().abs() }, self.color(0x101018, 0.8));
            } else if let Some(g) = self.arc_from(cx, cy + 2.2, 3.2, 0.5, 0.3) {
                self.rt.DrawGeometry(&g, self.color(0x101018, 0.75), 1.2, &self.round);
            }
        }
    }

    /// Settings button: a small gear (opens the same menu as right-clicking).
    /// A soft ring that pulses outwards around something the guide is pointing at.
    fn draw_spotlight(&self, r: R) {
        let th = self.theme();
        let t = (unsafe { windows::Win32::System::SystemInformation::GetTickCount() } % 3_600_000) as f32 / 1000.0;
        let p = (t * 1.4).fract();
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        unsafe {
            self.rt.DrawEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: 12.0 + 6.0 * p, radiusY: 12.0 + 6.0 * p }, self.color(th.accent[0], 0.9 * (1.0 - p)), 2.0, None);
            self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: 12.0, radiusY: 12.0 }, self.color(th.accent[0], 0.22));
        }
    }

    fn draw_gear(&self, r: R) {
        if self.guide == Some(setup::RECORDINGS_STEP) && !self.record(Moving::Spotlight(r)) {
            self.draw_spotlight(r); // "here's where Recordings is"
        }
        let th = self.theme();
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let hover = self.gear_hover;
        let a = if hover { 0.95 } else { 0.6 };
        unsafe {
            if hover {
                self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: 12.0, radiusY: 12.0 }, self.color(th.fg, 0.08));
            }
            self.rt.DrawEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: 4.6, radiusY: 4.6 }, self.color(th.fg, a), 1.8, None);
            for k in 0..8 {
                let ang = k as f32 * std::f32::consts::TAU / 8.0 + 0.2;
                let (c, s) = (ang.cos(), ang.sin());
                self.rt.DrawLine(pt(cx + c * 6.0, cy + s * 6.0), pt(cx + c * 8.2, cy + s * 8.2), self.color(th.fg, a), 2.2, &self.round);
            }
            self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: 1.6, radiusY: 1.6 }, self.color(th.fg, a));
        }
    }

    /// Explain answer or download progress, in the strip.
    fn draw_job_strip(&self, s: R) {
        use recorder::{JobKind, JobState};
        let Some(job) = &self.job else { return };
        let th = self.theme();
        unsafe { self.surface(s, 16.0) };
        let (x, w) = (s.x + 14.0, s.w - 28.0);
        let tc = if job.state == JobState::Failed { th.hot } else { th.fg };
        self.text(&job.title, &self.f.label, R { x, y: s.y + 9.0, w: w - 24.0, h: 14.0 }, th.fg, 0.75);
        self.text("×", &self.f.title, R { x: s.x + s.w - 30.0, y: s.y + 4.0, w: 20.0, h: 22.0 }, th.fg, 0.55);
        match job.kind {
            JobKind::Explain => {
                if !job.query.is_empty() {
                    let q: String = job.query.split_whitespace().collect::<Vec<_>>().join(" ");
                    self.text(&format!("“{q}”"), &self.f.row, R { x, y: s.y + 28.0, w, h: 18.0 }, th.accent[0], 0.9);
                }
                let wy = s.y + 52.0;
                let w16: Vec<u16> = job.text.encode_utf16().collect();
                unsafe {
                    self.rt.DrawText(&w16, &self.f.wrap, &R { x, y: wy, w, h: s.y + s.h - wy - 8.0 }.d2d(), self.color(tc, 0.92), D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                }
            }
            JobKind::Download => {
                let (done, total) = job.progress;
                let f = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                let bar = R { x, y: s.y + 34.0, w, h: 6.0 };
                unsafe {
                    self.rt.FillRoundedRectangle(&self.rrect(bar, 3.0), self.color(th.fg, 0.10));
                    let fill = if job.state == JobState::Done { 1.0 } else { f };
                    if fill > 0.0 {
                        self.rt.FillRoundedRectangle(&self.rrect(R { w: (bar.w * fill).max(6.0), ..bar }, 3.0), self.color(if job.state == JobState::Failed { th.hot } else { th.accent[0] }, 1.0));
                    }
                }
                let mb = |b: u64| b as f64 / 1_048_576.0;
                let mut detail = if job.state == JobState::Running && total > 0 { format!("{}  ·  {:.0} of {:.0} MB", job.text, mb(done), mb(total)) } else { job.text.clone() };
                if self.record_when_ready && self.setup_current.is_some() {
                    detail += "  ·  recording starts the moment it's ready"; // the press they made is remembered
                }
                self.text(&detail, &self.f.row, R { x, y: s.y + 46.0, w, h: 18.0 }, tc, 0.85);
            }
        }
    }

    /// Live transcript: status line, then the latest lines labelled by speaker.
    fn draw_strip(&self, s: R) {
        use recorder::State;
        let th = self.theme();
        unsafe { self.surface(s, 16.0) };
        let rec = &self.rec;
        let (x, w) = (s.x + 14.0, s.w - 28.0);
        let header = match rec.state {
            State::Recording => {
                let e = rec.started.elapsed().as_secs();
                let n = rec.speakers.len();
                format!("REC {:02}:{:02} · {} · {}", e / 60, e % 60, if n == 1 { "1 speaker".into() } else { format!("{n} speakers") }, rec.status)
            }
            _ => rec.status.clone(),
        };
        let hc = if rec.state == State::Failed { th.hot } else { th.fg };
        if rec.state == State::Recording {
            let p = ((rec.started.elapsed().as_secs_f32() * std::f32::consts::TAU / 1.4).sin() + 1.0) / 2.0;
            unsafe { self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(x + 4.0, s.y + 16.0), radiusX: 4.0, radiusY: 4.0 }, self.color(th.hot, 0.6 + 0.4 * p)) };
            self.text(&header, &self.f.label, R { x: x + 14.0, y: s.y + 9.0, w: w - 14.0, h: 14.0 }, hc, 0.75);
        } else {
            self.text(&header, &self.f.label, R { x, y: s.y + 9.0, w: w - 24.0, h: 14.0 }, hc, 0.85);
            self.text("×", &self.f.title, R { x: s.x + s.w - 30.0, y: s.y + 4.0, w: 20.0, h: 22.0 }, th.fg, 0.55);
        }
        let lines_max = if self.strip_expanded { STRIP_LINES_BIG } else { 3 };
        let shown: Vec<_> = rec.lines.iter().rev().take(lines_max).collect();
        if shown.is_empty() {
            let hint = if rec.state == State::Recording { "Speech from your mic and your PC will appear here" } else { "" };
            self.text(hint, &self.f.row, R { x, y: s.y + 34.0, w, h: 18.0 }, th.fg, 0.4);
        }
        for (n, line) in shown.iter().rev().enumerate() {
            let y = s.y + 32.0 + n as f32 * 21.0;
            let fade = if shown.len() == lines_max && n == 0 { 0.55 } else { 1.0 }; // oldest line dimmer
            let k = rec.speakers.iter().position(|w| *w == line.who).unwrap_or(0);
            let c = if line.who == "You" { th.accent[0] } else { th.accent[1 + k % 4] };
            self.text(&line.who, &self.f.label, R { x, y: y + 1.0, w: 66.0, h: 16.0 }, c, fade);
            self.text(&line.text, &self.f.row, R { x: x + 70.0, y, w: w - 70.0, h: 18.0 }, th.fg, 0.92 * fade);
        }
    }

    /// Arc of `frac` of a circle starting at angle `a0` (radians).
    fn arc_from(&self, cx: f32, cy: f32, r: f32, a0: f32, frac: f32) -> Option<ID2D1PathGeometry> {
        unsafe {
            let g = self.d2d.CreatePathGeometry().ok()?;
            let sink = g.Open().ok()?;
            let a1 = a0 + frac * std::f32::consts::TAU;
            sink.BeginFigure(pt(cx + r * a0.cos(), cy + r * a0.sin()), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: pt(cx + r * a1.cos(), cy + r * a1.sin()),
                size: D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if frac > 0.5 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close().ok()?;
            Some(g)
        }
    }

    /// Linear gradient brush; stops are (position, rgb, alpha), alpha scaled by the current fade.
    fn lin(&self, x0: f32, y0: f32, x1: f32, y1: f32, stops: &[(f32, u32, f32)]) -> Option<ID2D1LinearGradientBrush> {
        let stops: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|&(p, c, a)| D2D1_GRADIENT_STOP { position: p, color: color(c, a * self.alpha) }).collect();
        unsafe {
            let coll = self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok()?;
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(x0, y0), endPoint: pt(x1, y1) };
            self.rt.CreateLinearGradientBrush(&props, None, &coll).ok()
        }
    }

    /// Time + date: a cell in the pill, or a large clock in the card header. Glows during the sheen.
    fn draw_clock(&self, r: R, vertical: bool, big: bool) {
        let th = self.theme();
        let glow = if self.sheen >= 0.0 { (std::f32::consts::PI * self.sheen).sin() } else { 0.0 };
        let tc = mix(th.fg, th.accent[0], glow);
        if big {
            self.text(&self.time_text, &self.f.clock_big, R { x: r.x, y: r.y, w: 110.0, h: r.h }, tc, 1.0);
        } else if vertical {
            self.text(&self.time_text, &self.f.c_clock, R { x: r.x, y: r.y + 9.0, w: r.w, h: 18.0 }, tc, 1.0);
            self.text(&self.date_text, &self.f.c_label, R { x: r.x, y: r.y + 27.0, w: r.w, h: 12.0 }, th.fg, 0.5);
            unsafe { self.rt.FillRectangle(&R { x: r.x + 14.0, y: r.y + r.h - 1.0, w: r.w - 28.0, h: 1.0 }.d2d(), self.color(th.fg, 0.10)) };
        } else {
            self.text(&self.time_text, &self.f.clock, R { x: r.x + 12.0, y: r.y + 5.0, w: r.w - 14.0, h: 19.0 }, tc, 1.0);
            self.text(&self.date_text, &self.f.label, R { x: r.x + 12.0, y: r.y + 23.0, w: r.w - 14.0, h: 12.0 }, th.fg, 0.5);
            unsafe { self.rt.FillRectangle(&R { x: r.x + r.w - 1.0, y: r.y + 11.0, w: 1.0, h: r.h - 22.0 }.d2d(), self.color(th.fg, 0.10)) };
        }
    }

    fn ring(&self, i: usize, cx: f32, cy: f32, rad: f32) {
        if self.record(Moving::Ring(i, cx, cy, rad)) {
            return; // it glides to each new reading: drawn by the moving pass
        }
        let th = self.theme();
        let v = self.disp[i].clamp(0.0, 1.0);
        let c = if self.snap.frac[i] >= 0.9 { th.hot } else { th.accent[i] };
        let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: rad, radiusY: rad };
        unsafe {
            self.rt.DrawEllipse(&e, self.color(th.fg, 0.13), 2.6, None);
            if v > 0.004 {
                if v > 0.998 {
                    self.rt.DrawEllipse(&e, self.color(c, 1.0), 2.6, None);
                } else if let Some(g) = self.arc(cx, cy, rad, v) {
                    self.rt.DrawGeometry(&g, self.color(c, 1.0), 2.6, &self.round);
                }
            }
        }
    }

    unsafe fn draw_pill(&self, lay: &Layout) {
        let th = self.theme();
        let p = lay.pill;
        let rad = if lay.vertical { p.w / 2.0 } else { p.h / 2.0 };
        unsafe {
            self.surface(p, rad);
            self.draw_rec(lay.rec);
            self.draw_mascot(lay.mascot, lay.pill);
            self.draw_gear(lay.gear);
            if let Some(m) = lay.music {
                self.draw_music_cell(m, lay.vertical);
            }
            self.draw_mixer_button(lay.mixer);
            if let Some(k) = lay.kbd {
                self.draw_keyboard(k);
            }
            if let Some(cr) = lay.clock {
                self.draw_clock(cr, lay.vertical, false);
            }
            let inset = |r: R| if lay.vertical { R { x: r.x + 5.0, y: r.y + 2.0, w: r.w - 10.0, h: r.h - 4.0 } } else { R { x: r.x + 2.0, y: r.y + 5.0, w: r.w - 4.0, h: r.h - 10.0 } };
            let hr = if lay.vertical { 22.0 } else { 16.0 };
            if let Some(c) = lay.cells.iter().find(|c| c.0 == self.hover && c.0 != self.sel) {
                self.rt.FillRoundedRectangle(&self.rrect(inset(c.1), hr), self.color(th.fg, 0.05));
            }
            if self.hl_a > 0.003 {
                let r = R { x: self.hl[0], y: self.hl[1], w: self.hl[2], h: self.hl[3] };
                self.rt.FillRoundedRectangle(&self.rrect(inset(r), hr), self.color(th.fg, 0.11 * self.hl_a));
            }
        }
        let s = &self.snap;
        let dash = "–";
        for &(i, c) in &lay.cells {
            let val = if s.ready { s.text[i].as_str() } else { dash };
            let vc = if s.frac[i] >= 0.9 && i != NET { th.hot } else { th.fg };
            let (down, up) = if s.ready { (s.down.as_str(), s.up.as_str()) } else { (dash, dash) };
            if lay.vertical {
                // stacked: label, ring, value
                let (lc, la) = self.label_color(i);
                self.text(&self.label(i), &self.f.c_label, R { x: c.x, y: c.y + 4.0, w: c.w, h: 11.0 }, lc, la);
                if i == NET {
                    self.text(&format!("↓ {down}"), &self.f.c_net, R { x: c.x + 2.0, y: c.y + 19.0, w: c.w - 4.0, h: 15.0 }, th.fg, 0.95);
                    self.text(&format!("↑ {up}"), &self.f.c_net, R { x: c.x + 2.0, y: c.y + 36.0, w: c.w - 4.0, h: 15.0 }, th.fg, 0.95);
                } else {
                    self.ring(i, c.x + c.w / 2.0, c.y + 26.0, 8.5);
                    self.text(val, &self.f.c_value, R { x: c.x, y: c.y + 38.0, w: c.w, h: 18.0 }, vc, 0.97);
                }
            } else if i == NET {
                self.text("↓", &self.f.net, R { x: c.x + 12.0, y: c.y + 6.0, w: 12.0, h: 15.0 }, th.accent[NET], 1.0);
                self.text("↑", &self.f.net, R { x: c.x + 12.0, y: c.y + 21.0, w: 12.0, h: 15.0 }, th.accent[1], 1.0);
                self.text(down, &self.f.net, R { x: c.x + 25.0, y: c.y + 6.0, w: c.w - 30.0, h: 15.0 }, th.fg, 0.95);
                self.text(up, &self.f.net, R { x: c.x + 25.0, y: c.y + 21.0, w: c.w - 30.0, h: 15.0 }, th.fg, 0.95);
            } else {
                self.ring(i, c.x + 19.0, c.y + c.h / 2.0, 9.0);
                let (lc, la) = self.label_color(i);
                self.text(&self.label(i), &self.f.label, R { x: c.x + 34.0, y: c.y + 7.0, w: c.w - 36.0, h: 12.0 }, lc, la);
                self.text(val, &self.f.value, R { x: c.x + 34.0, y: c.y + 18.0, w: c.w - 36.0, h: 18.0 }, vc, 0.97);
            }
        }
    }

    fn arc(&self, cx: f32, cy: f32, r: f32, v: f32) -> Option<ID2D1PathGeometry> {
        unsafe {
            let g = self.d2d.CreatePathGeometry().ok()?;
            let sink = g.Open().ok()?;
            let a0 = -PI / 2.0;
            let a1 = a0 + v * 2.0 * PI;
            sink.BeginFigure(pt(cx + r * a0.cos(), cy + r * a0.sin()), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: pt(cx + r * a1.cos(), cy + r * a1.sin()),
                size: D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if v > 0.5 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close().ok()?;
            Some(g)
        }
    }

    unsafe fn draw_panel(&mut self, lay: &Layout) {
        let e = self.open.clamp(0.0, 1.0);
        let ps = self.panel_sel;
        let full = lay.panel;
        // grow out of the pill: shrink toward the pill side, slide in slightly
        let k = 0.35 + 0.65 * e;
        let slide = 8.0 * (1.0 - e);
        let mut r = full;
        match lay.dir {
            Dir::Down => {
                r.h = full.h * k;
                r.y -= slide;
            }
            Dir::Up => {
                r.h = full.h * k;
                r.y = full.y + full.h - r.h + slide;
            }
            Dir::Right => {
                r.w = full.w * k;
                r.x -= slide;
            }
            Dir::Left => {
                r.w = full.w * k;
                r.x = full.x + full.w - r.w + slide;
            }
        }
        // content is laid out at its final place, offset by the slide and clipped to the growing surface
        let (x, y, w) = match lay.dir {
            Dir::Down => (full.x, full.y - slide, full.w),
            Dir::Up => (full.x, full.y + slide, full.w),
            Dir::Right => (full.x - slide, full.y, full.w),
            Dir::Left => (full.x + slide, full.y, full.w),
        };
        unsafe {
            self.alpha = e;
            self.surface(r, 18.0);
            if (self.settings.is_some() || self.guide.is_some()) && self.pass != 1 {
                // settings stay easy to read whatever the pill opacity: back the panel nearly solid
                self.rt.FillRoundedRectangle(&self.rrect(r, 18.0), self.color(self.theme().bg, 0.94));
            }
            self.rt.PushAxisAlignedClip(&R { x: r.x + 2.0, y: r.y + 2.0, w: r.w - 4.0, h: r.h - 6.0 }.d2d(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            self.alpha = e * e;
            if self.guide.is_some() {
                self.draw_guide(full);
            } else if self.settings.is_some() {
                self.draw_settings(r, full);
            } else {
                if self.mixer.is_some() { self.draw_mixer(x, y, w) } else if self.music_open { self.draw_music_panel(x, y, w) } else { self.draw_top(x, y, w, ps, TITLES[ps]) }
            }
            self.rt.PopAxisAlignedClip();
            self.alpha = 1.0;
        }
    }

    /// Title row + top-3 list (shared by the pill panel and the card).
    unsafe fn draw_top(&self, x: f32, y: f32, w: f32, ps: usize, title: &str) {
        let th = self.theme();
        unsafe {
            let s = &self.snap;
            let accent = th.accent[ps];
            self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(x + 20.0, y + 22.0), radiusX: 4.0, radiusY: 4.0 }, self.color(accent, 1.0));
            self.text(title, &self.f.title, R { x: x + 32.0, y: y + 12.0, w: 170.0, h: 20.0 }, th.fg, 1.0);
            self.text(&s.detail[ps], &self.f.detail, R { x: x + 170.0, y: y + 12.0, w: w - 186.0, h: 20.0 }, th.fg, 0.6);
            self.rt.FillRectangle(&R { x: x + 16.0, y: y + 40.0, w: w - 32.0, h: 1.0 }.d2d(), self.color(th.fg, 0.07));
            if s.top_for != ps || s.top.is_empty() {
                let msg = if s.top_for != ps { "Sampling…" } else if ps == NET { "No active network adapters" } else { "Nothing using this right now" };
                self.text(msg, &self.f.row, R { x: x + 16.0, y: y + 50.0, w: w - 32.0, h: 18.0 }, th.fg, 0.5);
            } else {
                // rows before a section header (the disk list) are unranked; rank restarts after it
                let mut ranked = !s.top.iter().any(|t| t.frac < 0.0);
                let mut rank = 0;
                for (n, t) in s.top.iter().enumerate() {
                    let ry = y + 50.0 + n as f32 * ROW_H;
                    if t.frac < 0.0 {
                        ranked = true;
                        self.text(&t.name, &self.f.label, R { x: x + 16.0, y: ry + 6.0, w: w - 32.0, h: 14.0 }, th.fg, 0.45);
                        continue;
                    }
                    if ranked {
                        rank += 1;
                        self.text(&rank.to_string(), &self.f.row_val, R { x: x + 10.0, y: ry, w: 14.0, h: 17.0 }, th.fg, 0.35);
                    } else {
                        let d = D2D1_ELLIPSE { point: pt(x + 19.0, ry + 8.5), radiusX: 2.5, radiusY: 2.5 };
                        self.rt.FillEllipse(&d, self.color(accent, 0.8));
                    }
                    self.text(&t.name, &self.f.row, R { x: x + 32.0, y: ry, w: w - 170.0, h: 17.0 }, th.fg, 0.92);
                    self.text(&t.value, &self.f.row_val, R { x: x + w - 146.0, y: ry, w: 130.0, h: 17.0 }, th.fg, 0.95);
                    let bw = w - 48.0;
                    self.rt.FillRoundedRectangle(&self.rrect(R { x: x + 32.0, y: ry + 20.0, w: bw, h: 3.0 }, 1.5), self.color(th.fg, 0.08));
                    let f = t.frac.clamp(0.0, 1.0);
                    if f > 0.0 {
                        // red = genuinely heavy: >50% share for CPU/RAM/GPU apps, >90% active for a drive
                        let hot = match ps { NET => false, stats::DISK => !ranked && f >= 0.9, _ => f >= 0.5 };
                        let c = if hot { th.hot } else { accent };
                        self.rt.FillRoundedRectangle(&self.rrect(R { x: x + 32.0, y: ry + 20.0, w: (bw * f).max(3.0), h: 3.0 }, 1.5), self.color(c, 1.0));
                    }
                }
            }
        }
    }

    /// The larger floating view: one row per metric with a history graph, plus the top apps.
    unsafe fn draw_card(&self, lay: &Layout) {
        let th = self.theme();
        let c = lay.pill;
        let s = &self.snap;
        unsafe {
            self.surface(c, 20.0);
            self.draw_rec(lay.rec);
            self.draw_mascot(lay.mascot, lay.pill);
            self.draw_gear(lay.gear);
            self.draw_mixer_button(lay.mixer);
            if let Some(cr) = lay.clock {
                self.draw_clock(cr, false, true);
            } else {
                self.text("ResourcePill", &self.f.title, R { x: c.x + 20.0, y: c.y + 14.0, w: 160.0, h: 20.0 }, th.fg, 1.0);
            }
            let live = format!("Live · every {}", INTERVALS[self.cfg.interval].0);
            // right column: date over refresh rate (the big time sits on the left), ending just
            // before the header buttons (speaker, gear, Peter, record)
            let (top, bottom) = if self.cfg.clock { (self.date_text.as_str(), live.as_str()) } else { (live.as_str(), "") };
            self.text(top, &self.f.detail, R { x: c.x + 120.0, y: c.y + 9.0, w: c.w - 282.0, h: 16.0 }, th.fg, 0.7);
            self.text(bottom, &self.f.detail, R { x: c.x + 120.0, y: c.y + 24.0, w: c.w - 282.0, h: 14.0 }, th.fg, 0.4);
            if let Some(r) = lay.cells.iter().find(|r| r.0 == self.hover && r.0 != self.sel) {
                self.rt.FillRoundedRectangle(&self.rrect(r.1, 14.0), self.color(th.fg, 0.04));
            }
            if self.hl_a > 0.003 {
                let r = R { x: self.hl[0], y: self.hl[1], w: self.hl[2], h: self.hl[3] };
                self.rt.FillRoundedRectangle(&self.rrect(r, 14.0), self.color(th.fg, 0.08 * self.hl_a));
            }
            for &(i, r) in &lay.cells {
                let hot = s.frac[i] >= 0.9 && i != NET;
                let accent = if hot { th.hot } else { th.accent[i] };
                let (value, sub) = if !s.ready {
                    ("–".to_string(), String::new())
                } else if i == NET {
                    (format!("↓ {}", s.down), format!("↑ {}", s.up))
                } else {
                    (s.text[i].clone(), s.detail[i].clone())
                };
                if i == NET {
                    let e = D2D1_ELLIPSE { point: pt(r.x + 26.0, r.y + r.h / 2.0), radiusX: 13.0, radiusY: 13.0 };
                    self.rt.DrawEllipse(&e, self.color(th.accent[NET], 0.9), 2.6, None);
                    self.text("⇅", &self.f.title, R { x: r.x + 13.0, y: r.y + r.h / 2.0 - 10.0, w: 26.0, h: 20.0 }, th.accent[NET], 1.0);
                } else {
                    self.ring(i, r.x + 26.0, r.y + r.h / 2.0, 13.0);
                }
                let (lc, la) = self.label_color(i);
                self.text(&self.label(i), &self.f.label, R { x: r.x + 50.0, y: r.y + 8.0, w: 120.0, h: 12.0 }, lc, la);
                self.text(&value, &self.f.big, R { x: r.x + 50.0, y: r.y + 19.0, w: 128.0, h: 22.0 }, if hot { th.hot } else { th.fg }, 0.97);
                self.text(&sub, &self.f.small, R { x: r.x + 50.0, y: r.y + 41.0, w: 140.0, h: 14.0 }, th.fg, 0.55);
                // history graph
                let g = R { x: r.x + 196.0, y: r.y + 10.0, w: r.w - 206.0, h: r.h - 20.0 };
                self.rt.FillRoundedRectangle(&self.rrect(g, 6.0), self.color(th.fg, 0.035));
                let hist = &self.hist[i];
                let max = if i == NET { hist.iter().fold(1024.0f32, |a, &b| a.max(b)) } else { 1.0 };
                if hist.len() >= 2 {
                    if let Some((area, line)) = self.graph(g, hist, max) {
                        self.rt.FillGeometry(&area, self.color(accent, 0.16), None);
                        self.rt.DrawGeometry(&line, self.color(accent, 0.95), 1.5, &self.round);
                    }
                }
            }
            let ty = c.y + CARD_HEAD + lay.cells.len() as f32 * CARD_ROW;
            self.rt.FillRectangle(&R { x: c.x + 16.0, y: ty, w: c.w - 32.0, h: 1.0 }.d2d(), self.color(th.fg, 0.07));
            let ps = if self.sel != NONE { self.sel } else { lay.cells.first().map_or(0, |c| c.0) };
            let title = if ps == NET { "Busiest adapters".to_string() } else { format!("Top apps · {}", TITLES[ps]) };
            self.draw_top(c.x, ty + 2.0, c.w, ps, &title);
        }
    }

    /// Area + line path for a history graph, newest sample at the right edge.
    fn graph(&self, g: R, hist: &std::collections::VecDeque<f32>, max: f32) -> Option<(ID2D1PathGeometry, ID2D1PathGeometry)> {
        let step = g.w / (HIST - 1) as f32;
        let x0 = g.x + g.w - step * (hist.len() - 1) as f32;
        let pts: Vec<_> = hist
            .iter()
            .enumerate()
            .map(|(n, &v)| pt(x0 + n as f32 * step, g.y + g.h - 2.0 - (v / max).clamp(0.0, 1.0) * (g.h - 6.0)))
            .collect();
        unsafe {
            let line = self.d2d.CreatePathGeometry().ok()?;
            let sink = line.Open().ok()?;
            sink.BeginFigure(pts[0], D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddLines(&pts[1..]);
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close().ok()?;
            let area = self.d2d.CreatePathGeometry().ok()?;
            let sink = area.Open().ok()?;
            sink.BeginFigure(pt(x0, g.y + g.h), D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLines(&pts);
            sink.AddLine(pt(g.x + g.w, g.y + g.h));
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close().ok()?;
            Some((area, line))
        }
    }

    // ---------- tray, settings ----------

    unsafe fn tray(&self, op: NOTIFY_ICON_MESSAGE) {
        let mut nid = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        copy_wide(&mut nid.szTip, "ResourcePill");
        let _ = unsafe { Shell_NotifyIconW(op, &nid) };
    }

    fn tip(&self, s: &str) {
        let mut nid = NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: self.hwnd, uID: 1, uFlags: NIF_TIP, ..Default::default() };
        copy_wide(&mut nid.szTip, s);
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }

    /// Apply a menu command. Returns false for unknown ids.
    pub fn command(&mut self, id: u32) {
        self.run_command(id);
        self.sync_pulse(); // a setting may start or stop an animation (e.g. Peter's AI sparkle)
    }

    fn run_command(&mut self, id: u32) {
        let id = id as usize;
        match id {
            1 => self.set_visible(!self.want_visible),
            100..=199 if id - 100 < THEMES.len() => self.cfg.theme = id - 100,
            200..=204 => {
                let i = id - 200;
                if !self.cfg.shown[i] || self.cfg.shown.iter().filter(|&&b| b).count() > 1 {
                    self.cfg.shown[i] = !self.cfg.shown[i];
                }
                if self.sel != NONE && !self.cfg.shown[self.sel] {
                    self.select(NONE);
                }
                if self.cfg.card && self.sel == NONE {
                    self.select(self.shown()[0]); // the card always has a selection
                }
                self.hover = NONE;
            }
            300..=399 if id - 300 < SIZES.len() => {
                self.cfg.size = id - 300;
                self.update_scale();
            }
            400..=499 if id - 400 < OPACITY.len() => self.cfg.opacity = id - 400,
            500..=599 if id - 500 < INTERVALS.len() => {
                self.cfg.interval = id - 500;
                self.shared.interval_ms.store(INTERVALS[self.cfg.interval].1, Ordering::Relaxed);
            }
            600..=699 if id - 600 < PLACES.len() => {
                if !self.cfg.card {
                    self.select(NONE);
                }
                self.place(id - 600);
            }
            700 => {
                self.cfg.side_layout = !self.cfg.side_layout;
                if !self.cfg.side_layout {
                    self.cfg.vertical = false;
                }
            }
            701 => {
                self.cfg.click_through = !self.cfg.click_through;
                unsafe {
                    let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
                    let t = WS_EX_TRANSPARENT.0 as isize;
                    SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, if self.cfg.click_through { ex | t } else { ex & !t });
                }
            }
            702 => set_autostart(!autostart()),
            703 => {
                self.toggle_admin();
                return;
            }
            704 => self.cfg.clock = !self.cfg.clock,
            705 => {
                self.sheen = 0.0;
                if self.cfg.chime {
                    extras::play_chime();
                }
                self.kick();
                return;
            }
            706 => self.cfg.chime = !self.cfg.chime,
            707 => self.cfg.half_chime = !self.cfg.half_chime,
            711 => self.cfg.cues = !self.cfg.cues,
            713 => self.toggle_mixer(),
            714 => {
                self.cfg.duck = !self.cfg.duck;
                self.duck.on.store(self.cfg.duck, Ordering::Relaxed);
            }
            715 => {
                self.cfg.duck_ai = !self.cfg.duck_ai;
                self.duck.ai.store(self.cfg.duck_ai, Ordering::Relaxed);
            }
            712 => self.cfg.voice = !self.cfg.voice,
            1210 | 1211 => self.cfg.tx_cloud = id == 1211,
            709 => {
                self.cfg.auto_hide = !self.cfg.auto_hide;
                self.sync_edge_timer();
            }
            710 => {
                self.toggle_tuck();
                return;
            }
            708 => {
                extras::play_half_chime();
                return;
            }
            1300 => {
                self.explain_now();
                return;
            }
            1500 => {
                self.start_update();
                return;
            }
            1600 if self.video_is_open() => self.video_close(),
            1600 => self.video_open(),
            1601 => self.video_hotkey(),
            1602 => self.video_lists(),
            1610..=1619 if id - 1610 < video::ALPHAS.len() => self.video_set_alpha(id - 1610),
            1620..=1622 => self.video_set_chat(id - 1620),
            1630 => self.video_set_ghost(!self.cfg.video_ghost),
            1310..=1312 => self.cfg.explain = id as usize - 1310,
            1320 => {
                let key = extras::clipboard_text(self.hwnd).unwrap_or_default().trim().to_string();
                let provider = extras::key_provider(&key).filter(|p| extras::save_key(p, &key));
                if let Some(p) = provider {
                    extras::set_clipboard_text(self.hwnd, ""); // do not leave the key lying on the clipboard
                    self.cfg.explain = if p == "gemini" { 2 } else { 1 };
                }
                let msg = if provider.is_some() {
                    format!("Saved {} · encrypted so only your Windows account can read it", extras::mask(&key))
                } else if key.is_empty() {
                    "The clipboard has no text. Copy your key (Ctrl+C), then choose Paste again".into()
                } else {
                    "That doesn't look like an API key. Copy just the key (Anthropic sk-ant-…, or Gemini from aistudio.google.com/apikey), then choose Paste again".into()
                };
                self.job = Some(recorder::Job::note(recorder::JobKind::Explain, "API key", &msg));
                self.after_rec_change();
            }
            1321 | 1322 => {
                let gemini = id == 1322;
                extras::remove_key(if gemini { "gemini" } else { "anthropic" });
                if self.cfg.explain == if gemini { 2 } else { 1 } {
                    self.cfg.explain = 0;
                }
            }
            1400..=1498 if id - 1400 < catalog::MODELS.len() => {
                let m = &catalog::MODELS[id - 1400];
                if !recorder::models_dir().join(m.id).join(m.marker).exists() {
                    self.start_download(m.id);
                }
                return;
            }
            1499 => {
                let dir = recorder::models_dir();
                let _ = std::fs::create_dir_all(&dir);
                unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(dir.as_os_str()), None, None, SW_SHOWNORMAL) };
                return;
            }
            910..=911 => self.cfg.anim = id - 910,
            920..=923 => self.cfg.fps = id - 920,
            930..=932 => {
                // one switch for smoothness vs. PC use: frame rate and animations together
                (self.cfg.fps, self.cfg.anim) = [(3, 0), (2, 0), (0, 1)][id - 930];
            }
            717 => {
                self.cfg.kbd_show = !self.cfg.kbd_show;
                if let Some((x, y)) = self.cfg.pos {
                    self.place_at(x, y, None);
                }
            }
            716 => {
                self.cfg.art_tint = !self.cfg.art_tint;
                let cover = self.now.art.clone();
                self.tint_from_cover(cover.as_deref().map(|v| v.as_slice()));
            }
            900..=902 => {
                self.cfg.smooth = id - 900;
                self.shared.smoothing.store(self.cfg.smooth as u32, Ordering::Relaxed);
            }
            1000..=1099 if id - 1000 < recorder::ASR.len() => self.cfg.asr = id - 1000,
            1100..=1199 if id - 1100 <= recorder::LLM.len() => self.cfg.llm = id - 1100,
            1204 => {
                recorder::open_viewer(None);
                return;
            }
            1202 => self.cfg.final_pass = !self.cfg.final_pass,
            1203 => self.cfg.keep_audio = !self.cfg.keep_audio,
            1207 => {
                self.cfg.gpu = !self.cfg.gpu;
                self.cfg.gpu_asked = true;
                if self.cfg.gpu {
                    self.start_setup(); // download the GPU speech engine (with progress in the strip)
                }
            }
            1206 => {
                self.pick_recordings_folder();
                return;
            }
            1205 => {
                self.open_guide(1); // straight to the speech model
                return;
            }
            1200 => {
                let dir = recorder::recordings_dir();
                let _ = std::fs::create_dir_all(&dir);
                unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(dir.as_os_str()), None, None, SW_SHOWNORMAL) };
                return;
            }
            1201 => {
                self.toggle_record();
                return;
            }
            800 => {
                self.set_card(!self.cfg.card);
                return;
            }
            999 => {
                unsafe { self.quit() };
                return;
            }
            _ => return,
        }
        // size/metric changes alter the pill's footprint: keep it on screen
        if let Some((x, y)) = self.cfg.pos {
            self.place_at(x, y, None);
        }
        self.cfg.save();
        unsafe { self.render() };
    }

    /// Admin mode on: one UAC prompt, then an elevated copy registers the login task and takes over.
    /// Off: remove the task (elevating once if needed) and fall back to normal autostart.
    fn toggle_admin(&mut self) {
        let Ok(exe) = std::env::current_exe() else { return };
        let elevated = admin::is_elevated();
        if self.cfg.admin_mode {
            let removed = if elevated { admin::remove_task() } else { runas(&exe, "--disable-admin", true) };
            if removed {
                self.cfg.admin_mode = false;
                set_autostart(true);
                self.cfg.save();
            }
        } else if elevated {
            if admin::install_task() {
                self.cfg.admin_mode = true;
                set_autostart(false);
                self.cfg.save();
            }
        } else if runas(&exe, "--enable-admin", false) {
            // the elevated copy waits for us to exit, then installs the task and carries on
            unsafe { self.quit() };
        }
    }

    unsafe fn quit(&mut self) {
        unsafe {
            // an active recording keeps going in the helper until its files are saved
            if self.rec.busy() {
                self.rec.stop();
            }
            self.end_explain_mode(); // normal cursors back, hook removed
            self.video_close();
            self.duck.stop.store(true, Ordering::Relaxed); // music back to your volume before exiting
            if let Some(t) = self.duck_thread.take() {
                let _ = t.join();
            }
            self.cfg.save();
            let nid = NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: self.hwnd, uID: 1, ..Default::default() };
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY);
            self.wake = None; // ends the sampler thread
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        unsafe {
            if !self.bmp.is_invalid() {
                SelectObject(self.memdc, self.old);
                let _ = DeleteObject(self.bmp.into());
            }
            let _ = DeleteDC(self.memdc);
            let _ = DestroyIcon(self.icon);
        }
    }
}

// ---------- helpers ----------

/// Full monitor rectangle (the screen edge the pill tucks into).
fn monitor_rect(p: POINT) -> RECT {
    unsafe {
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() { mi.rcMonitor } else { RECT { left: 0, top: 0, right: 1920, bottom: 1080 } }
    }
}

fn work_area(p: POINT) -> RECT {
    unsafe {
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() { mi.rcWork } else { RECT { left: 0, top: 0, right: 1920, bottom: 1080 } }
    }
}

fn on_screen(x: i32, y: i32) -> bool {
    !unsafe { MonitorFromPoint(POINT { x: x + 20, y: y + 20 }, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// Blend two RGB colours (t = 0 → a, 1 → b).
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |s: u32| (((a >> s) & 0xFF) as f32 * (1.0 - t) + ((b >> s) & 0xFF) as f32 * t).round() as u32;
    ch(16) << 16 | ch(8) << 8 | ch(0)
}

fn color(rgb: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 0xFF) as f32 / 255.0,
        g: ((rgb >> 8) & 0xFF) as f32 / 255.0,
        b: (rgb & 0xFF) as f32 / 255.0,
        a: a.clamp(0.0, 1.0),
    }
}

fn pt(x: f32, y: f32) -> windows_numerics::Vector2 {
    windows_numerics::Vector2 { X: x, Y: y }
}

fn copy_wide(dst: &mut [u16], s: &str) {
    let n = dst.len() - 1;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16().take(n).chain(std::iter::once(0))) {
        *d = c;
    }
}

/// Relaunch ourselves elevated (shows the UAC prompt). With `wait`, returns the helper's success.
fn runas(exe: &std::path::Path, args: &str, wait: bool) -> bool {
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(args);
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        if ShellExecuteExW(&mut sei).is_err() {
            return false; // UAC declined or failed
        }
        if sei.hProcess.is_invalid() {
            return !wait;
        }
        let mut ok = true;
        if wait {
            WaitForSingleObject(sei.hProcess, 20_000);
            let mut code = 1u32;
            ok = GetExitCodeProcess(sei.hProcess, &mut code).is_ok() && code == 0;
        }
        let _ = CloseHandle(sei.hProcess);
        ok
    }
}

fn autostart() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME, RRF_RT_REG_SZ, None, None, None) == ERROR_SUCCESS }
}

fn set_autostart(on: bool) {
    unsafe {
        if on {
            let Ok(exe) = std::env::current_exe() else { return };
            let v: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(std::iter::once(0)).collect();
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME, REG_SZ.0, Some(v.as_ptr() as *const _), (v.len() * 2) as u32);
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME);
        }
    }
}

/// Dark context menus (uxtheme ordinal 135 = SetPreferredAppMode, 136 = FlushMenuThemes).
fn dark_menus() {
    unsafe {
        let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(f) = GetProcAddress(ux, PCSTR(135 as *const u8)) {
            let f: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            f(2); // ForceDark
        }
        if let Some(f) = GetProcAddress(ux, PCSTR(136 as *const u8)) {
            let f: extern "system" fn() = std::mem::transmute(f);
            f();
        }
    }
}

/// Tray icon drawn in code: a gradient capsule with three dots.
fn make_icon() -> HICON {
    const S: i32 = 32;
    unsafe {
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: S,
                biHeight: -S,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let Ok(colorbm) = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0) else { return HICON::default() };
        if bits.is_null() {
            return HICON::default();
        }
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (S * S) as usize);
        let (a, b) = ((0x5A, 0xC8, 0xFA), (0xBF, 0x5A, 0xF2));
        for y in 0..S {
            for x in 0..S {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let cx = fx.clamp(10.0, 22.0);
                let d = ((fx - cx).powi(2) + (fy - 16.0).powi(2)).sqrt();
                let cov = (10.0 - d + 0.5).clamp(0.0, 1.0);
                let dot = [9.5f32, 16.0, 22.5]
                    .iter()
                    .map(|&dx| (2.6 - ((fx - dx).powi(2) + (fy - 16.0).powi(2)).sqrt() + 0.5).clamp(0.0, 1.0))
                    .fold(0.0f32, f32::max);
                let t = x as f32 / S as f32;
                let mix = |p: i32, q: i32| ((p as f32 * (1.0 - t) + q as f32 * t) * (1.0 - dot) + 255.0 * dot) as u32;
                let (r, g, bl) = (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2));
                px[(y * S + x) as usize] = ((cov * 255.0) as u32) << 24 | r << 16 | g << 8 | bl;
            }
        }
        let mask = CreateBitmap(S, S, 1, 1, None);
        let ii = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: colorbm };
        let icon = CreateIconIndirect(&ii).unwrap_or_default();
        let _ = DeleteObject(colorbm.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

/// Serialises tests that change process environment variables (APPDATA).
#[cfg(test)]
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_roundtrip_and_garbage() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner()); // tests that change APPDATA must not overlap
        let s = Settings { theme: 4, shown: [true, false, true, false, true], size: 2, opacity: 3, interval: 0, side_layout: true, vertical: true, click_through: false, card: true, smooth: 2, admin_mode: true, clock: false, anim: 1, asr: 1, llm: 2, chime: false, explain: 1, final_pass: false, keep_audio: true, half_chime: false, cues: false, tx_cloud: true, voice: false, fps: 1, duck: false, duck_ai: false, duck_roles: "chrome=l".into(), art_tint: false, kbd_show: false, guide_done: true, rec_dir: "D:/Meetings".into(), auto_hide: true, gpu: true, gpu_asked: true, video_url: "https://youtu.be/dQw4w9WgXcQ?t=5".into(), video_w: 640, video_alpha: 2, video_chat: 2, video_ghost: true, pos: Some((-100, 50)) };
        let dir = std::env::temp_dir().join("rp-test");
        unsafe { std::env::set_var("APPDATA", &dir) };
        s.save();
        let l = Settings::load();
        assert_eq!((l.theme, l.shown, l.size, l.opacity, l.interval, l.vertical, l.card, l.smooth, l.pos), (4, s.shown, 2, 3, 0, true, true, 2, Some((-100, 50))));
        assert_eq!((l.clock, l.anim, l.asr, l.llm, l.chime, l.explain, l.final_pass, l.keep_audio), (false, 1, 1, 2, false, 1, false, true));
        assert_eq!((l.cues, l.tx_cloud, l.voice, l.fps, l.duck, l.duck_ai), (false, true, false, 1, false, false));
        assert_eq!((l.duck_roles.as_str(), l.art_tint, l.kbd_show), ("chrome=l", false, false));
        assert_eq!(l.rec_dir, "D:/Meetings");
        assert_eq!((l.gpu, l.gpu_asked), (true, true));
        assert_eq!((l.video_url.as_str(), l.video_w, l.video_alpha, l.video_chat, l.video_ghost), ("https://youtu.be/dQw4w9WgXcQ?t=5", 640, 2, 2, true));
        // corrupt / out-of-range values fall back to defaults, never panic
        let _ = std::fs::write(Settings::path().unwrap(), "theme=99\nsize=-1\nshown=00000\nx=abc\n=\n\u{0}");
        let g = Settings::load();
        assert_eq!((g.theme, g.size, g.shown, g.pos), (0, 1, [true; 5], None));
    }
}

/// `RP_PROFILE=1`: every 5 s, how many frames were drawn (full / only moving areas) and how long
/// they took, in %TEMP%\rp-profile.txt. Off (and free) otherwise.
fn profile(partial: bool, ms: f64) {
    use std::cell::Cell;
    thread_local!(static S: Cell<(u32, u32, f64, f64, Option<Instant>)> = const { Cell::new((0, 0, 0.0, 0.0, None)) });
    thread_local!(static ON: bool = std::env::var_os("RP_PROFILE").is_some());
    if !ON.with(|o| *o) {
        return;
    }
    S.with(|s| {
        let (mut full, mut part, mut full_ms, mut part_ms, since) = s.get();
        let since = since.unwrap_or_else(Instant::now);
        if partial {
            (part, part_ms) = (part + 1, part_ms + ms);
        } else {
            (full, full_ms) = (full + 1, full_ms + ms);
        }
        if since.elapsed().as_secs() >= 5 {
            let line = format!("full {full} ({:.1} ms each), moving-only {part} ({:.1} ms each)\n", full_ms / full.max(1) as f64, part_ms / part.max(1) as f64);
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("rp-profile.txt")).map(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
            s.set((0, 0, 0.0, 0.0, Some(Instant::now())));
        } else {
            s.set((full, part, full_ms, part_ms, Some(since)));
        }
    });
}
