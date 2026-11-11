#![windows_subsystem = "windows"]
//! ResourcePill: an always-on-top, pill-shaped resource monitor.

mod stats;
mod temps;
mod admin;

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
const GAP: f32 = 8.0;
const PANEL_W: f32 = 330.0;
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
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_ANIM: usize = 1;
const HOTKEY: i32 = 1;
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
    pos: Option<(i32, i32)>, // pill top-left, physical px
}

impl Default for Settings {
    fn default() -> Self {
        Settings { theme: 0, shown: [true; 5], size: 1, opacity: 0, interval: 1, side_layout: true, vertical: false, click_through: false, card: false, smooth: 0, admin_mode: false, clock: true, anim: 0, pos: None }
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
            "theme={}\nsize={}\nopacity={}\ninterval={}\nside_layout={}\nvertical={}\nclick_through={}\ncard={}\nsmooth={}\nadmin_mode={}\nclock={}\nanim={}\nshown={}\nx={}\ny={}\n",
            self.theme, self.size, self.opacity, self.interval, b(self.side_layout), b(self.vertical), b(self.click_through), b(self.card), self.smooth, b(self.admin_mode), b(self.clock), self.anim, shown, x, y
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
struct Layout {
    clock: Option<R>, // clock cell (pill) or clock area (card header)
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
        if arg("--quit") {
            if let Ok(h) = FindWindowW(CLASS, None) {
                let _ = PostMessageW(Some(h), WM_APP_QUIT, WPARAM(0), LPARAM(0));
            }
            return;
        }
        if arg("--disable-admin") {
            std::process::exit(if elevated && admin::remove_task() { 0 } else { 1 });
        }
        // One instance only; a second launch just shows the running one. A copy started by the
        // admin task (or the enable flow) waits briefly for the previous copy to finish exiting.
        let handover = arg("--from-task") || arg("--enable-admin");
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
            WM_RBUTTONUP => {
                menu(hwnd);
                return LRESULT(0);
            }
            WM_TRAY => {
                match lp.0 as u32 {
                    WM_LBUTTONUP => {
                        with(|a| a.set_visible(!a.want_visible));
                    }
                    WM_RBUTTONUP | WM_CONTEXTMENU => menu(hwnd),
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
    clock: IDWriteTextFormat,
    c_clock: IDWriteTextFormat,
    clock_big: IDWriteTextFormat,
    big: IDWriteTextFormat,
    row: IDWriteTextFormat,
    row_val: IDWriteTextFormat,
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
    fade: f32,
    want_visible: bool,
    last_frame: Instant,
    anim_on: bool,
    alpha: f32, // multiplier applied by `color`

    // dragging: cursor offset inside the pill as a fraction, so it survives orientation changes
    press: Option<(POINT, (f32, f32))>,
    dragging: bool,
    tracking: bool,

    // clock + hourly sheen
    time_text: String,
    date_text: String,
    hour: Option<u16>,
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
                clock: fmt(15.0, semi, l)?,
                c_clock: fmt(13.5, semi, c)?,
                clock_big: fmt(22.0, semi, l)?,
                big: fmt(18.0, semi, l)?,
                row: fmt(12.0, normal, l)?,
                row_val: fmt(12.0, semi, t)?,
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
                fade: 0.0,
                want_visible: true,
                last_frame: Instant::now(),
                anim_on: false,
                alpha: 1.0,
                press: None,
                dragging: false,
                tracking: false,
                time_text: String::new(),
                date_text: String::new(),
                hour: None,
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
            self.tray(NIM_ADD);
            if autostart() {
                set_autostart(true); // keep the path current if the exe moved
            }
            let h = self.hwnd.0 as isize;
            self.wake = Some(stats::spawn(self.shared.clone(), move || {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_DATA, WPARAM(0), LPARAM(0));
            }));
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
        }
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
            (V_PILL, s.len() as f32 * V_SEG + PAD * 2.0 + if self.cfg.clock { CLOCK_V } else { 0.0 })
        } else {
            (s.iter().map(|&i| H_SEG[i]).sum::<f32>() + PAD * 2.0 + if self.cfg.clock { CLOCK_W } else { 0.0 }, H_PILL)
        }
    }

