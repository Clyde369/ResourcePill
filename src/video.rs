//! YouTube under the pill: a WebView2 window docked under the pill (beside it when the pill is
//! vertical) that follows it around, hides with it and tucks with it. The page (video.html) runs
//! the player, live chat and the hover bar; this side docks, sizes, fades and makes it
//! click-through ("ghost": clicks go to what's underneath, it fades while the mouse is over it,
//! hold Ctrl to use it again).

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize};

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;

use crate::*;

const PAGE: &str = include_str!("video.html");
const UI: &str = include_str!("video.js");
const HOST: &str = "peter.pill"; // the page's own origin (YouTube needs a real one to embed and for live chat)
pub const ALPHAS: [u8; 5] = [100, 85, 70, 50, 35]; // window opacity choices, %
pub const CHATS: [&str; 3] = ["Off", "Beside the video", "Over the video"];
const PEEK: u8 = 18; // % while the mouse is over a ghost video
const CHAT_W: f32 = 300.0; // side chat column (DIPs, matches video.html)
const GAP: f32 = 6.0; // between the pill and the video
const MIN_W: f32 = 320.0;
const RADIUS: f32 = 12.0;
pub const WM_APP_VIDEO: u32 = WM_APP + 20; // the page sent something (see INBOX)
pub const TIMER_VIDEO: usize = 20; // hover bar + ghost peek: watches the mouse while the video shows
const CLASS: PCWSTR = w!("ResourcePill.Video");

static PILL: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    // outside App so window messages that arrive while App is busy (our own SetWindowPos) still reach them
    static WV: RefCell<Option<(ICoreWebView2Controller, ICoreWebView2)>> = const { RefCell::new(None) };
    static INBOX: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

pub struct Video {
    hwnd: HWND,
    hidden: bool, // Ctrl+Alt+Y / the hide button (keeps playing)
    fresh: bool,  // just opened: a link on the clipboard plays first
    watch: bool,  // showing YouTube's own watch page (embedding is off for this video)
    side: bool,   // live chat beside the video (window is wider)
    shown: bool,
    rect: RECT,
    inside: bool, // mouse over it
    cursor: POINT,
    unlocked: bool, // ghost, but Ctrl was held over it: usable until the mouse leaves
    applied: (u8, bool), // opacity and click-through as last set on the window
}

/// What a pasted link points at. Every field is checked to be URL-safe (it goes into JSON as is).
#[derive(Default, Debug, PartialEq)]
pub struct Target {
    pub id: String,
    pub list: String,
    pub start: u32,
    pub channel: String,
}

