//! Auto-duck: when someone talks in a voice or call app (Discord, Teams, Zoom…), music apps
//! (Spotify…) glide down just enough for the voice to come through, then ease back up once it
//! has been quiet for a moment. Built to go unnoticed:
//! - the volume moves on a soft S-curve in decibels (how loudness is heard), 50 times a second;
//! - how far it dips depends on how loud the music is next to the voice: loud music dips more,
//!   quiet music barely moves;
//! - "is someone talking?" comes from a small AI voice detector (Silero, in the helper, on the
//!   call app's audio only, never saved) or, without it, from the level meters (background
//!   noise learned and ignored).

use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::Interface;

use crate::media::volume;

const VOICE: [&str; 9] = ["discord", "teams", "ms-teams", "zoom", "slack", "skype", "whatsapp", "telegram", "signal"];
const MUSIC: [&str; 10] = ["spotify", "tidal", "deezer", "applemusic", "itunes", "amazon music", "musicbee", "foobar2000", "aimp", "vlc"];

const TICK_MS: f32 = 20.0; // 50 updates a second while listening or moving

// --- how far to dip follows how loud the talking is ---
// soft voices (a video murmuring in the background) dip the music a little; full-on talking a lot
// (the real call audio's average level, measured by the AI listener)
const SOFT_VOICE_DB: f32 = -45.0; // this quiet or softer counts as "soft"…
const FULL_VOICE_DB: f32 = -20.0; // …this loud or louder as "full-on talking"
const SOFT_DIP_DB: f32 = 3.0; // a gentle step down for soft voices
const FULL_DIP_DB: f32 = 14.0; // well out of the way for real conversation
const DEFAULT_DIP_DB: f32 = -10.5; // when there's nothing to measure yet

/// How far (dB, positive) to dip for a voice at `voice_db`: a straight line from soft to full.
fn dip_for(voice_db: f32) -> f32 {
    let t = ((voice_db - SOFT_VOICE_DB) / (FULL_VOICE_DB - SOFT_VOICE_DB)).clamp(0.0, 1.0);
    SOFT_DIP_DB + (FULL_DIP_DB - SOFT_DIP_DB) * t
}

// --- telling talking from background noise by level (all in dB) ---
const ENV_ATTACK_MS: f32 = 15.0; // the level envelope follows rises almost at once…
const ENV_RELEASE_MS: f32 = 120.0; // …and falls smoothly, so single readings don't jitter
const FLOOR_WINDOW: usize = 150; // noise = the quietest moment of the last 3 s (150 ticks)…
const FLOOR_RISE_MS: f32 = 6000.0; // …rising slowly when the noise really gets louder
const OPEN_ABOVE: f32 = 9.0; // talking starts: this many dB above the noise…
const OPEN_MS: f32 = 60.0; // …for at least this long (clicks and pops are shorter)
const CLOSE_ABOVE: f32 = 5.0; // still talking while above noise + 5 dB…
const HOLD_MS: f32 = 700.0; // …and through pauses between words up to this long
const QUIETEST_DB: f32 = -40.0; // never counts anything quieter than this as talking
const AI_HOLD_MS: u64 = 350; // with the AI: extra calm through short gaps (it already waits 0.45 s)

// --- how the music moves: two smoothing stages give an S-curve (soft start, soft landing) ---
const DIP_MS: f32 = 45.0; // per stage: the dip lands in about 0.2 s
const RISE_MS: f32 = 550.0; // per stage: the return eases up over about 2.5 s
/// What an app does for auto-lower.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Role {
    Voice,   // people talk here: it triggers the lowering
    Lowered, // music / videos: turned down while someone talks
    Ignored, // left alone
}

impl Role {
    pub fn code(self) -> char {
        match self {
            Role::Voice => 'v',
            Role::Lowered => 'l',
            Role::Ignored => 'i',
        }
    }

    pub fn from_code(c: char) -> Option<Role> {
        match c {
            'v' => Some(Role::Voice),
            'l' => Some(Role::Lowered),
            'i' => Some(Role::Ignored),
            _ => None,
        }
    }
}

/// The role Peter picks by itself: call apps are voices, music apps get lowered.
#[cfg(test)]
pub fn auto_role(stem: &str) -> Role {
    auto_role_playing(stem, "")
}

