//! Now playing: whatever app shows in Windows' media flyout (Spotify, a browser tab, …), read
//! through the system media controls. Free, no sign-in. A small thread polls once a second and
//! runs play/pause/next/previous/shuffle.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Media::Control::{GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager, GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status};
use windows::Storage::Streams::DataReader;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

#[derive(Clone, Default, PartialEq)]
pub struct Now {
    pub present: bool,
    pub title: String,
    pub artist: String,
    pub app: String, // e.g. "Spotify"
    pub playing: bool,
    pub shuffle: Option<bool>, // None: the app doesn't offer shuffle
    pub art: Option<Arc<Vec<u8>>>, // encoded cover image (JPEG/PNG)
    pub art_id: u32, // changes whenever `art` does
    pub pos: f64,    // seconds into the song when the player last reported it
    pub len: f64,    // song length in seconds (0: unknown, e.g. a live stream)
    pub stamp: i64,  // when `pos` was reported (FILETIME, 100 ns since 1601)
    pub volume: Option<(f32, bool)>, // 0..1, and whether it is the app's own (false: the PC's)
}

impl Now {
    /// Where the song is now: players report the position now and then, so run it on while playing.
    pub fn position(&self) -> f64 {
        let mut p = self.pos;
        if self.playing && self.stamp > 0 {
            let ft = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
            let now = ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64;
            p += (now - self.stamp).max(0) as f64 / 1e7;
        }
        if self.len > 0.0 { p.min(self.len) } else { p }
    }
}

/// "3:07"
pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

#[derive(Clone, Copy)]
pub enum Cmd {
    PlayPause,
    Next,
    Prev,
    Shuffle,
    Volume(f32), // set, 0..1
}

pub fn spawn(hwnd: HWND, msg: u32) -> (Arc<Mutex<Now>>, Sender<Cmd>) {
    let now = Arc::new(Mutex::new(Now::default()));
    let (tx, rx) = channel();
    let shared = now.clone();
    let h = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("media".into()).spawn(move || {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let notify = move || unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(0), LPARAM(0));
        };
        run(&shared, &rx, notify);
    });
    (now, tx)
}

fn run(shared: &Mutex<Now>, rx: &Receiver<Cmd>, notify: impl Fn()) {
    let Ok(mgr) = Manager::RequestAsync().and_then(|op| op.join()) else { return };
    let mut art_tries = 0;
    let mut old_cover: Option<Arc<Vec<u8>>> = None; // the previous song's picture (often still served at first)
    let mut art_retry = std::time::Instant::now();
    loop {
        // a command, or a poll every second
        let cmd = match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(c) => Some(c),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(_) => return, // the app is closing
        };
        let session = mgr.GetCurrentSession().ok();
        let app_id = session.as_ref().and_then(|s| s.SourceAppUserModelId().ok()).map(|h| h.to_string().to_ascii_lowercase()).unwrap_or_default();
        match (cmd, &session) {
            (Some(Cmd::Volume(v)), Some(_)) => volume::set(&app_id, v),
            (Some(c), Some(s)) => {
                let _ = command(s, c);
                std::thread::sleep(Duration::from_millis(150)); // let the player catch up before reading
            }
            _ => {}
        }
        let prev = shared.lock().map(|n| n.clone()).unwrap_or_default();
        let mut next = session.as_ref().and_then(|s| read(s, &prev).ok()).unwrap_or_default();
        if next.present {
            next.volume = volume::get(&app_id);
        }
        // cover art. Players often hand over the new cover a moment after the song changes (at
        // first still the old one), so keep looking for ~12 s and take whatever is newest; a song
        // still without a cover is retried every 5 s.
        let new_song = next.present && (next.title != prev.title || next.artist != prev.artist);
        if new_song {
            art_tries = 12;
            old_cover = prev.art.clone();
            next.art = None; // no stale cover from the last song
            next.art_id = prev.art_id.wrapping_add(1);
        } else if next.present && next.art.is_none() && art_tries == 0 && art_retry.elapsed() > Duration::from_secs(5) {
            art_tries = 1;
            art_retry = std::time::Instant::now();
        }
        if art_tries > 0 {
            art_tries -= 1;
            if let Some(bytes) = session.as_ref().and_then(|s| art(s).ok()) {
                // the first second or two may still be the previous song's picture: skip it
                let stale = art_tries > 9 && old_cover.as_deref() == Some(&bytes);
                if !stale && next.art.as_deref() != Some(&bytes) {
                    next.art = Some(Arc::new(bytes));
                    next.art_id = next.art_id.wrapping_add(1);
                }
            }
        }
        if next != prev {
            if let Ok(mut n) = shared.lock() {
                *n = next;
            }
            notify();
        }
    }
}