fn safe(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// "90", "90s", "1m30s", "1h2m3s" → seconds
fn seconds(t: &str) -> Option<u32> {
    if t.is_empty() {
        return None;
    }
    if let Ok(n) = t.parse() {
        return Some(n);
    }
    let (mut total, mut n) = (0u32, 0u32);
    for c in t.chars() {
        match c {
            '0'..='9' => n = n.checked_mul(10)?.checked_add(c as u32 - '0' as u32)?,
            'h' | 'm' | 's' => {
                total += n * [3600, 60, 1]["hms".find(c)?];
                n = 0;
            }
            _ => return None,
        }
    }
    Some(total + n)
}

/// What the bar was given: something to play on our page, or a page to open.
#[derive(Debug, PartialEq)]
pub enum Link {
    Yt(Target),
    Tw { channel: String, video: String, clip: String },
    Web(String),
}

impl Link {
    /// The message that starts it on our page (None: it's a page to open instead).
    fn play_json(&self) -> Option<String> {
        match self {
            Link::Yt(t) => Some(format!(r#"{{"play":{{"kind":"yt","id":"{}","list":"{}","start":{},"channel":"{}"}}}}"#, t.id, t.list, t.start, t.channel)),
            Link::Tw { channel, video, clip } => Some(format!(r#"{{"play":{{"kind":"tw","channel":"{channel}","video":"{video}","clip":"{clip}"}}}}"#)),
            Link::Web(_) => None,
        }
    }
}

/// A YouTube or Twitch link plays on our page; an address opens as is; anything else is a search.
/// None for other kinds of links (file:, ms-settings: …), which never open from here.
pub fn classify(text: &str) -> Option<Link> {
    let t = text.trim();
    if t.is_empty() || t.chars().any(char::is_control) {
        return None;
    }
    if let Some(y) = parse(t) {
        return Some(Link::Yt(y));
    }
    if let Some(tw) = twitch(t) {
        return Some(tw);
    }
    let spaced = t.contains(char::is_whitespace);
    match t.split_once("://").map(|(s, _)| s.to_ascii_lowercase()) {
        Some(s) if s == "https" && !spaced => Some(Link::Web(t.to_string())),
        Some(s) if s == "http" && !spaced => Some(Link::Web(if local(t) { t.to_string() } else { format!("https://{}", &t[7..]) })), // never in the clear
        Some(_) => None,
        None if !spaced && (host(t).contains('.') || host(t) == "localhost") => Some(Link::Web(format!("https://{t}"))),
        None => Some(Link::Web(format!("https://www.google.com/search?q={}", explainui::url_encode(t)))),
    }
}

/// A Twitch channel, past broadcast or clip.
pub fn twitch(text: &str) -> Option<Link> {
    let t = text.trim();
    let h = host(t);
    let rest = t.split_once("://").map_or(t, |(_, r)| r);
    let rest = rest.split_once('/').map_or("", |(_, r)| r).split('#').next().unwrap_or("");
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let q = |k: &str| query.split('&').find_map(|p| p.strip_prefix(k)?.strip_prefix('=')).unwrap_or("").to_string();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let name = |s: &str| (1..=25).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    const NOT_CHANNELS: [&str; 22] = ["directory", "downloads", "jobs", "p", "search", "settings", "subscriptions", "inventory", "wallet", "drops", "prime", "turbo", "login", "signup", "messages", "friends", "following", "store", "bits", "moderator", "popout", "videos"];
    let (mut channel, mut video, mut clip) = (String::new(), String::new(), String::new());
    match (h.as_str(), segs.as_slice()) {
        ("clips.twitch.tv", ["embed"]) => clip = q("clip"),
        ("clips.twitch.tv", [slug]) => clip = slug.to_string(),
        ("player.twitch.tv", []) => {
            channel = q("channel");
            video = q("video").trim_start_matches('v').to_string();
        }
        ("twitch.tv" | "www.twitch.tv" | "m.twitch.tv", ["videos", id]) => video = id.to_string(),
        ("twitch.tv" | "www.twitch.tv" | "m.twitch.tv", [_, "clip", slug]) => clip = slug.to_string(),
        ("twitch.tv" | "www.twitch.tv" | "m.twitch.tv", [ch]) if !NOT_CHANNELS.contains(&ch.to_ascii_lowercase().as_str()) => channel = ch.to_ascii_lowercase(),
        _ => return None,
    }
    let ok = (channel.is_empty() || name(&channel)) && video.bytes().all(|b| b.is_ascii_digit()) && safe(&clip);
    (ok && !(channel.is_empty() && video.is_empty() && clip.is_empty())).then_some(Link::Tw { channel, video, clip })
}

/// A YouTube link (watch, youtu.be, Shorts, live, embed, playlist, channel live).
pub fn parse(text: &str) -> Option<Target> {
    let t = text.trim();
    let host = self::host(t);
    let rest = t.split_once("://").map_or(t, |(_, r)| r);
    let rest = rest.split_once('/').map_or("", |(_, r)| r);
    let rest = rest.split('#').next().unwrap_or("");
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let q = |k: &str| query.split('&').find_map(|p| p.strip_prefix(k)?.strip_prefix('=')).unwrap_or("");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut tg = Target { list: q("list").into(), start: seconds(q("t")).or_else(|| seconds(q("start"))).unwrap_or(0), ..Default::default() };
    if host == "youtu.be" || host == "www.youtu.be" {
        tg.id = segs.first()?.to_string();
    } else if is_youtube(&host) {
        match segs.as_slice() {
            [] | ["watch", ..] | ["playlist", ..] => tg.id = q("v").into(),
            ["embed", "live_stream", ..] => tg.channel = q("channel").into(),
            ["shorts" | "live" | "embed" | "v", id, ..] => tg.id = id.to_string(),
            ["channel", ch, ..] => tg.channel = ch.to_string(),
            _ => return None,
        }
    } else {
        return None;
    }
    let id_ok = tg.id.is_empty() || (tg.id.len() == 11 && safe(&tg.id));
    let ch_ok = tg.channel.is_empty() || (tg.channel.starts_with("UC") && safe(&tg.channel));
    let any = !(tg.id.is_empty() && tg.list.is_empty() && tg.channel.is_empty());
    (id_ok && ch_ok && safe(&tg.list) && any).then_some(tg)
}

/// This PC (a local dev server): the only place plain http is allowed.
fn local(url: &str) -> bool {
    matches!(host(url).as_str(), "localhost" | "127.0.0.1")
}

/// A page the window may load: https, or http on this PC only (the pill may run as administrator).
fn web_ok(url: &str) -> bool {
    url.starts_with("https://") || (url.starts_with("http://") && local(url))
}

/// Random per run; the bar's messages carry it so sites can't send their own (std's hasher keys are random).
fn session_token() -> &'static str {
    use std::hash::{BuildHasher, Hasher};
    static T: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    T.get_or_init(|| (0..2).map(|_| format!("{:016x}", std::collections::hash_map::RandomState::new().build_hasher().finish())).collect())
}

/// Lowercase host of a URL ("" if none).
fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let h = &rest[..end];
    h.rsplit('@').next().unwrap_or(h).split(':').next().unwrap_or("").to_ascii_lowercase()
}

fn is_youtube(h: &str) -> bool {
    h == "youtube.com" || h.ends_with(".youtube.com") || h == "youtube-nocookie.com" || h.ends_with(".youtube-nocookie.com")
}

fn navigate(url: &str) {
    WV.with(|w| {
        if let Some((_, wv)) = &*w.borrow() {
            let _ = unsafe { wv.Navigate(&HSTRING::from(url)) };
        }
    });
}

fn post(json: &str) {
    WV.with(|w| {
        if let Some((_, wv)) = &*w.borrow() {
            let _ = unsafe { wv.PostWebMessageAsJson(&HSTRING::from(json)) };
        }
    });
}

fn data_dir() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("ResourcePill"))
}

/// Physical client size of the video window.
unsafe fn client(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut r) };
    r
}

