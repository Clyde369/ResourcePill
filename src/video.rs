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
const LISTS_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Lists</title></head><body></body></html>";
const LISTS_UI: &str = include_str!("lists.js");
const HOST: &str = "peter.pill"; // the page's own origin (YouTube needs a real one to embed and for live chat)
pub const ALPHAS: [u8; 5] = [100, 85, 70, 50, 35]; // window opacity choices, %
pub const CHATS: [&str; 3] = ["Off", "Beside the video", "Over the video"];
const PEEK: u8 = 18; // % while the mouse is over a ghost video
const CHAT_W: f32 = 300.0; // side chat column (DIPs, matches video.html)
const LIST_W: f32 = 320.0; // the shopping list beside the page (DIPs)
const GAP: f32 = 6.0; // between the pill and the video
const MIN_W: f32 = 320.0;
const RADIUS: f32 = 12.0;
pub const WM_APP_VIDEO: u32 = WM_APP + 20; // the page sent something (see INBOX)
pub const TIMER_VIDEO: usize = 20; // hover bar + ghost peek: watches the mouse while the video shows
const CLASS: PCWSTR = w!("ResourcePill.Video");

static PILL: AtomicIsize = AtomicIsize::new(0);
// what the page, WebView2 and background work (price lookups) sent, for WM_APP_VIDEO to hand to App
static INBOX: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
fn inbox() -> std::sync::MutexGuard<'static, Vec<String>> {
    INBOX.lock().unwrap_or_else(|e| e.into_inner())
}