/// Same, knowing which app owns Windows' media controls right now (`playing`, as the mixer
/// names it, e.g. "Chrome" for a YouTube tab): that app is media too, so it gets lowered.
pub fn auto_role_playing(stem: &str, playing: &str) -> Role {
    if is(&VOICE, stem) {
        Role::Voice
    } else if is(&MUSIC, stem) || (!playing.is_empty() && crate::mixer::display_name(stem) == playing) {
        Role::Lowered
    } else {
        Role::Ignored
    }
}

/// "spotify=l;chrome=v" (your choices, saved in the settings) → list.
pub fn parse_roles(s: &str) -> Vec<(String, Role)> {
    s.split(';').filter_map(|p| p.split_once('=')).filter_map(|(k, v)| Some((k.trim().to_ascii_lowercase(), Role::from_code(v.trim().chars().next()?)?))).filter(|(k, _)| !k.is_empty()).collect()
}

pub fn format_roles(roles: &[(String, Role)]) -> String {
    roles.iter().map(|(k, r)| format!("{k}={}", r.code())).collect::<Vec<_>>().join(";")
}


/// Shared with the pill: switches in, state out.
pub struct Shared {
    pub on: AtomicBool,   // the auto-lower switch
    pub ai: AtomicBool,   // the "smarter detection (AI)" switch
    pub stop: AtomicBool, // the pill is closing
    pub factor: AtomicU32, // f32 bits: music now ÷ your level (1 = not lowered)
    pub own: Mutex<Vec<(String, f32)>>, // while lowered: app name → your level (for the mixer)
    pub mode: AtomicU8,   // 0 idle, 1 listening with the AI, 2 listening by level
    pub roles: Mutex<Vec<(String, Role)>>, // your choices by program name (the rest: auto)
    pub roles_ver: AtomicU32, // bumped when you change a role
    pub talker: Mutex<Option<(String, String)>>, // who is being made room for: (app name, program path)
    pub ducking: AtomicBool, // music is lowered right now
    pub notify: AtomicIsize, // the pill's window, told when lowering starts / ends
    pub media_app: Mutex<String>, // the app playing in Windows' media controls (e.g. "Chrome"): lowered too
}

impl Shared {
    pub fn new() -> Shared {
        Shared {
            on: false.into(),
            ai: false.into(),
            stop: false.into(),
            factor: AtomicU32::new(1f32.to_bits()),
            own: Mutex::new(Vec::new()),
            mode: 0.into(),
            roles: Mutex::new(Vec::new()),
            roles_ver: 0.into(),
            talker: Mutex::new(None),
            ducking: false.into(),
            notify: 0.into(),
            media_app: Mutex::new(String::new()),
        }
    }

    /// The role an app plays: your choice, else the automatic one.
    pub fn role(&self, stem: &str) -> (Role, bool) {
        match self.roles.lock().ok().and_then(|r| r.iter().find(|(k, _)| k == stem).map(|(_, r)| *r)) {
            Some(r) => (r, false),
            None => (auto_role_playing(stem, &self.media_app.lock().map(|m| m.clone()).unwrap_or_default()), true),
        }
    }