fn command(s: &Session, c: Cmd) -> windows::core::Result<()> {
    match c {
        Cmd::PlayPause => s.TryTogglePlayPauseAsync()?.join().map(|_| ()),
        Cmd::Next => s.TrySkipNextAsync()?.join().map(|_| ()),
        Cmd::Prev => s.TrySkipPreviousAsync()?.join().map(|_| ()),
        Cmd::Shuffle => {
            let on = s.GetPlaybackInfo()?.IsShuffleActive().and_then(|r| r.Value()).unwrap_or(false);
            s.TryChangeShuffleActiveAsync(!on)?.join().map(|_| ())
        }
        Cmd::Volume(_) => Ok(()), // handled by `volume`
    }
}

/// The playing app's own volume (its slider in Windows' volume mixer), found by matching the
/// app's audio sessions to the media app; the PC's volume when no session matches.
pub mod volume {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    use windows::core::{Interface, PWSTR};

    pub fn device() -> Option<IMMDevice> {
        unsafe {
            let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            e.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()
        }
    }

    /// "spotify" for a process running …\Spotify.exe
    pub fn exe_stem(pid: u32) -> Option<String> {
        exe_path(pid).and_then(|p| p.rsplit('\\').next().map(|f| f.trim_end_matches(".exe").trim_end_matches(".EXE").to_ascii_lowercase()))
    }

    /// Full path of a process's program file (for its icon).
    pub fn exe_path(pid: u32) -> Option<String> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 512];
            let mut n = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).is_ok();
            let _ = CloseHandle(h);
            ok.then(|| String::from_utf16_lossy(&buf[..n as usize]))
        }
    }

    /// Volume controls of the audio sessions belonging to the media app.
    fn sessions(app_id: &str) -> Vec<ISimpleAudioVolume> {
        let mut out = Vec::new();
        let Some(dev) = device() else { return out };
        unsafe {
            let Ok(mgr) = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { return out };
            let Ok(list) = mgr.GetSessionEnumerator() else { return out };
            for i in 0..list.GetCount().unwrap_or(0) {
                let Ok(ctl) = list.GetSession(i) else { continue };
                let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else { continue };
                let pid = ctl2.GetProcessId().unwrap_or(0);
                if pid == 0 {
                    continue; // system sounds
                }
                if exe_stem(pid).is_some_and(|stem| stem.len() >= 3 && app_id.contains(&stem)) {
                    if let Ok(v) = ctl.cast::<ISimpleAudioVolume>() {
                        out.push(v);
                    }
                }
            }
        }
        out
    }

    pub fn master() -> Option<IAudioEndpointVolume> {
        unsafe { device()?.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok() }
    }

    pub fn get(app_id: &str) -> Option<(f32, bool)> {
        unsafe {
            if let Some(v) = sessions(app_id).first() {
                return v.GetMasterVolume().ok().map(|x| (x, true));
            }
            master()?.GetMasterVolumeLevelScalar().ok().map(|x| (x, false))
        }
    }

    pub fn set(app_id: &str, level: f32) {
        let level = level.clamp(0.0, 1.0);
        let s = sessions(app_id);
        unsafe {
            if s.is_empty() {
                if let Some(m) = master() {
                    let _ = m.SetMasterVolumeLevelScalar(level, std::ptr::null());
                }
            }
            for v in s {
                let _ = v.SetMasterVolume(level, std::ptr::null());
            }
        }
    }
}

/// Title, artist and state; keeps the previous cover until the art step decides.
fn read(s: &Session, prev: &Now) -> windows::core::Result<Now> {
    let info = s.GetPlaybackInfo()?;
    let props = s.TryGetMediaPropertiesAsync()?.join()?;
    let title = props.Title()?.to_string();
    if title.is_empty() {
        return Ok(Now::default());
    }
    let shuffle_ok = info.Controls().and_then(|c| c.IsShuffleEnabled()).unwrap_or(false);
    Ok(Now {
        present: true,
        title,
        artist: props.Artist()?.to_string(),
        app: app_name(&s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default()),
        playing: info.PlaybackStatus()? == Status::Playing,
        shuffle: if shuffle_ok { Some(info.IsShuffleActive().and_then(|r| r.Value()).unwrap_or(false)) } else { None },
        art: prev.art.clone(),
        art_id: prev.art_id,
        ..timeline(s).unwrap_or_default()
    })
}