pub struct Video {
    hwnd: HWND,
    hidden: bool, // Ctrl+Alt+Y / the hide button (keeps playing)
    fresh: bool,  // just opened: a link on the clipboard plays first
    side: bool,   // live chat beside the video (window is wider)
    list: bool,   // the shopping list beside the page (window is wider)
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

// ---------- tabs ----------
// Saving RAM: only the tab on screen has a live page. A background tab that is playing sound stays
// alive but hidden, asking WebView2 to use as little memory as it can; every other background tab
// is unloaded (its page closed: just the address and title are kept) and loads again when clicked.

struct Tab {
    id: u32,
    url: String,
    title: String,
    live: Option<(ICoreWebView2Controller, ICoreWebView2)>,
    audio: bool,
}

struct Browser {
    hwnd: HWND,
    env: Option<ICoreWebView2Environment>,
    page_dir: std::path::PathBuf,
    tabs: Vec<Tab>,
    active: u32,
    next: u32,
    extensions_done: bool,
    side_on: bool, // the list beside the page (its own small page on the right)
    side: Option<(ICoreWebView2Controller, ICoreWebView2)>,
}

thread_local! {
    // outside App: WebView2 events and window messages reach it while App is busy
    static BROWSER: RefCell<Option<Browser>> = const { RefCell::new(None) };
}

/// Runs `f` on the browser state if there is one and it isn't already in use (never panics on re-entry).
fn browser<R>(f: impl FnOnce(&mut Browser) -> R) -> Option<R> {
    BROWSER.with(|b| b.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn start_page() -> String {
    format!("https://{HOST}/player.html")
}

fn lists_page() -> String {
    format!("https://{HOST}/lists.html")
}

fn active_view() -> Option<ICoreWebView2> {
    browser(|b| b.tabs.iter().find(|t| t.id == b.active).and_then(|t| t.live.as_ref().map(|l| l.1.clone()))).flatten()
}

/// The page on screen goes to `url` (if it isn't loaded yet, it opens there when it is).
fn navigate(url: &str) {
    match active_view() {
        Some(wv) => {
            let _ = unsafe { wv.Navigate(&HSTRING::from(url)) };
        }
        None => {
            browser(|b| {
                let a = b.active;
                if let Some(t) = b.tabs.iter_mut().find(|t| t.id == a) {
                    t.url = url.to_string();
                }
            });
        }
    }
}

/// A message for the page on screen.
fn post(json: &str) {
    if let Some(wv) = active_view() {
        let _ = unsafe { wv.PostWebMessageAsJson(&HSTRING::from(json)) };
    }
}

/// A message only the lists page may see: sent to the tab showing it right now, never to whatever
/// site a tab moved on to while a price lookup was running.
fn post_to_lists(json: &str) {
    post_lists(json, true, true);
}

/// The same, to the lists tab(s) and/or the list beside the page (after one saves, the other catches up).
fn post_lists(json: &str, tabs: bool, side: bool) {
    let mut views: Vec<ICoreWebView2> = Vec::new();
    browser(|b| {
        if tabs {
            views.extend(b.tabs.iter().filter_map(|t| t.live.as_ref().map(|l| l.1.clone())));
        }
        if side {
            views.extend(b.side.as_ref().map(|s| s.1.clone()));
        }
    });
    for wv in views {
        let mut p = PWSTR::null();
        if unsafe { wv.Source(&mut p) }.is_ok() && take_pwstr(p).starts_with(&lists_page()) {
            let _ = unsafe { wv.PostWebMessageAsJson(&HSTRING::from(json)) };
        }
    }
}

/// The list beside the page: on (its own small page, made here) or off (closed, its memory freed).
fn side_set(on: bool) {
    let Some((was, gone, env, hwnd)) = browser(|b| {
        let was = std::mem::replace(&mut b.side_on, on);
        (was, if on { None } else { b.side.take() }, b.env.clone(), b.hwnd)
    }) else {
        return;
    };
    if let Some((c, _)) = gone {
        let _ = unsafe { c.Close() };
    }
    relayout(hwnd);
    if on && !was {
        let Some(env) = env else { return };
        let done = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |hr, ctl| match hr.and(ctl.ok_or(windows::core::Error::from(E_POINTER))) {
            Ok(c) => unsafe { setup_side(hwnd, c) }.or_else(inbox_fail),
            Err(e) => inbox_fail(e),
        }));
        let _ = unsafe { env.CreateCoreWebView2Controller(hwnd, &done) };
    }
}

/// The list beside the page: only our lists page, only list messages; a link in it opens in a tab.
unsafe fn setup_side(hwnd: HWND, ctl: ICoreWebView2Controller) -> windows::core::Result<()> {
    unsafe {
        let Some(page_dir) = browser(|b| (b.side_on && b.side.is_none()).then(|| b.page_dir.clone())).flatten() else {
            let _ = ctl.Close(); // turned off (or already there) while it was starting
            return Ok(());
        };
        let wv = ctl.CoreWebView2()?;
        if let Ok(c2) = ctl.cast::<ICoreWebView2Controller2>() {
            let _ = c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 255, R: 17, G: 18, B: 21 });
        }
        let s = wv.Settings()?;
        let _ = s.SetAreDefaultContextMenusEnabled(false);
        let _ = s.SetIsStatusBarEnabled(false);
        let _ = s.SetAreDevToolsEnabled(cfg!(debug_assertions));
        wv.cast::<ICoreWebView2_3>()?.SetVirtualHostNameToFolderMapping(&HSTRING::from(HOST), &HSTRING::from(page_dir.as_os_str()), COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY)?;
        let mut token = 0i64;
        wv.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut p = PWSTR::null();
                args.Uri(&mut p)?;
                let uri = take_pwstr(p);
                if !uri.starts_with(&lists_page()) {
                    args.SetCancel(true)?;
                    if web_ok(&uri) {
                        send(format!("list-open:{uri}"));
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;
        wv.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut p = PWSTR::null();
                args.Uri(&mut p)?;
                args.SetHandled(true)?;
                let uri = take_pwstr(p);
                if web_ok(&uri) {
                    send(format!("list-open:{uri}"));
                }
                Ok(())
            })),
            &mut token,
        )?;
        wv.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut p = PWSTR::null();
                args.TryGetWebMessageAsString(&mut p)?;
                let Some(s) = take_pwstr(p).strip_prefix(&format!("{}|", session_token())).map(str::to_string) else { return Ok(()) };
                let allowed = ["list-load", "list-save:", "list-prices:", "list-share:", "list-open:"].iter().any(|k| s.starts_with(k));
                if allowed {
                    // a save from here: the lists tab catches up (not this page, which has it already)
                    send(s.strip_prefix("list-save:").map_or(s.clone(), |d| format!("list-save-side:{d}")));
                }
                Ok(())
            })),
            &mut token,
        )?;
        let ui = HSTRING::from(LISTS_UI.replace("%HOST%", HOST).replace("%TOKEN%", session_token()));
        let nav = wv.clone();
        let added = AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(move |hr, _| {
            hr?;
            let _ = nav.Navigate(&HSTRING::from(format!("{}?side", lists_page())));
            Ok(())
        }));
        wv.AddScriptToExecuteOnDocumentCreated(&ui, &added)?;
        browser(|b| b.side = Some((ctl, wv)));
        relayout(hwnd);
        Ok(())
    }
}