    /// The app now playing in Windows' media controls changed (it is lowered automatically).
    pub fn set_media_app(&self, name: &str) {
        if let Ok(mut m) = self.media_app.lock() {
            if *m != name {
                *m = name.to_string();
                self.roles_ver.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn set_roles(&self, roles: Vec<(String, Role)>) {
        if let Ok(mut r) = self.roles.lock() {
            *r = roles;
        }
        self.roles_ver.fetch_add(1, Ordering::Relaxed);
    }

    pub fn factor(&self) -> f32 {
        f32::from_bits(self.factor.load(Ordering::Relaxed))
    }

    /// Your level for a lowered app (what the mixer should show), if it is lowered right now.
    pub fn own_level(&self, name: &str) -> Option<f32> {
        if self.factor() >= 0.999 {
            return None;
        }
        self.own.lock().ok()?.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
    }
}

struct Music {
    name: String,
    stem: String, // program name (for the safety note)
    vol: ISimpleAudioVolume,
    own: f32, // the level you set (restored afterwards)
    set: f32, // the level we last applied (to notice when you change it yourself)
}

/// The helper listening to the call app with the AI voice detector.
struct Ai {
    child: Child,
    ready: Arc<AtomicBool>,
    apps: Arc<Mutex<Vec<(String, bool, f32)>>>, // per voice app: (name, talking now, level in dB)
    done: Arc<AtomicBool>,
}

impl Ai {
    fn start(apps: &[String]) -> Option<Ai> {
        let exe = crate::recorder::helper()?;
        let models = crate::recorder::models_dir();
        if !models.join("vad").join("silero_vad.onnx").exists() {
            return None; // the voice detector comes with the transcriber's models
        }
        let mut child = Command::new(exe)
            .args(["--listen", "--models", &models.to_string_lossy(), "--voice-apps", &apps.join(",")])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000) // no console window
            .spawn()
            .ok()?;
        let out = child.stdout.take()?;
        let (ready, done) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let apps: Arc<Mutex<Vec<(String, bool, f32)>>> = Arc::new(Mutex::new(Vec::new()));
        let (r, d, s) = (ready.clone(), done.clone(), apps.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                // READY / SPEECH <app> 1|0 / VOICE <app> <dB>
                let f: Vec<&str> = line.split('\t').collect();
                let mut apps = s.lock().unwrap_or_else(|e| e.into_inner());
                let mut entry = |app: &str| -> usize {
                    apps.iter().position(|a| a.0 == app).unwrap_or_else(|| {
                        apps.push((app.to_string(), false, -90.0));
                        apps.len() - 1
                    })
                };
                match (f.first().copied(), f.get(1).copied(), f.get(2).copied()) {
                    (Some("READY"), ..) => r.store(true, Ordering::Relaxed),
                    (Some("SPEECH"), Some(app), Some(v)) => {
                        let i = entry(app);
                        apps[i].1 = v == "1";
                    }
                    (Some("VOICE"), Some(app), Some(v)) => {
                        if let Ok(db) = v.parse::<f32>() {
                            let i = entry(app);
                            apps[i].2 = db;
                        }
                    }
                    _ => {}
                }
            }
            d.store(true, Ordering::Relaxed);
        });
        Some(Ai { child, ready, apps, done })
    }

    /// Who is talking now (the loudest of the voice apps with voices on), with their level.
    fn talking(&self) -> Option<(String, f32)> {
        let apps = self.apps.lock().ok()?;
        apps.iter().filter(|a| a.1).max_by(|a, b| a.2.total_cmp(&b.2)).map(|a| (a.0.clone(), a.2))
    }
}

impl Drop for Ai {
    fn drop(&mut self) {
        drop(self.child.stdin.take()); // the helper exits when its input closes…
        let _ = self.child.kill(); // …or right now
        let _ = self.child.wait();
    }
}