/// Rounded corners.
unsafe fn round(hwnd: HWND) {
    unsafe {
        let r = client(hwnd);
        let rad = (RADIUS * GetDpiForWindow(hwnd) as f32 / 96.0) as i32 * 2;
        let rgn = Some(CreateRoundRectRgn(0, 0, r.right + 1, r.bottom + 1, rad, rad));
        let _ = SetWindowRgn(hwnd, rgn, true); // the window owns the region from here
    }
}

extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // no frame: the page draws everything (WS_THICKFRAME is only there so it can be resized)
            WM_NCCALCSIZE if wp.0 != 0 => return LRESULT(0),
            WM_NCACTIVATE => return LRESULT(1),
            // always just under the pill, so its panels open over the video (YouTube's full screen just fills this window)
            WM_WINDOWPOSCHANGING => {
                let p = &mut *(lp.0 as *mut WINDOWPOS);
                let pill = PILL.load(Ordering::Relaxed);
                if p.flags.0 & SWP_NOZORDER.0 == 0 && pill != 0 {
                    p.hwndInsertAfter = HWND(pill as *mut _);
                }
            }
            WM_SIZE => {
                WV.with(|w| {
                    if let Some((c, _)) = &*w.borrow() {
                        let _ = c.SetBounds(client(hwnd));
                    }
                });
                round(hwnd);
            }
            WM_MOVE => WV.with(|w| {
                if let Some((c, _)) = &*w.borrow() {
                    let _ = c.NotifyParentWindowPositionChanged();
                }
            }),
            WM_SIZING => {
                with(|a| a.video_sizing(wp.0 as u32, &mut *(lp.0 as *mut RECT)));
                return LRESULT(1);
            }
            WM_EXITSIZEMOVE => {
                with(|a| a.cfg.save());
            }
            WM_CLOSE => {
                with(|a| a.video_close()); // Alt+F4 while it has focus
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

/// Start WebView2 in the window; the page loads when it's ready (a moment later).
unsafe fn start_webview(hwnd: HWND) -> windows::core::Result<()> {
    let dir = data_dir().ok_or(windows::core::Error::from(E_FAIL))?;
    let page_dir = dir.join("video");
    let _ = std::fs::create_dir_all(&page_dir);
    std::fs::write(page_dir.join("player.html"), PAGE).map_err(|_| windows::core::Error::from(E_FAIL))?;
    let opts = CoreWebView2EnvironmentOptions::default();
    unsafe {
        opts.set_additional_browser_arguments("--autoplay-policy=no-user-gesture-required".into());
        opts.set_are_browser_extensions_enabled(true); // ad blockers etc. (see load_extensions)
    }
    let env_done = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |hr, env| {
        let env = hr.and(env.ok_or(windows::core::Error::from(E_POINTER)));
        let env = match env {
            Ok(e) => e,
            Err(e) => return inbox_fail(e),
        };
        let ctl_done = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |hr, ctl| {
            match hr.and(ctl.ok_or(windows::core::Error::from(E_POINTER))) {
                Ok(c) => unsafe { setup(hwnd, c, &page_dir) }.or_else(inbox_fail),
                Err(e) => inbox_fail(e),
            }
        }));
        unsafe { env.CreateCoreWebView2Controller(hwnd, &ctl_done) }.or_else(inbox_fail)
    }));
    let opts: ICoreWebView2EnvironmentOptions = opts.into();
    unsafe { CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &HSTRING::from(dir.join("WebView2").as_os_str()), &opts, &env_done) }
}

/// Unpacked browser extensions for the YouTube window (e.g. an ad blocker): every folder with a
/// manifest.json in %LOCALAPPDATA%\ResourcePill\extensions.
fn extension_dirs() -> Vec<std::path::PathBuf> {
    let Some(dir) = data_dir().map(|d| d.join("extensions")) else { return Vec::new() };
    let _ = std::fs::create_dir_all(&dir);
    std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.join("manifest.json").is_file()).collect()
}