/// The address of the tab on screen.
fn active_url() -> String {
    browser(|b| b.tabs.iter().find(|t| t.id == b.active).map(|t| t.url.clone())).flatten().unwrap_or_default()
}

/// Name of the bar's tab-strip receiver on each page: random per run (sites can find it, but can't
/// replace it: the bar defines it read-only before any site script runs).
fn tabs_fn() -> String {
    use std::hash::{BuildHasher, Hasher};
    static N: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    N.get_or_init(|| format!("rp{:016x}", std::collections::hash_map::RandomState::new().build_hasher().finish())).clone()
}

/// The tab strip goes straight into the bar by a script call, not by a page message: every script on
/// a page can read page messages, and other tabs' titles are nobody else's business. The bar's
/// receiver checks the session token, so a site can't feed it a fake strip either.
fn push_tabs() {
    bar_call(&tabs_json());
}

/// A short note in the bar, by the same private call as the tab strip: what you added to your list is
/// nobody else's business, and the page on screen may be another site by the time it's ready.
fn bar_toast(text: &str) {
    bar_call(&serde_json::json!({ "toast": text }).to_string());
}

/// Hands `json` to the bar on the page on screen without the page's own scripts seeing it.
fn bar_call(json: &str) {
    if let Some(wv) = active_view() {
        let call = format!("window['{0}'] && window['{0}']('{1}', {2});", tabs_fn(), session_token(), json);
        let done = ExecuteScriptCompletedHandler::create(Box::new(|_, _| Ok(())));
        let _ = unsafe { wv.ExecuteScript(&HSTRING::from(call), &done) };
    }
}