    fn layout(&self) -> Layout {
        if self.cfg.card {
            let (w, h) = self.pill_size(false);
            let cells = self
                .shown()
                .into_iter()
                .enumerate()
                .map(|(n, i)| (i, R { x: M + 8.0, y: M + CARD_HEAD + n as f32 * CARD_ROW, w: w - 16.0, h: CARD_ROW }))
                .collect();
            return Layout {
                clock: Some(R { x: M + 20.0, y: M + 8.0, w: 200.0, h: CARD_HEAD - 12.0 }),
                vertical: false,
                pill: R { x: M, y: M, w, h },
                cells,
                panel: R::default(),
                dir: Dir::Down,
                win_w: w + M * 2.0,
                win_h: h + M * 2.0,
            };
        }
        let vertical = self.cfg.vertical;
        let (pw, ph) = self.pill_size(vertical);
        let mut cells = Vec::new();
        let mut off = PAD;
        let clock = self.cfg.clock.then(|| {
            let r = if vertical { R { x: 0.0, y: off, w: pw, h: CLOCK_V } } else { R { x: off, y: 0.0, w: CLOCK_W, h: ph } };
            off += if vertical { CLOCK_V } else { CLOCK_W };
            r
        });
        for i in self.shown() {
            if vertical {
                cells.push((i, R { x: 0.0, y: off, w: pw, h: V_SEG }));
                off += V_SEG;
            } else {
                cells.push((i, R { x: off, y: 0.0, w: H_SEG[i], h: ph }));
                off += H_SEG[i];
            }
        }
        // Panel goes toward the side of the screen with room, and is kept on screen.
        let (px, py) = self.cfg.pos.unwrap_or((0, 0));
        let wa = work_area(POINT { x: px + (pw * self.scale) as i32 / 2, y: py + (ph * self.scale) as i32 / 2 });
        let rel = |v: i32, o: i32| (v - o) as f32 / self.scale; // screen px → DIPs relative to pill
        let (wl, wr, wt, wb) = (rel(wa.left, px), rel(wa.right, px), rel(wa.top, py), rel(wa.bottom, py));
        let panel_w = if vertical { PANEL_W } else { pw.max(PANEL_W) };
        let anchor = cells.iter().find(|c| c.0 == self.panel_sel).or(cells.first()).map(|c| c.1).unwrap_or_default();
        let (dir, mut panel) = if vertical {
            let right = (pw / 2.0 - wl) < (wr - pw / 2.0); // open toward the roomier side
            let x = if right { pw + GAP } else { -GAP - panel_w };
            let y = anchor.y + anchor.h / 2.0 - self.list_h() / 2.0;
            (if right { Dir::Right } else { Dir::Left }, R { x, y, w: panel_w, h: self.list_h() })
        } else {
            let down = (ph / 2.0 - wt) < (wb - ph / 2.0);
            let y = if down { ph + GAP } else { -GAP - self.list_h() };
            (if down { Dir::Down } else { Dir::Up }, R { x: (pw - panel_w) / 2.0, y, w: panel_w, h: self.list_h() })
        };
        panel.x = panel.x.clamp(wl + EDGE, (wr - EDGE - panel.w).max(wl + EDGE));
        if vertical {
            panel.y = panel.y.clamp(wt + EDGE, (wb - EDGE - panel.h).max(wt + EDGE));
        }
        // window = union(pill, panel) + margin; shift everything into window space
        let (l, t) = (panel.x.min(0.0), panel.y.min(0.0));
        let (r, b) = ((panel.x + panel.w).max(pw), (panel.y + panel.h).max(ph));
        let (ox, oy) = (M - l, M - t);
        let mv = |r: R| R { x: r.x + ox, y: r.y + oy, ..r };
        Layout {
            clock: clock.map(mv),
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
                    // the card's list can grow (e.g. one row per drive): keep it on screen
                    if self.cfg.card && self.press.is_none() {
                        if let Some((x, y)) = self.cfg.pos {
                            self.place_at(x, y, None);
                        }
                    }
                    if self.want_visible {
                        let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
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
                        if full || self.sheen >= 0.0 {
                            self.kick();
                        } else if !self.anim_on {
                            self.render();
                        }
                    }
                    let s = &self.snap;
                    let tip = format!("ResourcePill\nCPU {} · RAM {} · GPU {}\nDisk {} · Net ↓{} ↑{}", s.text[0], s.text[1], s.text[2], s.text[3], s.down, s.up);
                    self.tip(&tip);
                }
                WM_TIMER if wp.0 == TIMER_ANIM => {
                    let moving = self.step();
                    if self.fade > 0.0 {
                        self.render();
                    }
                    if !moving {
                        let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
                        self.anim_on = false;
                        if !self.want_visible {
                            let _ = ShowWindow(self.hwnd, SW_HIDE);
                        }
                    }
                }
                WM_HOTKEY if wp.0 as i32 == HOTKEY => self.set_visible(!self.want_visible),
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
                        if h != self.hover {
                            self.hover = h;
                            self.render();
                        }
                    }
                }
                WM_MOUSELEAVE => {
                    self.tracking = false;
                    if self.hover != NONE {
                        self.hover = NONE;
                        self.render();
                    }
                }
                WM_LBUTTONUP => {
                    let _ = ReleaseCapture();
                    if self.press.take().is_some() {
                        if self.dragging {
                            self.cfg.save();
                        } else {
                            let h = self.hit(lp);
                            if h != NONE {
                                // the card always shows a top-apps list; the pill toggles its panel
                                self.select(if h == self.sel && !self.cfg.card { NONE } else { h });
                            }
                        }
                    }
                    self.dragging = false;
                }
                WM_LBUTTONDBLCLK => self.set_card(!self.cfg.card),
                WM_CAPTURECHANGED => {
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

    fn select(&mut self, i: usize) {
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
    }

    /// Start the ~60 fps animation timer; it stops itself once everything settles.
    fn kick(&mut self) {
        if !self.anim_on {
            self.last_frame = Instant::now();
            self.anim_on = unsafe { SetTimer(Some(self.hwnd), TIMER_ANIM, 15, None) } != 0;
            if !self.anim_on {
                self.step();
                unsafe { self.render() };
            }
        }
    }

    fn step(&mut self) -> bool {
        let dt = self.last_frame.elapsed().as_secs_f32().min(0.05);
        self.last_frame = Instant::now();
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
        for i in 0..5 {
            go(&mut self.disp[i], self.snap.frac[i], 16.0, 0.003);
        }
        let open = self.sel != NONE;
        go(&mut self.open, if open { 1.0 } else { 0.0 }, 14.0, 0.003);
        go(&mut self.hl_a, if open { 1.0 } else { 0.0 }, 14.0, 0.003);
        if open {
            if let Some(c) = self.layout().cells.iter().find(|c| c.0 == self.sel) {
                let t = [c.1.x, c.1.y, c.1.w, c.1.h];
                for k in 0..4 {
                    go(&mut self.hl[k], t[k], 18.0, 0.05);
                }
            }
        }
        go(&mut self.fade, if self.want_visible { 1.0 } else { 0.0 }, 12.0, 0.003);
        if self.sheen >= 0.0 {
            self.sheen += dt / 1.6; // 1.6 s sweep
            if self.sheen >= 1.0 {
                self.sheen = -1.0;
            }
            moving = true;
        }
        moving
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

    unsafe fn render(&mut self) {
        unsafe {
            let lay = self.layout();
            let (pw, ph) = ((lay.win_w * self.scale).ceil() as i32, (lay.win_h * self.scale).ceil() as i32);
            if !self.ensure_surface(pw, ph) {
                return;
            }
            self.rt.SetDpi(96.0 * self.scale, 96.0 * self.scale);
            let rc = RECT { left: 0, top: 0, right: pw, bottom: ph };
            if self.rt.BindDC(self.memdc, &rc).is_err() {
                return;
            }
            self.rt.BeginDraw();
            self.rt.Clear(Some(&color(0, 0.0)));
            self.alpha = 1.0;
            if self.cfg.card {
                self.draw_card(&lay);
            } else {
                self.draw_pill(&lay);
            }
            if !self.cfg.card && self.open > 0.003 && self.panel_sel != NONE && self.cfg.shown[self.panel_sel] {
                self.draw_panel(&lay);
            }
            if self.rt.EndDraw(None, None).is_err() {
                return;
            }
            // window position derives from the pill position so the pill never moves when the panel flips sides
            let (px, py) = self.cfg.pos.unwrap_or((0, 0));
            let dst = POINT { x: px - (lay.pill.x * self.scale).round() as i32, y: py - (lay.pill.y * self.scale).round() as i32 };
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


    unsafe fn surface(&self, r: R, rad: f32) {
        let th = self.theme();
        unsafe {
            // soft shadow
            for i in 0..7 {
                let g = i as f32 * 1.6;
                let s = R { x: r.x - g, y: r.y - g + 3.0, w: r.w + g * 2.0, h: r.h + g * 2.0 };
                self.rt.FillRoundedRectangle(&self.rrect(s, rad + g), self.color(0, th.shadow_a * (1.0 - i as f32 / 7.0)));
            }
            self.rt.FillRoundedRectangle(&self.rrect(r, rad), self.color(th.bg, th.bg_a * OPACITY[self.cfg.opacity].1));
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
            self.rt.PushAxisAlignedClip(&R { x: r.x + 2.0, y: r.y + 2.0, w: r.w - 4.0, h: r.h - 6.0 }.d2d(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            self.alpha = e * e;
            self.draw_top(x, y, w, ps, TITLES[ps]);
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
            if let Some(cr) = lay.clock {
                self.draw_clock(cr, false, true);
            } else {
                self.text("ResourcePill", &self.f.title, R { x: c.x + 20.0, y: c.y + 14.0, w: 160.0, h: 20.0 }, th.fg, 1.0);
            }
            let live = format!("Live · every {}", INTERVALS[self.cfg.interval].0);
            // right column: date over refresh rate (the big time sits on the left)
            let (top, bottom) = if self.cfg.clock { (self.date_text.as_str(), live.as_str()) } else { (live.as_str(), "") };
            self.text(top, &self.f.detail, R { x: c.x + 160.0, y: c.y + 9.0, w: c.w - 180.0, h: 16.0 }, th.fg, 0.7);
            self.text(bottom, &self.f.detail, R { x: c.x + 160.0, y: c.y + 24.0, w: c.w - 180.0, h: 14.0 }, th.fg, 0.4);
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
    fn command(&mut self, id: u32) {
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
                self.kick();
                return;
            }
            910..=911 => self.cfg.anim = id - 910,
            900..=902 => {
                self.cfg.smooth = id - 900;
                self.shared.smoothing.store(self.cfg.smooth as u32, Ordering::Relaxed);
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

/// Context menu, shown outside the App borrow because TrackPopupMenu pumps messages.
unsafe fn menu(hwnd: HWND) {
    unsafe {
        let Some(cfg) = with(|a| (a.cfg.clone(), a.want_visible)) else { return };
        let (cfg, visible) = cfg;
        let Ok(m) = CreatePopupMenu() else { return };
        let item = |menu: HMENU, id: usize, text: &str, checked: bool| {
            let flags = if checked { MF_STRING | MF_CHECKED } else { MF_STRING };
            let _ = AppendMenuW(menu, flags, id, &HSTRING::from(text));
        };
        let sub = |title: &str, fill: &dyn Fn(HMENU)| {
            if let Ok(s) = CreatePopupMenu() {
                fill(s);
                let _ = AppendMenuW(m, MF_POPUP, s.0 as usize, &HSTRING::from(title));
            }
        };
        item(m, 1, if visible { "Hide\tCtrl+Alt+M" } else { "Show\tCtrl+Alt+M" }, false);
        item(m, 800, if cfg.card { "Switch to pill\tDouble-click" } else { "Switch to card (larger window)\tDouble-click" }, false);
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        sub("Theme", &|s| THEMES.iter().enumerate().for_each(|(i, t)| item(s, 100 + i, t.name, cfg.theme == i)));
        sub("Metrics", &|s| LABELS.iter().enumerate().for_each(|(i, l)| item(s, 200 + i, l, cfg.shown[i])));
        sub("Size", &|s| SIZES.iter().enumerate().for_each(|(i, z)| item(s, 300 + i, z.0, cfg.size == i)));
        sub("Background opacity", &|s| OPACITY.iter().enumerate().for_each(|(i, o)| item(s, 400 + i, o.0, cfg.opacity == i)));
        sub("Refresh rate", &|s| INTERVALS.iter().enumerate().for_each(|(i, v)| item(s, 500 + i, v.0, cfg.interval == i)));
        sub("Animations", &|s| ANIMS.iter().enumerate().for_each(|(i, v)| item(s, 910 + i, v, cfg.anim == i)));
        item(m, 704, "Show clock", cfg.clock);
        item(m, 705, "Play the hourly animation", false);
        sub("Smoothing", &|s| SMOOTH.iter().enumerate().for_each(|(i, v)| item(s, 900 + i, v, cfg.smooth == i)));
        sub("Snap to", &|s| PLACES.iter().enumerate().for_each(|(i, p)| item(s, 600 + i, p, false)));
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        item(m, 700, "Vertical when docked to a side", cfg.side_layout);
        item(m, 701, "Click-through (unlock from tray)", cfg.click_through);
        if cfg.admin_mode {
            let _ = AppendMenuW(m, MF_STRING | MF_CHECKED | MF_GRAYED, 702, w!("Start with Windows (admin task)"));
        } else {
            item(m, 702, "Start with Windows", autostart());
        }
        item(m, 703, "Admin mode: CPU && SATA temperatures", cfg.admin_mode);
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        item(m, 999, "Quit", false);
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, p.x, p.y, None, hwnd, None).0;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(m); // destroys submenus too
        if cmd > 0 {
            with(|a| a.command(cmd as u32));
        }
    }
}

// ---------- helpers ----------

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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_roundtrip_and_garbage() {
        let s = Settings { theme: 4, shown: [true, false, true, false, true], size: 2, opacity: 3, interval: 0, side_layout: true, vertical: true, click_through: false, card: true, smooth: 2, admin_mode: true, clock: false, anim: 1, pos: Some((-100, 50)) };
        let dir = std::env::temp_dir().join("rp-test");
        unsafe { std::env::set_var("APPDATA", &dir) };
        s.save();
        let l = Settings::load();
        assert_eq!((l.theme, l.shown, l.size, l.opacity, l.interval, l.vertical, l.card, l.smooth, l.clock, l.anim, l.pos), (4, s.shown, 2, 3, 0, true, true, 2, false, 1, Some((-100, 50))));
        // corrupt / out-of-range values fall back to defaults, never panic
        let _ = std::fs::write(Settings::path().unwrap(), "theme=99\nsize=-1\nshown=00000\nx=abc\n=\n\u{0}");
        let g = Settings::load();
        assert_eq!((g.theme, g.size, g.shown, g.pos), (0, 1, [true; 5], None));
    }
}