/// Add the extensions one by one (re-adding updates them), then open the page, so ads are blocked from the first video.
fn load_extensions(wv: ICoreWebView2, mut dirs: Vec<std::path::PathBuf>) {
    let Some(dir) = dirs.pop() else {
        navigate(&format!("https://{HOST}/player.html"));
        return;
    };
    let profile = unsafe { wv.cast::<ICoreWebView2_13>().and_then(|w| w.Profile()).and_then(|p| p.cast::<ICoreWebView2Profile7>()) };
    let Ok(profile) = profile else {
        return load_extensions(wv, Vec::new()); // runtime too old for extensions: just play
    };
    let next = wv.clone();
    let done = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |_, _| {
        load_extensions(next, dirs); // a broken extension is skipped
        Ok(())
    }));
    if unsafe { profile.AddBrowserExtension(&HSTRING::from(dir.as_os_str()), &done) }.is_err() {
        load_extensions(wv, Vec::new());
    }
}

fn inbox_fail(e: windows::core::Error) -> windows::core::Result<()> {
    send(format!("fail:{}", e.message()));
    Ok(())
}

/// Hand a message to the pill (handled on WM_APP_VIDEO, when App is free).
fn send(s: String) {
    INBOX.with(|i| i.borrow_mut().push(s));
    let _ = unsafe { PostMessageW(Some(HWND(PILL.load(Ordering::Relaxed) as *mut _)), WM_APP_VIDEO, WPARAM(0), LPARAM(0)) };
}

unsafe fn setup(hwnd: HWND, ctl: ICoreWebView2Controller, page_dir: &std::path::Path) -> windows::core::Result<()> {
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            let _ = ctl.Close(); // closed while it was starting
            return Ok(());
        }
        let wv = ctl.CoreWebView2()?;
        if let Ok(c2) = ctl.cast::<ICoreWebView2Controller2>() {
            let _ = c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 255, R: 0, G: 0, B: 0 });
        }
        let s = wv.Settings()?;
        let _ = s.SetAreDefaultContextMenusEnabled(false);
        let _ = s.SetIsStatusBarEnabled(false);
        let _ = s.SetIsZoomControlEnabled(false);
        let _ = s.SetAreDevToolsEnabled(cfg!(debug_assertions));
        wv.cast::<ICoreWebView2_3>()?.SetVirtualHostNameToFolderMapping(&HSTRING::from(HOST), &HSTRING::from(page_dir.as_os_str()), COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY)?;

        let mut token = 0i64;
        // web pages only: other kinds of links (file:, ms-settings:, app links…) never open from here,
        // since the pill may be running as administrator
        wv.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut p = PWSTR::null();
                args.Uri(&mut p)?;
                let uri = take_pwstr(p);
                if !(web_ok(&uri) || uri == "about:blank") {
                    args.SetCancel(true)?;
                }
                Ok(())
            })),
            &mut token,
        )?;
        // only the bar's own messages count: they carry this session's token, which sites never see
        wv.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut p = PWSTR::null();
                args.TryGetWebMessageAsString(&mut p)?;
                let Some(s) = take_pwstr(p).strip_prefix(&format!("{}|", session_token())).map(str::to_string) else { return Ok(()) };
                if s.starts_with("crashed") || s.starts_with("fail") || s.starts_with("full") {
                    return Ok(()); // the window's own events, never from a page
                }
                // resize from the page's edges: hand the drag to Windows' own sizing loop
                if let Some(ht) = s.strip_prefix("size:").and_then(|v| v.parse::<usize>().ok()).filter(|h| (10..=17).contains(h)) {
                    let mut c = POINT::default();
                    let _ = GetCursorPos(&mut c);
                    let _ = ReleaseCapture();
                    let _ = PostMessageW(Some(hwnd), WM_NCLBUTTONDOWN, WPARAM(ht), LPARAM(((c.y as u16 as isize) << 16) | c.x as u16 as isize));
                } else {
                    send(s);
                }
                Ok(())
            })),
            &mut token,
        )?;
        // a crashed page reloads; if the whole browser went down, the pill starts the window again
        wv.add_ProcessFailed(
            &ProcessFailedEventHandler::create(Box::new(move |wv, args| {
                let Some(args) = args else { return Ok(()) };
                let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                args.ProcessFailedKind(&mut kind)?;
                if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED {
                    send("crashed".into());
                } else if [COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED, COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE, COREWEBVIEW2_PROCESS_FAILED_KIND_FRAME_RENDER_PROCESS_EXITED].contains(&kind) {
                    if let Some(wv) = wv {
                        wv.Reload()?;
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;
        // links that open a new tab open here instead; sized pop-ups (sign in with Google/Twitch…) stay
        // pop-ups, but only for web pages
        wv.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut p = PWSTR::null();
                args.Uri(&mut p)?;
                let uri = take_pwstr(p);
                let mut sized = BOOL(0);
                args.WindowFeatures()?.HasSize(&mut sized)?;
                if sized.as_bool() && web_ok(&uri) {
                    return Ok(());
                }
                args.SetHandled(true)?;
                if web_ok(&uri) {
                    navigate(&uri);
                }
                Ok(())
            })),
            &mut token,
        )?;
        let _ = ctl.SetBounds(client(hwnd));
        WV.with(|w| *w.borrow_mut() = Some((ctl, wv.clone())));
        // the bar, chat and player logic go into every page (video.js only acts on ours and YouTube's watch page)
        let ui = HSTRING::from(UI.replace("%HOST%", HOST).replace("%TOKEN%", session_token()));
        let ui_target = wv.clone();
        let added = AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(move |hr, _| {
            hr?;
            load_extensions(wv.clone(), extension_dirs());
            Ok(())
        }));
        ui_target.AddScriptToExecuteOnDocumentCreated(&ui, &added)
    }
}