/// The tab strip for the bar: {tabs: [{id, title, active, audio, asleep}]}.
fn tabs_json() -> String {
    browser(|b| {
        let tabs: Vec<serde_json::Value> = b
            .tabs
            .iter()
            .map(|t| {
                let title = if !t.title.is_empty() {
                    t.title.clone()
                } else if t.url == start_page() {
                    "New tab".into()
                } else {
                    host(&t.url)
                };
                serde_json::json!({"id": t.id, "title": title.chars().take(80).collect::<String>(), "active": t.id == b.active, "audio": t.audio, "asleep": t.live.is_none()})
            })
            .collect();
        serde_json::json!({ "tabs": tabs }).to_string()
    })
    .unwrap_or_else(|| r#"{"tabs":[]}"#.into())
}

/// Adds a tab (on screen if `show`); returns its id.
fn tab_new(url: String, show: bool) -> Option<u32> {
    let id = browser(|b| {
        let id = b.next;
        b.next += 1;
        b.tabs.push(Tab { id, url, title: String::new(), live: None, audio: false });
        id
    })?;
    if show {
        tab_show(id);
    }
    send("tabs".into());
    Some(id)
}

/// Puts tab `id` on screen: the one it replaces is unloaded (or kept, hidden and trimmed, while it
/// plays sound), and `id` is shown, or loaded if it was asleep.
fn tab_show(id: u32) {
    let Some((prev, next)) = browser(|b| {
        if !b.tabs.iter().any(|t| t.id == id) {
            return None;
        }
        let old = std::mem::replace(&mut b.active, id);
        let prev = (old != id).then(|| b.tabs.iter_mut().find(|t| t.id == old)).flatten().and_then(|t| if t.audio { t.live.clone().map(|l| (l, false)) } else { t.live.take().map(|l| (l, true)) });
        let next = b.tabs.iter().find(|t| t.id == id).and_then(|t| t.live.clone());
        Some((prev, next))
    })
    .flatten() else {
        return;
    };
    unsafe {
        match prev {
            Some(((ctl, _), true)) => {
                let _ = ctl.Close(); // unloaded: the page and its memory are gone
            }
            Some(((ctl, wv), false)) => {
                let _ = ctl.SetIsVisible(false); // still playing: hidden, using as little memory as it can
                if let Ok(w) = wv.cast::<ICoreWebView2_19>() {
                    let _ = w.SetMemoryUsageTargetLevel(COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW);
                }
            }
            None => {}
        }
        match next {
            Some((ctl, wv)) => {
                if let Ok(w) = wv.cast::<ICoreWebView2_19>() {
                    let _ = w.SetMemoryUsageTargetLevel(COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL);
                }
                if let Some(hwnd) = browser(|b| b.hwnd) {
                    let _ = ctl.SetBounds(page_rect(hwnd));
                }
                let _ = ctl.SetIsVisible(true);
                send("tab-shown".into());
            }
            None => tab_load(id),
        }
    }
    send("tabs".into());
}

/// Closes tab `id` (the last one turns into a new tab).
fn tab_close(id: u32) {
    let Some((live, was_active, neighbour)) = browser(|b| {
        let i = b.tabs.iter().position(|t| t.id == id)?;
        let tab = b.tabs.remove(i);
        let neighbour = b.tabs.get(i).or_else(|| b.tabs.get(i.wrapping_sub(1))).map(|t| t.id);
        Some((tab.live, tab.id == b.active, neighbour))
    })
    .flatten() else {
        return;
    };
    if let Some((ctl, _)) = live {
        let _ = unsafe { ctl.Close() };
    }
    if was_active {
        match neighbour {
            Some(n) => tab_show(n),
            None => {
                tab_new(start_page(), true);
            }
        }
    }
    send("tabs".into());
}

/// Next (or previous) tab, wrapping around.
fn tab_step(forward: bool) {
    let target = browser(|b| {
        let i = b.tabs.iter().position(|t| t.id == b.active)?;
        let n = b.tabs.len();
        b.tabs.get(if forward { (i + 1) % n } else { (i + n - 1) % n }).map(|t| t.id)
    })
    .flatten();
    if let Some(t) = target {
        tab_show(t);
    }
}

/// Starts a live page for tab `id` (once the browser itself is ready).
fn tab_load(id: u32) {
    let Some((env, hwnd)) = browser(|b| Some((b.env.clone()?, b.hwnd))).flatten() else { return };
    let done = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |hr, ctl| match hr.and(ctl.ok_or(windows::core::Error::from(E_POINTER))) {
        Ok(c) => unsafe { setup(hwnd, id, c) }.or_else(inbox_fail),
        Err(e) => inbox_fail(e),
    }));
    let _ = unsafe { env.CreateCoreWebView2Controller(hwnd, &done) };
}

fn live_controllers() -> Vec<ICoreWebView2Controller> {
    browser(|b| b.tabs.iter().filter_map(|t| t.live.as_ref().map(|l| l.0.clone())).collect()).unwrap_or_default()
}