/// Runs until `stop` is set; the switches can be flipped at any time.
pub fn spawn(sh: Arc<Shared>) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("duck".into())
        .spawn(move || {
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            restore_after_crash();
            let mut voices: Vec<Voice> = Vec::new();
            let mut music: Vec<Music> = Vec::new();
            let mut scanned = Instant::now() - Duration::from_secs(10);
            let mut roles_seen = u32::MAX;
            let mut ear = Listener::new();
            let mut gain = Gain::default();
            let mut ai: Option<Ai> = None;
            let mut ai_apps: Vec<String> = Vec::new();
            let mut ai_retry = Instant::now();
            let mut last_speech = Instant::now() - Duration::from_secs(10);
            let mut dip = DEFAULT_DIP_DB;
            let mut applied = 0.0f32; // the dip (dB) last sent to Windows
            let mut checked = Instant::now(); // when we last read the music level back
            loop {
                let running = !sh.stop.load(Ordering::Relaxed);
                let enabled = running && sh.on.load(Ordering::Relaxed);
                // apps come and go (and you can change roles): re-scan every 2 s, or at once after a
                // role change, only while the music is at your level
                let ver = sh.roles_ver.load(Ordering::Relaxed);
                if enabled && gain.at_rest() && (scanned.elapsed() > Duration::from_secs(2) || ver != roles_seen) {
                    (voices, music) = scan(&sh);
                    scanned = Instant::now();
                    roles_seen = ver;
                }
                // the AI listener runs only while it's wanted and a voice app is open
                let want_ai = enabled && sh.ai.load(Ordering::Relaxed) && !voices.is_empty();
                let mut apps: Vec<String> = voices.iter().map(|v| v.stem.clone()).collect();
                apps.dedup();
                // real call apps first: the AI listener hears the first one that is playing
                apps.sort_by_key(|s| VOICE.iter().position(|v| s == v || s.starts_with(v)).unwrap_or(VOICE.len()));
                if ai.as_ref().is_some_and(|a| a.done.load(Ordering::Relaxed)) {
                    ai = None; // it stopped (voice app closed?): try again in a while
                    ai_retry = Instant::now() + Duration::from_secs(10);
                }
                if ai.is_some() && apps != ai_apps {
                    ai = None; // you changed which apps are voices: listen to the new ones
                    ai_retry = Instant::now();
                }
                if want_ai && ai.is_none() && Instant::now() >= ai_retry {
                    ai = Ai::start(&apps);
                    ai_apps = apps.clone();
                    ai_retry = Instant::now() + Duration::from_secs(10);
                }
                if !want_ai {
                    ai = None;
                }
                let ai_on = ai.as_ref().is_some_and(|a| a.ready.load(Ordering::Relaxed));
                sh.mode.store(if !enabled || voices.is_empty() { 0 } else if ai_on { 1 } else { 2 }, Ordering::Relaxed);

                // the loudest voice app right now (also: who to show on the pill)
                let peaks: Vec<f32> = voices.iter().map(|v| unsafe { v.meter.GetPeakValue() }.unwrap_or(0.0)).collect();
                let (loudest, peak) = peaks.iter().copied().enumerate().fold((0, 0.0f32), |b, (i, p)| if p > b.1 { (i, p) } else { b });
                let by_level = ear.hear(peak); // always fed, so it's ready if the AI stops
                // with the AI: who is talking (real audio, per app); without: the level meters
                let who = ai.as_ref().filter(|_| ai_on).and_then(|a| a.talking());
                let heard = if ai_on { who.is_some() } else { by_level };
                if heard {
                    last_speech = Instant::now();
                }
                let hold = if ai_on { Duration::from_millis(AI_HOLD_MS) } else { Duration::ZERO };
                let talking = enabled && !voices.is_empty() && !music.is_empty() && (heard || last_speech.elapsed() < hold);
                if talking && heard {
                    let v = match &who {
                        Some((app, _)) => voices.iter().find(|v| v.stem == *app),
                        None => voices.get(loudest),
                    };
                    if let (Some(v), Ok(mut t)) = (v, sh.talker.lock()) {
                        if t.as_ref().is_none_or(|(n, _)| *n != v.name) {
                            *t = Some((v.name.clone(), v.path.clone()));
                        }
                    }
                }

                // how far to dip follows how loud the talking is: soft voices a little, full-on talking a lot
                // the voice level: the AI listener's real call audio (the shared meters also hear the music)
                let voice_db = match &who {
                    Some((_, db)) => *db,
                    None if ai_on => f32::NAN, // between words: keep the depth where it is
                    None => ear.env,
                };
                if talking && voice_db.is_finite() {
                    let target = -dip_for(voice_db);
                    dip += (target - dip) * coef(400.0); // the depth glides too
                }
                let was_rest = gain.at_rest();
                let db = gain.step(if talking { dip } else { 0.0 });
                // only tell Windows when the level has moved by a hearable step (0.1 dB): the curve is
                // just as smooth, without dozens of needless volume calls a second while it holds
                if !(was_rest && gain.at_rest()) && (was_rest || gain.at_rest() || (db - applied).abs() >= 0.1) {
                    let check = checked.elapsed() >= Duration::from_millis(250);
                    if check {
                        checked = Instant::now();
                    }
                    apply(&mut music, db, was_rest, check, &sh);
                    applied = db;
                }
                // tell the pill when lowering starts and ends (its badge)
                let ducking = !gain.at_rest();
                if ducking != sh.ducking.swap(ducking, Ordering::Relaxed) {
                    let h = sh.notify.load(Ordering::Relaxed);
                    if h != 0 {
                        unsafe {
                            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(windows::Win32::Foundation::HWND(h as *mut _)), crate::WM_APP_DUCK, Default::default(), Default::default());
                        }
                    }
                }
                if !running {
                    // closing: music straight back to your level
                    if !was_rest {
                        apply(&mut music, 0.0, false, true, &sh);
                    }
                    sh.factor.store(1f32.to_bits(), Ordering::Relaxed);
                    return;
                }
                let idle = gain.at_rest() && (voices.is_empty() || !enabled);
                std::thread::sleep(Duration::from_millis(if idle { 400 } else { TICK_MS as u64 }));
            }
        })
        .ok()
}