impl App {
    /// Menu / Ctrl+Alt+Y: open it, or hide/show it while it's open.
    pub fn video_hotkey(&mut self) {
        match &mut self.video {
            None => self.video_open(),
            Some(v) => {
                v.hidden = !v.hidden;
                self.video_place();
            }
        }
    }

    pub fn video_is_open(&self) -> bool {
        self.video.is_some()
    }

    pub fn video_hidden(&self) -> bool {
        self.video.as_ref().is_some_and(|v| v.hidden)
    }

    pub fn video_open(&mut self) {
        if self.video.is_some() {
            return;
        }
        unsafe {
            PILL.store(self.hwnd.0 as isize, Ordering::Relaxed);
            let Ok(hinst) = GetModuleHandleW(None) else { return };
            static REGISTERED: AtomicBool = AtomicBool::new(false);
            if !REGISTERED.swap(true, Ordering::Relaxed) {
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: Some(proc),
                    hInstance: hinst.into(),
                    lpszClassName: CLASS,
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                    ..Default::default()
                });
            }
            let ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
            let Ok(hwnd) = CreateWindowExW(ex, CLASS, w!("YouTube · ResourcePill"), WS_POPUP | WS_THICKFRAME | WS_CLIPCHILDREN, 0, 0, 1, 1, None, None, Some(hinst.into()), None) else { return };
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            self.video = Some(Video { hwnd, hidden: false, fresh: true, watch: false, side: false, shown: false, rect: RECT::default(), inside: false, cursor: POINT::default(), unlocked: false, applied: (100, false) });
            self.video_place();
            if let Err(e) = start_webview(hwnd) {
                inbox_fail(e).ok();
            }
            SetTimer(Some(self.hwnd), TIMER_VIDEO, 60, None);
        }
    }

    pub fn video_close(&mut self) {
        let Some(v) = self.video.take() else { return };
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_VIDEO);
            if let Some((c, _)) = WV.with(|w| w.borrow_mut().take()) {
                let _ = c.Close();
            }
            let _ = DestroyWindow(v.hwnd);
        }
        INBOX.with(|i| i.borrow_mut().clear());
        self.cfg.save();
    }

    /// Where the video goes for the pill as laid out now (physical px): centred under the pill
    /// (over it when there's no room below), or beside a vertical pill.
    fn video_rect(&self, lay: &Layout) -> RECT {
        let s = self.scale;
        let (px, py) = self.cfg.pos.unwrap_or((0, 0));
        let (pw, ph) = (lay.pill.w * s, lay.pill.h * s);
        let wa = work_area(POINT { x: px + pw as i32 / 2, y: py + ph as i32 / 2 });
        let (l, t, r, b) = (wa.left as f32, wa.top as f32, wa.right as f32, wa.bottom as f32);
        let (e, gap) = (EDGE * s, GAP * s);
        let side = if self.video.as_ref().is_some_and(|v| v.side) { CHAT_W * s } else { 0.0 };
        let auto = if lay.vertical { 480.0 * s } else { pw };
        let mut w = if self.cfg.video_w > 0 { self.cfg.video_w as f32 * s } else { auto + side };
        w = w.clamp(MIN_W * s + side, (r - l - 2.0 * e).max(MIN_W * s));
        let mut h = (w - side) * 9.0 / 16.0;
        let max_h = b - t - 2.0 * e;
        if h > max_h {
            h = max_h;
            w = h * 16.0 / 9.0 + side;
        }
        let (px, py) = (px as f32, py as f32);
        let (x, y) = if lay.vertical {
            let x = if px + pw + gap + w <= r - e { px + pw + gap } else { px - gap - w };
            (x, py.clamp(t + e, (b - e - h).max(t + e)))
        } else {
            let below = py + ph + gap;
            let above = py - gap - h;
            let y = if below + h <= b - e || above < t + e { below } else { above };
            ((px + pw / 2.0 - w / 2.0).clamp(l + e, (r - e - w).max(l + e)), y)
        };
        RECT { left: x as i32, top: y as i32, right: (x + w) as i32, bottom: (y + h) as i32 }
    }

    /// Re-dock now (something about the video changed, not the pill).
    pub fn video_place(&mut self) {
        if self.video.is_some() {
            let lay = self.layout();
            self.video_follow(&lay);
        }
    }

    /// Keep the video with the pill: called with every pill frame.
    pub fn video_follow(&mut self, lay: &Layout) {
        let Some(v) = &self.video else { return };
        let show = !v.hidden && self.want_visible && self.tuck < 0.02;
        let rect = self.video_rect(lay);
        let Some(v) = &mut self.video else { return };
        unsafe {
            if show && (rect != v.rect || !v.shown) {
                let _ = SetWindowPos(v.hwnd, Some(HWND(PILL.load(Ordering::Relaxed) as *mut _)), rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top, SWP_NOACTIVATE);
                v.rect = rect;
            }
            if show != v.shown {
                let _ = ShowWindow(v.hwnd, if show { SW_SHOWNOACTIVATE } else { SW_HIDE });
                v.shown = show;
            }
        }
    }

    /// Back just under the pill (after the pill put itself on top again).
    pub fn video_raise(&self) {
        if let Some(v) = self.video.as_ref().filter(|v| v.shown) {
            let _ = unsafe { SetWindowPos(v.hwnd, Some(self.hwnd), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) };
        }
    }

    /// Resizing by an edge: keep 16:9 (plus the chat column) and stay docked.
    fn video_sizing(&mut self, edge: u32, r: &mut RECT) {
        let side = if self.video.as_ref().is_some_and(|v| v.side) { CHAT_W * self.scale } else { 0.0 };
        let (w, h) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
        let w = if edge == WMSZ_TOP || edge == WMSZ_BOTTOM { h * 16.0 / 9.0 + side } else { w };
        self.cfg.video_w = (w / self.scale).round() as u32;
        *r = self.video_rect(&self.layout());
        if let Some(v) = &mut self.video {
            v.rect = *r;
        }
    }

    /// Opacity and click-through for the current mouse position.
    fn video_apply(&mut self) {
        let (ghost, alpha) = (self.cfg.video_ghost, ALPHAS[self.cfg.video_alpha.min(ALPHAS.len() - 1)]);
        let Some(v) = &mut self.video else { return };
        let through = ghost && !v.unlocked;
        let want = (if through && v.inside { alpha.min(PEEK) } else { alpha }, through);
        if want == v.applied {
            return;
        }
        unsafe {
            let _ = SetLayeredWindowAttributes(v.hwnd, COLORREF(0), (want.0 as u32 * 255 / 100) as u8, LWA_ALPHA);
            let ex = GetWindowLongPtrW(v.hwnd, GWL_EXSTYLE);
            let t = WS_EX_TRANSPARENT.0 as isize;
            SetWindowLongPtrW(v.hwnd, GWL_EXSTYLE, if want.1 { ex | t } else { ex & !t });
        }
        v.applied = want;
    }

    /// TIMER_VIDEO: the hover bar, and ghost peeking (fade while the mouse is over it, Ctrl to use it).
    pub fn video_tick(&mut self) {
        // anything WM_APP_VIDEO couldn't hand over (it arrived while App was busy in a nested message loop)
        if INBOX.with(|i| !i.borrow().is_empty()) {
            self.video_inbox();
        }
        let ghost = self.cfg.video_ghost;
        let Some(v) = &mut self.video else { return };
        if !v.shown {
            return;
        }
        let mut c = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut c);
        }
        let inside = c.x >= v.rect.left && c.x < v.rect.right && c.y >= v.rect.top && c.y < v.rect.bottom;
        if inside && (!v.inside || c != v.cursor) && !(ghost && !v.unlocked) {
            post(r#"{"hover":1}"#);
        } else if !inside && v.inside {
            post(r#"{"hover":0}"#);
        }
        if !inside {
            v.unlocked = false;
        } else if ghost && unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } < 0 {
            v.unlocked = true;
        }
        v.inside = inside;
        v.cursor = c;
        self.video_apply();
    }

    fn video_state(&self) {
        let alpha = ALPHAS[self.cfg.video_alpha.min(ALPHAS.len() - 1)];
        post(&format!(r#"{{"state":{{"chat":{},"alpha":{},"ghost":{}}}}}"#, self.cfg.video_chat.min(2), alpha, self.cfg.video_ghost));
    }

    /// Play it on our page (YouTube, Twitch) or open the page; remembered so the window resumes there.
    fn video_open_link(&mut self, link: Link) {
        let away = self.video.as_ref().is_some_and(|v| v.watch);
        self.cfg.video_url = match &link {
            Link::Web(url) => url.clone(),
            _ => self.cfg.video_url.clone(),
        };
        match link.play_json() {
            None => navigate(&self.cfg.video_url),
            Some(_) if away => navigate(&format!("https://{HOST}/player.html")), // it plays once our page is ready
            Some(json) => post(&json),
        }
        self.cfg.save();
    }

    pub fn video_set_alpha(&mut self, i: usize) {
        self.cfg.video_alpha = i % ALPHAS.len();
        self.video_apply();
        self.video_state();
    }

    pub fn video_set_chat(&mut self, i: usize) {
        self.cfg.video_chat = i % CHATS.len();
        self.video_state();
    }

    pub fn video_set_ghost(&mut self, on: bool) {
        self.cfg.video_ghost = on;
        if let Some(v) = &mut self.video {
            v.unlocked = false;
        }
        self.video_apply();
        self.video_state();
    }

    /// WM_APP_VIDEO: what the page (or WebView2) sent.
    pub fn video_inbox(&mut self) {
        for m in INBOX.with(|i| std::mem::take(&mut *i.borrow_mut())) {
            let (k, val) = m.split_once(':').unwrap_or((&m, ""));
            match k {
                "ready" => {
                    self.video_state();
                    let Some(v) = &mut self.video else { continue };
                    v.watch = val != "embed"; // on some site, not our page
                    if v.watch {
                        continue;
                    }
                    // just opened: a YouTube/Twitch link on the clipboard wins, else carry on where you were.
                    // Back on our page later (Back, or a link to play): play what was asked; a site stays put
                    let fresh = std::mem::take(&mut v.fresh);
                    let playable = |t: &str| classify(t).is_some_and(|l| l.play_json().is_some());
                    let clip = extras::clipboard_text(self.hwnd).filter(|c| fresh && playable(c));
                    let text = clip.unwrap_or_else(|| self.cfg.video_url.clone());
                    if let Some(link) = classify(&text).filter(|l| fresh || l.play_json().is_some()) {
                        if link.play_json().is_some() {
                            self.cfg.video_url = text.trim().to_string();
                        }
                        self.video_open_link(link);
                    }
                }
                "open" => match classify(val) {
                    Some(link) => {
                        if link.play_json().is_some() {
                            self.cfg.video_url = val.trim().to_string();
                        }
                        self.video_open_link(link);
                    }
                    None => post(r#"{"err":"Only web addresses (https://…), YouTube and Twitch links, or a search."}"#),
                },
                // embedding is off for this video: YouTube's own page, in this window
                "watch" => {
                    let mut it = val.split('|');
                    let (id, list, start) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0));
                    if id.len() == 11 && safe(id) && safe(list) {
                        let list = if list.is_empty() { String::new() } else { format!("&list={list}") };
                        navigate(&format!("https://www.youtube.com/watch?v={id}{list}&t={start}s"));
                    }
                }
                "chat" => self.video_set_chat(val.parse().unwrap_or(0)),
                "side" => {
                    if let Some(v) = &mut self.video {
                        v.side = val == "1";
                    }
                    self.video_place();
                }
                "alpha" => self.video_set_alpha(self.cfg.video_alpha + 1),
                "ghost" => self.video_set_ghost(!self.cfg.video_ghost),
                "hide" => self.video_hotkey(),
                "close" => self.video_close(),
                "ext" if val.starts_with("https://www.youtube.com/") => unsafe {
                    ShellExecuteW(None, w!("open"), &HSTRING::from(val), None, None, SW_SHOWNORMAL);
                },
                // the browser behind the window went down: start it again (once a minute at most, so a crash loop can't spin)
                "crashed" if self.video.is_some() => {
                    thread_local!(static LAST: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) });
                    let again = LAST.with(|l| l.replace(Some(std::time::Instant::now()))).is_some_and(|t| t.elapsed().as_secs() < 60);
                    let hidden = self.video_hidden();
                    self.video_close();
                    if again {
                        self.job = Some(recorder::Job::note(recorder::JobKind::Explain, "YouTube", "The video window's browser keeps crashing. Open it again with Ctrl+Alt+Y; if it keeps happening, try removing the extensions in %LOCALAPPDATA%\\ResourcePill\\extensions."));
                        self.after_rec_change();
                    } else {
                        self.video_open();
                        if let Some(v) = &mut self.video {
                            v.hidden = hidden;
                        }
                        self.video_place();
                    }
                }
                "fail" if self.video.is_some() => {
                    self.video_close();
                    let msg = format!("YouTube needs Microsoft Edge WebView2 (built into Windows 11; on Windows 10 install it from Microsoft). {val}");
                    self.job = Some(recorder::Job::note(recorder::JobKind::Explain, "YouTube", &msg));
                    self.after_rec_change();
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: &str, list: &str, start: u32, channel: &str) -> Option<Target> {
        Some(Target { id: id.into(), list: list.into(), start, channel: channel.into() })
    }

    #[test]
    fn links() {
        assert_eq!(parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), t("dQw4w9WgXcQ", "", 0, ""));
        assert_eq!(parse("  youtube.com/watch?feature=x&v=dQw4w9WgXcQ&t=1m30s "), t("dQw4w9WgXcQ", "", 90, ""));
        assert_eq!(parse("https://youtu.be/dQw4w9WgXcQ?t=42"), t("dQw4w9WgXcQ", "", 42, ""));
        assert_eq!(parse("https://m.youtube.com/shorts/dQw4w9WgXcQ"), t("dQw4w9WgXcQ", "", 0, ""));
        assert_eq!(parse("https://www.youtube.com/live/dQw4w9WgXcQ?si=abc"), t("dQw4w9WgXcQ", "", 0, ""));
        assert_eq!(parse("https://www.youtube.com/playlist?list=PLx0sYbCqOb8TBPRdmBHs5Iftvv9TPboYG"), t("", "PLx0sYbCqOb8TBPRdmBHs5Iftvv9TPboYG", 0, ""));
        assert_eq!(parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PL1&start=1h2m3s#x"), t("dQw4w9WgXcQ", "PL1", 3723, ""));
        assert_eq!(parse("https://www.youtube.com/channel/UCX6OQ3DkcsbYNE6H8uQQuVA/live"), t("", "", 0, "UCX6OQ3DkcsbYNE6H8uQQuVA"));
        assert_eq!(parse("https://www.youtube.com/embed/live_stream?channel=UCX6OQ3DkcsbYNE6H8uQQuVA"), t("", "", 0, "UCX6OQ3DkcsbYNE6H8uQQuVA"));
        assert_eq!(parse("minecraft12"), None); // a word, not a video id
        // not YouTube, not a video, or would break out of the JSON
        assert_eq!(parse("https://notyoutube.com/watch?v=dQw4w9WgXcQ"), None);
        assert_eq!(parse("https://www.youtube.com/@IShowSpeed/live"), None);
        assert_eq!(parse("https://www.youtube.com/watch?v=dQw4w9WgX\"Q"), None);
        assert_eq!(parse("https://www.youtube.com/"), None);
        assert_eq!(parse("hello there"), None);
    }

    fn tw(channel: &str, video: &str, clip: &str) -> Option<Link> {
        Some(Link::Tw { channel: channel.into(), video: video.into(), clip: clip.into() })
    }
    fn web(url: &str) -> Option<Link> {
        Some(Link::Web(url.into()))
    }

    #[test]
    fn twitch_links() {
        assert_eq!(twitch("https://www.twitch.tv/IShowSpeed"), tw("ishowspeed", "", ""));
        assert_eq!(twitch("twitch.tv/kai_cenat?sr=a"), tw("kai_cenat", "", ""));
        assert_eq!(twitch("https://m.twitch.tv/videos/2280512345"), tw("", "2280512345", ""));
        assert_eq!(twitch("https://www.twitch.tv/xqc/clip/FunnyClipSlug-AbC_123"), tw("", "", "FunnyClipSlug-AbC_123"));
        assert_eq!(twitch("https://clips.twitch.tv/FunnyClipSlug"), tw("", "", "FunnyClipSlug"));
        assert_eq!(twitch("https://player.twitch.tv/?channel=xqc&parent=x"), tw("xqc", "", ""));
        // Twitch pages that aren't something to play: just browse them
        assert_eq!(twitch("https://www.twitch.tv/directory"), None);
        assert_eq!(twitch("https://www.twitch.tv/xqc/videos"), None);
        assert_eq!(twitch("https://www.twitch.tv/"), None);
        assert_eq!(twitch("https://www.twitch.tv/bad\"name"), None);
    }

    #[test]
    fn what_the_bar_opens() {
        assert!(matches!(classify("https://youtu.be/dQw4w9WgXcQ"), Some(Link::Yt(_))));
        assert_eq!(classify("twitch.tv/xqc"), tw("xqc", "", ""));
        assert_eq!(classify("discord.com/app"), web("https://discord.com/app"));
        assert_eq!(classify(" https://example.com/a?b=c "), web("https://example.com/a?b=c"));
        assert_eq!(classify("http://localhost:3000"), web("http://localhost:3000"));
        assert_eq!(classify("http://example.com/x"), web("https://example.com/x")); // upgraded, never in the clear
        assert!(web_ok("https://a.com") && web_ok("http://localhost:8080/") && !web_ok("http://a.com") && !web_ok("file:///C:/"));
        assert_eq!(session_token().len(), 32);
        assert_eq!(classify("best gta 6 clips"), web("https://www.google.com/search?q=best+gta+6+clips"));
        assert_eq!(classify("twitch.tv"), web("https://twitch.tv"));
        // never: other kinds of links, nothing at all
        assert_eq!(classify("file:///C:/Windows/System32"), None);
        assert_eq!(classify("ms-settings://display"), None);
        assert_eq!(classify("   "), None);
        // JSON-safe: what goes to the page is only checked characters
        assert_eq!(tw("xqc", "", "").unwrap().play_json().unwrap(), r#"{"play":{"kind":"tw","channel":"xqc","video":"","clip":""}}"#);
    }
}