/// Song position and length from the player's timeline.
fn timeline(s: &Session) -> windows::core::Result<Now> {
    let t = s.GetTimelineProperties()?;
    let start = t.StartTime()?.Duration;
    Ok(Now {
        pos: (t.Position()?.Duration - start) as f64 / 1e7,
        len: ((t.EndTime()?.Duration - start) as f64 / 1e7).max(0.0),
        stamp: t.LastUpdatedTime()?.UniversalTime,
        ..Default::default()
    })
}

fn art(s: &Session) -> windows::core::Result<Vec<u8>> {
    let props = s.TryGetMediaPropertiesAsync()?.join()?;
    let stream = props.Thumbnail()?.OpenReadAsync()?.join()?;
    // some players don't say how big the picture is: read in chunks until it ends (8 MB at most)
    let reader = DataReader::CreateDataReader(&stream)?;
    let mut buf = Vec::new();
    loop {
        let n = reader.LoadAsync(64 * 1024)?.join()? as usize;
        if n == 0 {
            break;
        }
        let at = buf.len();
        buf.resize(at + n, 0);
        reader.ReadBytes(&mut buf[at..])?;
        if buf.len() > 8 << 20 {
            return Err(windows::core::Error::empty());
        }
    }
    if buf.is_empty() {
        return Err(windows::core::Error::empty());
    }
    Ok(buf)
}

/// "Spotify.exe" / "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify" / "MSEdge" → a friendly name.
pub fn app_name(id: &str) -> String {
    let low = id.to_ascii_lowercase();
    for (k, v) in [("spotify", "Spotify"), ("chrome", "Chrome"), ("msedge", "Edge"), ("firefox", "Firefox"), ("zunemusic", "Media Player"), ("vlc", "VLC"), ("brave", "Brave"), ("opera", "Opera")] {
        if low.contains(k) {
            return v.into();
        }
    }
    let base = id.rsplit(['!', '\\']).next().unwrap_or(id);
    base.trim_end_matches(".exe").to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn friendly_app_names() {
        assert_eq!(super::app_name("Spotify.exe"), "Spotify");
        assert_eq!(super::app_name("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "Spotify");
        assert_eq!(super::app_name("MSEdge"), "Edge");
        assert_eq!(super::app_name("C:\\Apps\\Tidal.exe"), "Tidal");
        assert_eq!(super::clock(187.9), "3:07");
        let paused = super::Now { pos: 42.0, len: 40.0, ..Default::default() };
        assert_eq!(paused.position(), 40.0, "never past the end");
    }
}

#[cfg(test)]
mod live_art {
    /// `cargo test --release live_cover -- --ignored --nocapture`: what the player gives us as a cover right now.
    #[test]
    #[ignore]
    fn live_cover() {
        let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
        let mgr = super::Manager::RequestAsync().unwrap().join().unwrap();
        let s = mgr.GetCurrentSession().expect("a media session");
        let props = s.TryGetMediaPropertiesAsync().unwrap().join().unwrap();
        println!("song: {} / {}", props.Title().unwrap(), props.Artist().unwrap());
        match props.Thumbnail() {
            Err(e) => println!("no thumbnail reference: {e}"),
            Ok(t) => match t.OpenReadAsync().and_then(|op| op.join()) {
                Err(e) => println!("thumbnail won't open: {e}"),
                Ok(st) => println!("thumbnail stream: size {:?}, type {:?}", st.Size(), st.ContentType()),
            },
        }
        for i in 0..4 {
            match super::art(&s) {
                Ok(b) => {
                    println!("try {i}: {} bytes, starts {:02X?}, palette {:?}", b.len(), &b[..8.min(b.len())], crate::App::art_palette(&b));
                }
                Err(e) => println!("try {i}: no cover ({e})"),
            }
            std::thread::sleep(std::time::Duration::from_millis(700));
        }
    }
}

#[cfg(test)]
mod live_sessions_list {
    /// `cargo test --release live_media -- --ignored --nocapture`: every app in Windows' media controls.
    #[test]
    #[ignore]
    fn live_media() {
        let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
        let mgr = super::Manager::RequestAsync().unwrap().join().unwrap();
        let cur = mgr.GetCurrentSession().ok().and_then(|s| s.SourceAppUserModelId().ok()).map(|h| h.to_string()).unwrap_or_default();
        println!("current: {cur}");
        for s in mgr.GetSessions().unwrap() {
            let id = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
            let st = s.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()).map(|p| p.0).unwrap_or(-1);
            let title = s.TryGetMediaPropertiesAsync().and_then(|o| o.join()).and_then(|p| p.Title()).map(|t| t.to_string()).unwrap_or_default();
            println!("media session: {id:<40} status {st} (4 = playing)  {title}");
        }
    }
}