/// Set each music app to its own level lowered by `db`; picks up changes you make yourself.
fn apply(music: &mut [Music], db: f32, starting: bool, check: bool, sh: &Shared) {
    let k = 10f32.powf(db / 20.0);
    for m in music.iter_mut() {
        // reading the level back costs a call to Windows: only now and then (to notice your own changes)
        let now = if starting || check { unsafe { m.vol.GetMasterVolume() }.unwrap_or(m.set) } else { m.set };
        if starting {
            m.own = now; // first move: remember where you had it
        } else if (now - m.set).abs() > 0.015 && m.set > 0.0 && m.own > 0.0 {
            m.own = (now / (m.set / m.own)).clamp(0.0, 1.0); // you moved it mid-dip: keep your new level
        }
        m.set = (m.own * k).clamp(0.0, 1.0);
        unsafe {
            let _ = m.vol.SetMasterVolume(m.set, std::ptr::null());
        }
    }
    sh.factor.store(k.to_bits(), Ordering::Relaxed);
    if let Ok(mut own) = sh.own.lock() {
        *own = music.iter().map(|m| (m.name.clone(), m.own)).collect();
    }
    // safety note: if Peter is closed by force mid-dip, the next start puts your levels back
    if let Some(p) = safety_path() {
        if k >= 0.999 {
            let _ = std::fs::remove_file(p);
        } else if starting || check {
            let _ = std::fs::write(p, music.iter().map(|m| format!("{}={}\n", m.stem, m.own)).collect::<String>());
        }
    }
}

fn safety_path() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(std::env::var_os("APPDATA")?).join("ResourcePill").join("lowered.txt"))
}

/// At start: music left lowered by a forced close (the safety note exists) goes back to your level.
fn restore_after_crash() {
    let Some(p) = safety_path() else { return };
    let Ok(text) = std::fs::read_to_string(&p) else { return };
    let levels: Vec<(String, f32)> = text.lines().filter_map(|l| l.split_once('=')).filter_map(|(k, v)| Some((k.to_string(), v.parse().ok()?))).collect();
    unsafe {
        let list = volume::device().and_then(|d| d.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None).ok()).and_then(|m| m.GetSessionEnumerator().ok());
        for i in 0..list.as_ref().map_or(0, |l| l.GetCount().unwrap_or(0)) {
            let Some(ctl) = list.as_ref().and_then(|l| l.GetSession(i).ok()) else { continue };
            let Some(stem) = ctl.cast::<IAudioSessionControl2>().ok().and_then(|c| c.GetProcessId().ok()).and_then(volume::exe_stem) else { continue };
            if let (Some((_, own)), Ok(v)) = (levels.iter().find(|(k, _)| *k == stem), ctl.cast::<ISimpleAudioVolume>()) {
                let _ = v.SetMasterVolume(own.clamp(0.0, 1.0), std::ptr::null());
            }
        }
    }
    let _ = std::fs::remove_file(p);
}

/// A voice app's level meter, with what the pill needs to show it.
struct Voice {
    meter: IAudioMeterInformation,
    stem: String, // program name, e.g. "discord"
    name: String, // shown, e.g. "Discord"
    path: String, // program file (for its icon)
}