/// The tabs to remember: their addresses, and which one is on screen.
fn tabs_saved() -> Option<(String, usize)> {
    browser(|b| {
        let urls: Vec<String> = b.tabs.iter().map(|t| t.url.replace(['\t', '\r', '\n'], "")).filter(|u| web_ok(u)).collect();
        let active = b.tabs.iter().position(|t| t.id == b.active).unwrap_or(0);
        (urls.join("\t"), active)
    })
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

/// Width of the list beside the page (physical px; 0 while it's off).
fn side_w(hwnd: HWND) -> i32 {
    if browser(|b| b.side_on) == Some(true) { (LIST_W * unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0) as i32 } else { 0 }
}

/// Where the pages go: the window, less the list beside them.
fn page_rect(hwnd: HWND) -> RECT {
    let mut r = unsafe { client(hwnd) };
    r.right = (r.right - side_w(hwnd)).max(r.left);
    r
}

/// Pages on the left, the list (if on) on the right.
fn relayout(hwnd: HWND) {
    let page = page_rect(hwnd);
    for c in live_controllers() {
        let _ = unsafe { c.SetBounds(page) };
    }
    if let Some((c, _)) = browser(|b| b.side.clone()).flatten() {
        let mut r = unsafe { client(hwnd) };
        r.left = page.right;
        let _ = unsafe { c.SetBounds(r) };
    }
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
                relayout(hwnd);
                round(hwnd);
            }
            WM_MOVE => {
                for c in live_controllers().into_iter().chain(browser(|b| b.side.as_ref().map(|s| s.0.clone())).flatten()) {
                    let _ = c.NotifyParentWindowPositionChanged();
                }
            }
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

/// Start WebView2 for the window with these tabs (the one at `active` on screen); its page loads
/// once the browser is ready (a moment later).
unsafe fn start_webview(hwnd: HWND, urls: Vec<String>, active: usize) -> windows::core::Result<()> {
    let dir = data_dir().ok_or(windows::core::Error::from(E_FAIL))?;
    let page_dir = dir.join("video");
    let _ = std::fs::create_dir_all(&page_dir);
    std::fs::write(page_dir.join("player.html"), PAGE).map_err(|_| windows::core::Error::from(E_FAIL))?;
    std::fs::write(page_dir.join("lists.html"), LISTS_PAGE).map_err(|_| windows::core::Error::from(E_FAIL))?;
    let urls = if urls.is_empty() { vec![start_page()] } else { urls };
    let tabs: Vec<Tab> = urls.into_iter().enumerate().map(|(i, url)| Tab { id: i as u32, url, title: String::new(), live: None, audio: false }).collect();
    let active = active.min(tabs.len() - 1) as u32;
    let next = tabs.len() as u32;
    BROWSER.with(|b| *b.borrow_mut() = Some(Browser { hwnd, env: None, page_dir, tabs, active, next, extensions_done: false, side_on: false, side: None }));
    let opts = CoreWebView2EnvironmentOptions::default();
    unsafe {
        opts.set_additional_browser_arguments("--autoplay-policy=no-user-gesture-required".into());
        opts.set_are_browser_extensions_enabled(true); // ad blockers etc. (see load_extensions)
    }
    let env_done = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |hr, env| {
        match hr.and(env.ok_or(windows::core::Error::from(E_POINTER))) {
            Ok(e) => {
                let active = browser(|b| {
                    b.env = Some(e);
                    b.active
                });
                if let Some(id) = active {
                    tab_load(id); // only the tab on screen: the rest load when you open them
                }
                Ok(())
            }
            Err(e) => inbox_fail(e),
        }
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

/// Add the extensions one by one (re-adding updates them), then open `url`, so ads are blocked
/// from the first video. They belong to the profile, so only the first page of a run adds them.
fn load_extensions(wv: ICoreWebView2, mut dirs: Vec<std::path::PathBuf>, url: String) {
    let Some(dir) = dirs.pop() else {
        browser(|b| b.extensions_done = true);
        let _ = unsafe { wv.Navigate(&HSTRING::from(url)) };
        return;
    };
    let profile = unsafe { wv.cast::<ICoreWebView2_13>().and_then(|w| w.Profile()).and_then(|p| p.cast::<ICoreWebView2Profile7>()) };
    let Ok(profile) = profile else {
        return load_extensions(wv, Vec::new(), url); // runtime too old for extensions: just open it
    };
    let (next, next_url) = (wv.clone(), url.clone());
    let done = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |_, _| {
        load_extensions(next, dirs, next_url); // a broken extension is skipped
        Ok(())
    }));
    if unsafe { profile.AddBrowserExtension(&HSTRING::from(dir.as_os_str()), &done) }.is_err() {
        load_extensions(wv, Vec::new(), url);
    }
}

fn inbox_fail(e: windows::core::Error) -> windows::core::Result<()> {
    send(format!("fail:{}", e.message()));
    Ok(())
}

/// Hand a message to the pill (handled on WM_APP_VIDEO, when App is free).
fn send(s: String) {
    inbox().push(s);
    let _ = unsafe { PostMessageW(Some(HWND(PILL.load(Ordering::Relaxed) as *mut _)), WM_APP_VIDEO, WPARAM(0), LPARAM(0)) };
}

/// A new live page for tab `id`: settings, events, the bar, then its address.
unsafe fn setup(hwnd: HWND, id: u32, ctl: ICoreWebView2Controller) -> windows::core::Result<()> {
    unsafe {
        // the window closed, the tab closed, or you moved to another tab while it was starting
        let state = browser(|b| (b.active == id, b.tabs.iter().any(|t| t.id == id && t.live.is_none()), b.page_dir.clone(), b.extensions_done));
        let Some((true, true, page_dir, extensions_done)) = state.filter(|_| IsWindow(Some(hwnd)).as_bool()) else {
            let _ = ctl.Close();
            return Ok(());
        };
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
        // the tab strip follows each page's title and address
        wv.add_DocumentTitleChanged(
            &DocumentTitleChangedEventHandler::create(Box::new(move |wv, _| {
                let Some(wv) = wv else { return Ok(()) };
                let mut p = PWSTR::null();
                wv.DocumentTitle(&mut p)?;
                let title = take_pwstr(p);
                browser(|b| b.tabs.iter_mut().filter(|t| t.id == id).for_each(|t| t.title = title.clone()));
                send("tabs".into());
                Ok(())
            })),
            &mut token,
        )?;
        wv.add_SourceChanged(
            &SourceChangedEventHandler::create(Box::new(move |wv, _| {
                let Some(wv) = wv else { return Ok(()) };
                let mut p = PWSTR::null();
                wv.Source(&mut p)?;
                let url = take_pwstr(p);
                if web_ok(&url) {
                    browser(|b| b.tabs.iter_mut().filter(|t| t.id == id).for_each(|t| t.url = url.clone()));
                    send("tabs".into());
                }
                Ok(())
            })),
            &mut token,
        )?;
        // sound: a background tab that stops playing is unloaded (it was only kept alive for the sound)
        if let Ok(w8) = wv.cast::<ICoreWebView2_8>() {
            w8.add_IsDocumentPlayingAudioChanged(
                &IsDocumentPlayingAudioChangedEventHandler::create(Box::new(move |wv, _| {
                    let Some(wv) = wv.and_then(|w| w.cast::<ICoreWebView2_8>().ok()) else { return Ok(()) };
                    let mut playing = BOOL(0);
                    wv.IsDocumentPlayingAudio(&mut playing)?;
                    let unload = browser(|b| {
                        let active = b.active;
                        let t = b.tabs.iter_mut().find(|t| t.id == id)?;
                        t.audio = playing.as_bool();
                        if t.id != active && !t.audio { t.live.take() } else { None }
                    })
                    .flatten();
                    if let Some((ctl, _)) = unload {
                        let _ = ctl.Close();
                    }
                    send("tabs".into());
                    Ok(())
                })),
                &mut token,
            )?;
        }
        // only the bar's own messages count: they carry this session's token, which sites never see;
        // and only from the page on screen
        wv.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                if browser(|b| b.active) != Some(id) {
                    return Ok(());
                }
                let mut p = PWSTR::null();
                args.TryGetWebMessageAsString(&mut p)?;
                let Some(s) = take_pwstr(p).strip_prefix(&format!("{}|", session_token())).map(str::to_string) else { return Ok(()) };
                if ["crashed", "fail", "full", "list-result", "list-error", "list-added", "list-add-error", "list-save-side"].iter().any(|k| s.starts_with(k)) {
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
                    if browser(|b| b.active) == Some(id) {
                        send("crashed".into()); // every tab hears it: restart once
                    }
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
                    tab_new(uri, true); // a link that opens a new tab opens a new tab here
                }
                Ok(())
            })),
            &mut token,
        )?;
        let _ = ctl.SetBounds(page_rect(hwnd));
        let url = browser(|b| {
            let t = b.tabs.iter_mut().find(|t| t.id == id)?;
            t.live = Some((ctl.clone(), wv.clone()));
            Some(t.url.clone())
        })
        .flatten()
        .unwrap_or_else(start_page);
        // the bar (every page), the players (our page, YouTube's) and the lists (our lists page)
        let ui = HSTRING::from(format!("{UI}\n{LISTS_UI}").replace("%HOST%", HOST).replace("%TOKEN%", session_token()).replace("%TABSFN%", &tabs_fn()));
        let ui_target = wv.clone();
        let added = AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(move |hr, _| {
            hr?;
            if extensions_done {
                let _ = wv.Navigate(&HSTRING::from(url.clone()));
            } else {
                load_extensions(wv.clone(), extension_dirs(), url.clone());
            }
            Ok(())
        }));
        send("tabs".into());
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

    /// Menu / Ctrl+Alt+L: the smart lists, in the window under the pill.
    pub fn video_lists(&mut self) {
        if self.video.is_none() {
            self.video_open();
        }
        if let Some(v) = &mut self.video {
            v.hidden = false;
        }
        // its own tab: back to it if it's open, else a new one
        let page = lists_page();
        match browser(|b| b.tabs.iter().find(|t| t.url == page).map(|t| t.id)).flatten() {
            Some(id) => tab_show(id),
            None => {
                tab_new(page, true);
            }
        }
        self.video_place();
    }

    /// A browser tab is playing sound (an update waits for it).
    pub fn video_playing(&self) -> bool {
        self.video.is_some() && browser(|b| b.tabs.iter().any(|t| t.audio)).unwrap_or(false)
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
            self.video = Some(Video { hwnd, hidden: false, fresh: true, side: false, list: false, shown: false, rect: RECT::default(), inside: false, cursor: POINT::default(), unlocked: false, applied: (100, false) });
            self.video_place();
            // your tabs from last time; a YouTube/Twitch link you just copied opens in a new tab on top
            let mut urls: Vec<String> = self.cfg.video_tabs.split('\t').filter(|u| web_ok(u)).map(str::to_string).collect();
            let mut active = self.cfg.video_tab;
            let clip = extras::clipboard_text(self.hwnd).filter(|c| classify(c).is_some_and(|l| l.play_json().is_some()) && c.trim() != self.cfg.video_url);
            if clip.is_some() && !urls.is_empty() {
                urls.push(start_page()); // the start page plays the clipboard link when it's ready
                active = urls.len() - 1;
            }
            if let Err(e) = start_webview(hwnd, urls, active) {
                inbox_fail(e).ok();
            }
            SetTimer(Some(self.hwnd), TIMER_VIDEO, 60, None);
        }
    }

    pub fn video_close(&mut self) {
        let Some(v) = self.video.take() else { return };
        if let Some((tabs, active)) = tabs_saved() {
            self.cfg.video_tabs = tabs;
            self.cfg.video_tab = active;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_VIDEO);
            let gone = BROWSER.with(|b| b.try_borrow_mut().ok().and_then(|mut g| g.take()));
            if let Some(b) = gone {
                for (c, _) in b.tabs.into_iter().filter_map(|t| t.live).chain(b.side) {
                    let _ = c.Close();
                }
            }
            let _ = DestroyWindow(v.hwnd);
        }
        inbox().clear();
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
        let side = self.video.as_ref().map_or(0.0, |v| if v.side { CHAT_W * s } else { 0.0 } + if v.list { LIST_W * s } else { 0.0 });
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
        if !inbox().is_empty() {
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
        let away = !active_url().starts_with(&start_page()); // not on our player page
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
        let msgs = std::mem::take(&mut *inbox());
        for m in msgs {
            let (k, val) = m.split_once(':').unwrap_or((&m, ""));
            match k {
                "ready" => {
                    self.video_state();
                    push_tabs();
                    let Some(v) = &mut self.video else { continue };
                    if val != "embed" || active_url().ends_with("#new") {
                        continue; // some site, or a new tab: nothing to play
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
                // ---- tabs ----
                "tabs" => push_tabs(),
                "tab-shown" => {
                    // a page kept alive in the background is back: it re-sends its chat layout with the state
                    if let Some(v) = &mut self.video {
                        v.side = false;
                    }
                    self.video_place();
                    self.video_state();
                }
                "tab" => {
                    if let Ok(id) = val.parse() {
                        tab_show(id);
                    }
                }
                "tab-new" => {
                    tab_new(format!("{}#new", start_page()), true);
                }
                "tab-close" => {
                    if let Ok(id) = val.parse() {
                        tab_close(id);
                    }
                }
                "tab-next" => tab_step(true),
                "tab-prev" => tab_step(false),
                // ---- smart lists (lists.js) ----
                "list-load" => post_to_lists(&format!(r#"{{"lists":{},"region":"{}"}}"#, lists::load(), lists::region())),
                // a save from the lists tab: the list beside the page catches up, and the other way round
                "list-save" | "list-save-side" => {
                    if lists::save(val) {
                        let json = format!(r#"{{"lists":{},"region":"{}"}}"#, lists::load(), lists::region());
                        post_lists(&json, k == "list-save-side", k == "list-save");
                    }
                }
                // the List button: the shopping list beside the page
                "side-list" => {
                    let on = !self.video.as_ref().is_some_and(|v| v.list);
                    if let Some(v) = &mut self.video {
                        v.list = on;
                    }
                    side_set(on);
                    self.video_place();
                }
                // Add to list: the bar sends what it found on the page; the item is worked out off the UI thread
                "list-add" => {
                    bar_toast("Adding it to your list…");
                    let (facts, key, region) = (val.to_string(), extras::load_key("gemini"), lists::region());
                    std::thread::spawn(move || {
                        send(match lists::item_from_page(&facts, key).and_then(|item| {
                            let what = format!("{}{}", item["name"].as_str().unwrap_or("Item"), item["price"].as_f64().map_or(String::new(), |p| format!(" · {p:.2}")));
                            lists::add_item(item, &region).map(|(data, _)| serde_json::json!({"data": data, "toast": format!("Added to your list: {what}")}).to_string()) // not the list's name: this goes to the site's page
                        }) {
                            Ok(json) => format!("list-added:{json}"),
                            Err(e) => format!("list-add-error:{e}"),
                        });
                    });
                }
                "list-added" => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(val) {
                        post_to_lists(&format!(r#"{{"lists":{},"region":"{}"}}"#, v["data"].as_str().unwrap_or("null"), lists::region()));
                        bar_toast(v["toast"].as_str().unwrap_or("Added to your list"));
                    }
                }
                "list-add-error" => bar_toast(val),
                // a link in a list: opens in a tab (a new one when the lists page is the tab on screen)
                "list-open" if web_ok(val) => {
                    if active_url().starts_with(&lists_page()) {
                        tab_new(val.to_string(), true);
                    } else {
                        navigate(val);
                    }
                }
                "list-share" => {
                    let (kind, text) = val.split_once('|').unwrap_or((val, ""));
                    if let Some(url) = lists::share_url(kind, text) {
                        unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL) };
                    }
                }
                "list-prices" => match extras::load_key("gemini") {
                    None => post_to_lists(&format!(r#"{{"pricesErr":{}}}"#, serde_json::json!("Price comparison uses Gemini with Google Search. Get a free key at aistudio.google.com/apikey, copy it, then ⚙ → AI & API keys → Paste API key from clipboard."))),
                    Some(key) => {
                        let request = val.to_string();
                        std::thread::spawn(move || {
                            send(match lists::compare(&key, &request, &lists::region()) {
                                Ok(json) => format!("list-result:{json}"),
                                Err(e) => format!("list-error:{e}"),
                            });
                        });
                    }
                },
                "list-result" => post_to_lists(&format!(r#"{{"prices":{val}}}"#)),
                "list-error" => post_to_lists(&format!(r#"{{"pricesErr":{}}}"#, serde_json::json!(val))),
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