/// The voice apps' level meters and the lowered apps' volume controls right now (roles: yours,
/// else automatic).
fn scan(sh: &Shared) -> (Vec<Voice>, Vec<Music>) {
    let (mut voices, mut music) = (Vec::new(), Vec::new());
    unsafe {
        let Some(list) = volume::device().and_then(|d| d.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None).ok()).and_then(|m| m.GetSessionEnumerator().ok()) else { return (voices, music) };
        for i in 0..list.GetCount().unwrap_or(0) {
            let Ok(ctl) = list.GetSession(i) else { continue };
            if ctl.GetState().is_ok_and(|s| s == AudioSessionStateExpired) {
                continue;
            }
            let Ok(c2) = ctl.cast::<IAudioSessionControl2>() else { continue };
            let pid = c2.GetProcessId().unwrap_or(0);
            let Some(path) = (pid != 0).then(|| volume::exe_path(pid)).flatten() else { continue };
            let stem = path.rsplit('\\').next().unwrap_or(&path).trim_end_matches(".exe").trim_end_matches(".EXE").to_ascii_lowercase();
            let name = crate::mixer::display_name(&stem);
            match sh.role(&stem).0 {
                Role::Voice => {
                    if let Ok(meter) = ctl.cast::<IAudioMeterInformation>() {
                        voices.push(Voice { meter, stem, name, path });
                    }
                }
                Role::Lowered => {
                    if let Ok(v) = ctl.cast::<ISimpleAudioVolume>() {
                        let own = v.GetMasterVolume().unwrap_or(1.0);
                        music.push(Music { name, stem: stem.clone(), vol: v, own, set: own });
                    }
                }
                Role::Ignored => {}
            }
        }
    }
    (voices, music)
}

/// One-pole smoothing factor for a time constant, per tick.
fn coef(ms: f32) -> f32 {
    1.0 - (-TICK_MS / ms).exp()
}

/// Tells talking from background noise using only the level, in dB:
/// - a smooth envelope of the level (fast up, gentle down);
/// - the noise floor = the quietest envelope of the last 3 s (pauses between words show it),
///   allowed to rise only slowly when the room really gets louder;
/// - talking starts when the level stays 9 dB above the noise for 60 ms (clicks and pops are
///   shorter), continues while 5 dB above it, and holds through pauses up to 0.7 s.
struct Listener {
    env: f32,
    floor: f32,
    recent: std::collections::VecDeque<f32>,
    above_ms: f32,
    quiet_ms: f32,
    open: bool,
}

impl Listener {
    fn new() -> Listener {
        // floor starts "unknown" (very high): the first readings set it, nothing counts until then
        Listener { env: f32::NAN, floor: 200.0, recent: Default::default(), above_ms: 0.0, quiet_ms: HOLD_MS, open: false }
    }

    /// Feed one peak reading (0..1); returns whether someone is talking.
    fn hear(&mut self, peak: f32) -> bool {
        let db = 20.0 * peak.max(1e-5).log10();
        self.env = if self.env.is_nan() { db } else { self.env + (db - self.env) * coef(if db > self.env { ENV_ATTACK_MS } else { ENV_RELEASE_MS }) };
        self.recent.push_back(self.env);
        if self.recent.len() > FLOOR_WINDOW {
            self.recent.pop_front();
        }
        let quietest = self.recent.iter().copied().fold(f32::MAX, f32::min);
        self.floor = if quietest < self.floor { quietest } else { self.floor + (quietest - self.floor) * coef(FLOOR_RISE_MS) };
        let open_at = (self.floor + OPEN_ABOVE).max(QUIETEST_DB);
        let close_at = (self.floor + CLOSE_ABOVE).max(QUIETEST_DB - 4.0);
        // starting needs the raw level to stay up (a click is one reading; a syllable is many)
        self.above_ms = if db > open_at { self.above_ms + TICK_MS } else { 0.0 };
        if self.env > close_at && (self.open || self.above_ms >= OPEN_MS) {
            self.open = true;
            self.quiet_ms = 0.0;
        } else {
            self.quiet_ms += TICK_MS;
            if self.quiet_ms >= HOLD_MS {
                self.open = false;
            }
        }
        self.open
    }
}

/// The music's dip in dB, through two smoothing stages: the result starts softly and lands
/// softly (an S-curve) in both directions, with no steps to hear.
#[derive(Default)]
struct Gain {
    a: f32,
    b: f32,
}

impl Gain {
    fn step(&mut self, target: f32) -> f32 {
        let c = coef(if target < self.b { DIP_MS } else { RISE_MS });
        self.a += (target - self.a) * c;
        self.b += (self.a - self.b) * c;
        if self.b > -0.05 && target == 0.0 && self.a > -0.05 {
            (self.a, self.b) = (0.0, 0.0); // close enough: exactly your level again
        }
        self.b
    }

    fn at_rest(&self) -> bool {
        self.b == 0.0 && self.a == 0.0
    }
}

fn is(list: &[&str], stem: &str) -> bool {
    list.iter().any(|k| stem == *k || stem.starts_with(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_apps() {
        assert!(is(&VOICE, "discord"));
        assert!(is(&VOICE, "discordptb"));
        assert!(is(&MUSIC, "spotify"));
        assert!(!is(&MUSIC, "chrome"));
        assert!(!is(&VOICE, "spotify"));
    }

    fn db(x: f32) -> f32 {
        10f32.powf(x / 20.0)
    }

    /// A noisy open mic like the call measured live (about -34..-30 dB, jittering).
    fn noise(i: usize) -> f32 {
        db(-34.0 + 4.0 * ((i * 37 % 100) as f32 / 100.0))
    }

    #[test]
    fn steady_noise_never_counts() {
        let mut ear = Listener::new();
        assert!((0..1500).all(|i| !ear.hear(noise(i))), "30 s of background noise must never duck");
    }

    #[test]
    fn hears_speech_over_noise_quickly_and_holds_through_pauses() {
        let mut ear = Listener::new();
        for i in 0..300 {
            ear.hear(noise(i));
        }
        // a word at -16 dB over the noise: detected within ~100 ms
        let t = (0..20).position(|i| ear.hear(db(-16.0) + noise(i) * 0.1)).expect("speech must be heard");
        assert!(t as f32 * TICK_MS <= 100.0, "took {} ms", t as f32 * TICK_MS);
        for i in 0..15 {
            ear.hear(db(-16.0) + noise(i) * 0.1);
        }
        // a 0.4 s pause between words: still talking
        assert!((0..20).all(|i| ear.hear(noise(i))), "short pauses must not let the music up");
        // then real quiet: released after the hold
        let freed = (0..100).position(|i| !ear.hear(noise(i))).expect("must release");
        assert!(freed as f32 * TICK_MS <= HOLD_MS + 100.0);
    }

    #[test]
    fn ignores_clicks_and_pops() {
        let mut ear = Listener::new();
        for i in 0..300 {
            ear.hear(noise(i));
        }
        // a 20 ms keyboard click, well above the noise
        assert!(!ear.hear(db(-12.0)));
        assert!((0..50).all(|i| !ear.hear(noise(i))), "a single click must not duck");
    }

    #[test]
    fn clean_line_quiet_then_voice() {
        let mut ear = Listener::new();
        assert!((0..100).all(|_| !ear.hear(0.0)), "silence is not talking");
        assert!((0..10).any(|_| ear.hear(db(-20.0))), "a normal voice on a clean line is heard");
    }

    #[test]
    fn soft_voices_dip_a_little_full_talking_a_lot() {
        assert_eq!(dip_for(-50.0), SOFT_DIP_DB, "a video murmuring in the background: a gentle step");
        assert_eq!(dip_for(-6.0), FULL_DIP_DB, "full-on talking: well out of the way");
        let mid = dip_for(-27.0);
        assert!(mid > SOFT_DIP_DB + 3.0 && mid < FULL_DIP_DB - 3.0, "in between: in between ({mid})");
        assert!(dip_for(-20.0) > dip_for(-30.0), "louder talking always dips more");
    }

    #[test]
    fn roles_are_yours_or_automatic() {
        assert_eq!(auto_role("discord"), Role::Voice);
        assert_eq!(auto_role("spotify"), Role::Lowered);
        assert_eq!(auto_role("chrome"), Role::Ignored);
        let saved = parse_roles("chrome=l; Spotify=i;bad;zoom=x");
        assert_eq!(saved, vec![("chrome".to_string(), Role::Lowered), ("spotify".to_string(), Role::Ignored)]);
        assert_eq!(parse_roles(&format_roles(&saved)), saved, "saved roles read back the same");
        let sh = Shared::new();
        sh.set_roles(saved);
        assert_eq!(sh.role("chrome"), (Role::Lowered, false), "your choice wins");
        assert_eq!(sh.role("discord"), (Role::Voice, true), "the rest stays automatic");
        // whatever plays in Windows' media controls (a YouTube tab) is lowered automatically
        assert_eq!(auto_role_playing("chrome", "Chrome"), Role::Lowered);
        assert_eq!(auto_role_playing("chrome", ""), Role::Ignored);
        assert_eq!(auto_role_playing("discord", "Discord"), Role::Voice, "a call app stays a voice");
        let sh2 = Shared::new();
        sh2.set_media_app("Chrome");
        assert_eq!(sh2.role("chrome"), (Role::Lowered, true));
    }

    #[test]
    fn gain_moves_smoothly_both_ways() {
        let mut g = Gain::default();
        let (mut prev, mut biggest) = (0.0f32, 0.0f32);
        let mut down_ms = 0.0;
        for _ in 0..50 {
            let v = g.step(DEFAULT_DIP_DB);
            biggest = biggest.max((v - prev).abs());
            prev = v;
            down_ms += TICK_MS;
            if v < DEFAULT_DIP_DB + 1.0 {
                break;
            }
        }
        assert!(down_ms <= 300.0, "dip should land within ~0.3 s, took {down_ms} ms");
        assert!(biggest < 2.5, "no jumps on the way down: {biggest} dB in one tick");
        // the way back starts softly (S-curve): the first tick moves far less than later ones
        let first = g.step(0.0) - prev;
        let mut up_ms = TICK_MS;
        let mut v = prev + first;
        let mut biggest_up = first;
        while !g.at_rest() {
            let n = g.step(0.0);
            biggest_up = biggest_up.max(n - v);
            v = n;
            up_ms += TICK_MS;
        }
        assert!(first < biggest_up * 0.25, "the return must start gently");
        assert!((1500.0..=4500.0).contains(&up_ms), "return takes about 2.5 s, took {up_ms} ms");
        assert!(biggest_up < 0.6, "no audible steps on the way back: {biggest_up} dB in one tick");
    }

    #[test]
    fn mixer_sees_your_level_only_while_lowered() {
        let sh = Shared::new();
        *sh.own.lock().unwrap() = vec![("Spotify".into(), 0.85)];
        assert_eq!(sh.own_level("Spotify"), None, "not lowered: the real level is your level");
        sh.factor.store(0.3f32.to_bits(), Ordering::Relaxed);
        assert_eq!(sh.own_level("Spotify"), Some(0.85));
        assert_eq!(sh.own_level("Discord"), None);
    }
}

#[cfg(test)]
mod live {
    /// `cargo test --release live_duck -- --ignored --nocapture`: voice-app peak levels for 10 s.
    #[test]
    #[ignore]
    fn live_duck() {
        let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
        let sh = super::Shared::new();
        let (voices, music) = super::scan(&sh);
        println!("{} voice sessions, {} music sessions", voices.len(), music.len());
        for _ in 0..20 {
            let peaks: Vec<String> = voices.iter().map(|v| format!("{} {:.3}", v.name, unsafe { v.meter.GetPeakValue() }.unwrap_or(-1.0))).collect();
            let vols: Vec<String> = music.iter().map(|m| format!("{:.2}", unsafe { m.vol.GetMasterVolume() }.unwrap_or(-1.0))).collect();
            println!("voice peaks {peaks:?}  music volume {vols:?}");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}

#[cfg(test)]
mod sessions {
    /// `cargo test --release live_sessions -- --ignored --nocapture`: every audio session (pid, app, state, level).
    #[test]
    #[ignore]
    fn live_sessions() {
        use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
        use windows::Win32::Media::Audio::*;
        use windows::core::Interface;
        let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
        unsafe {
            let list = crate::media::volume::device().unwrap().Activate::<IAudioSessionManager2>(windows::Win32::System::Com::CLSCTX_ALL, None).unwrap().GetSessionEnumerator().unwrap();
            for _ in 0..3 {
                for i in 0..list.GetCount().unwrap() {
                    let c = list.GetSession(i).unwrap();
                    let pid = c.cast::<IAudioSessionControl2>().unwrap().GetProcessId().unwrap_or(0);
                    let peak = c.cast::<IAudioMeterInformation>().unwrap().GetPeakValue().unwrap_or(0.0);
                    println!("pid {pid:>6} {:<14} state {:?} peak {peak:.3}", crate::media::volume::exe_stem(pid).unwrap_or_default(), c.GetState().unwrap());
                }
                println!("--");
                std::thread::sleep(std::time::Duration::from_millis(700));
            }
        }
    }
}
