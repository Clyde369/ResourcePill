//! Pill-side control of ResourcePill-Scribe (the recording/transcription helper): start/stop it,
//! collect its live output, and predict how each model will run on this machine right now.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const ASR: [(&str, &str); 2] = [("parakeet-v3", "Parakeet TDT 0.6B v3 (fast)"), ("cohere-transcribe", "Cohere Transcribe 2B (most accurate)")];
pub const LLM: [(&str, &str, &str); 2] = [
    ("qwen3.5-4b", "Qwen3.5-4B-Q4_K_M.gguf", "Qwen 3.5 4B (quick)"),
    ("qwen3.5-9b", "Qwen3.5-9B-Q4_K_M.gguf", "Qwen 3.5 9B (best)"),
];

#[derive(PartialEq, Clone, Copy)]
pub enum State {
    Idle,
    Recording,
    Finishing,
    Done,
    Failed,
}

pub struct Line {
    pub who: String,
    pub text: String,
}

pub struct Recorder {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    inbox: Arc<Mutex<Vec<String>>>,
    pub state: State,
    pub started: Instant,
    pub lines: VecDeque<Line>,
    pub speakers: Vec<String>,
    pub status: String,
    pub summary: Option<PathBuf>,
    pub folder: Option<PathBuf>,
    improving: bool, // saved (SAVED), the helper still working on a better version (until DONE)
    live: bool,      // a live transcript while recording (off: recording only, transcript after stopping)
}

pub fn models_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("ResourcePill").join("models")
}

static REC_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Use this folder for recordings (your choice in the setup guide); None: the default.
pub fn set_recordings_dir(dir: Option<PathBuf>) {
    *REC_DIR.lock().unwrap_or_else(|e| e.into_inner()) = dir;
}

/// Where recordings, transcripts and summaries go: your chosen folder, else the default.
pub fn recordings_dir() -> PathBuf {
    REC_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(default_recordings_dir)
}

/// Documents\ResourcePill\Recordings (follows OneDrive/folder redirection).
pub fn default_recordings_dir() -> PathBuf {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Documents, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    let docs = unsafe {
        SHGetKnownFolderPath(&FOLDERID_Documents, KF_FLAG_DEFAULT, None).ok().map(|p| {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            PathBuf::from(s)
        })
    };
    docs.unwrap_or_else(|| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("Documents")).join("ResourcePill").join("Recordings")
}

pub fn helper() -> Option<PathBuf> {
    // next to the exe, or unpacked from a one-file build
    let p = crate::bundle::helper_dir()?.join("ResourcePill-Scribe.exe");
    p.exists().then_some(p)
}

impl Recorder {
    pub fn new() -> Self {
        Recorder {
            child: None,
            stdin: None,
            inbox: Arc::new(Mutex::new(Vec::new())),
            state: State::Idle,
            started: Instant::now(),
            lines: VecDeque::new(),
            speakers: Vec::new(),
            status: String::new(),
            summary: None,
            folder: None,
            improving: false,
            live: true,
        }
    }

    pub fn busy(&self) -> bool {
        matches!(self.state, State::Recording | State::Finishing)
    }

    /// Starts recording. `notify` is called (from a background thread) whenever output arrives.
    pub fn start(&mut self, asr: &str, llm: Option<PathBuf>, final_asr: Option<&str>, keep_audio: bool, gpu: bool, live: bool, gemini_key: Option<String>, notify: impl Fn() + Send + 'static) -> Result<(), String> {
        let exe = helper().ok_or("ResourcePill-Scribe.exe is missing next to ResourcePill.exe")?;
        let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        let folder = recordings_dir().join(format!("{}-{:02}-{:02} {:02}.{:02}.{:02}", now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond));
        let mut cmd = Command::new(exe);
        cmd.args(["--asr", asr, "--out", &folder.to_string_lossy()]);
        if let Some(m) = final_asr {
            cmd.args(["--final", m]);
        }
        if keep_audio {
            cmd.arg("--keep-audio");
        }
        if !live && final_asr.is_some() {
            cmd.arg("--no-live"); // record only; the accurate pass writes the transcript after stopping
        }
        if gpu {
            cmd.arg("--gpu");
        }
        if let Some(k) = gemini_key {
            cmd.env("RP_GEMINI_KEY", k); // the environment, not the command line (which other apps can list)
        }
        if let Some(l) = &llm {
            cmd.args(["--llm", &l.to_string_lossy()]);
        }
        self.live = live || final_asr.is_none(); // no accurate pass: the live one is the only transcript
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000 | 0x0000_4000) // no console window, below-normal priority
            .spawn()
            .map_err(|e| format!("could not start the transcriber: {e}"))?;
        let out = child.stdout.take().ok_or("no output pipe")?;
        // a fresh inbox: a previous recording still improving in the background finishes on its
        // own (its files are rewritten when done) and its messages no longer reach this one
        self.inbox = Arc::new(Mutex::new(Vec::new()));
        let inbox = self.inbox.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                inbox.lock().unwrap_or_else(|e| e.into_inner()).push(line);
                notify();
            }
            inbox.lock().unwrap_or_else(|e| e.into_inner()).push("EXIT".into());
            notify();
        });
        self.stdin = child.stdin.take();
        self.child = Some(child);
        self.state = State::Recording;
        self.started = Instant::now();
        self.lines.clear();
        self.speakers.clear();
        self.summary = None;
        self.improving = false;
        self.folder = Some(folder);
        self.status = "Loading speech model…".into();
        Ok(())
    }

    /// Ends the recording; the helper then saves the transcript and writes the summary.
    pub fn stop(&mut self) {
        if let Some(mut s) = self.stdin.take() {
            let _ = writeln!(s, "stop");
        }
        if self.state == State::Recording {
            self.state = State::Finishing;
            self.status = "Finishing transcript…".into();
        }
    }

    /// Applies queued helper output. Returns true if anything changed.
    pub fn pump(&mut self) -> bool {
        let msgs: Vec<String> = std::mem::take(&mut *self.inbox.lock().unwrap_or_else(|e| e.into_inner()));
        for m in &msgs {
            let f: Vec<String> = m.split('\t').map(unescape).collect();
            match f[0].as_str() {
                "READY" => self.status = if self.live { "Listening".into() } else { "Recording · the transcript comes after you stop".into() },
                "SEG" if f.len() >= 4 => {
                    let who = f[2].to_string();
                    if !self.speakers.contains(&who) {
                        self.speakers.push(who.clone());
                    }
                    self.lines.push_back(Line { who, text: f[3].to_string() });
                    while self.lines.len() > 40 {
                        self.lines.pop_front();
                    }
                }
                // the live transcript is saved: openable now, the helper keeps improving it until DONE
                "SAVED" => {
                    self.state = State::Done;
                    self.improving = true;
                    self.status = "Transcript saved · improving in background… click to open".into();
                }
                "STATUS" if f.len() >= 2 => self.status = if self.improving { format!("Saved · {} · click to open", f[1]) } else { f[1].to_string() },
                "SUMMARY" if f.len() >= 2 => self.summary = Some(PathBuf::from(&f[1])),
                "DONE" => {
                    self.state = State::Done;
                    self.improving = false;
                    self.status = if self.summary.is_some() { "Summary ready · click to open".into() } else { "Transcript saved · click to open".into() };
                }
                "ERROR" if f.len() >= 2 => {
                    self.status = f[1].to_string();
                    if self.state == State::Recording {
                        self.state = State::Failed;
                    }
                }
                "EXIT" => {
                    if let Some(mut c) = self.child.take() {
                        let _ = c.wait();
                    }
                    self.stdin = None;
                    if std::mem::take(&mut self.improving) {
                        self.status = "Transcript saved (couldn't improve it) · click to open".into();
                    }
                    if self.busy() {
                        self.state = State::Failed;
                        if self.status.is_empty() || self.status.starts_with("Listening") {
                            self.status = "The transcriber stopped unexpectedly".into();
                        }
                    }
                }
                _ => {}
            }
        }
        !msgs.is_empty()
    }
}

/// Reverse of the helper's line-protocol escaping (\\ \t \n).
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

// ---------- one-off helper jobs: explain highlighted text, download a model ----------

#[derive(PartialEq, Clone, Copy)]
pub enum JobKind {
    Explain,
    Download,
}

#[derive(PartialEq, Clone, Copy)]
pub enum JobState {
    Running,
    Done,
    Failed,
}

pub struct Job {
    child: Option<Child>,
    inbox: Arc<Mutex<Vec<String>>>,
    pub kind: JobKind,
    pub state: JobState,
    pub title: String,
    pub query: String,     // explain: the highlighted text
    pub text: String,      // answer, status or error
    pub progress: (u64, u64),
    pub at: Instant, // when it started (short notes clear themselves)
    pub note: bool,  // just a message from Peter (not a failure): normal colours, clears itself
}

impl Job {
    /// Runs the helper with `args`; `input` (if any) is written to its stdin and closed.
    pub fn start(kind: JobKind, title: String, query: String, args: &[String], input: Option<String>, notify: impl Fn() + Send + 'static) -> Result<Job, String> {
        let exe = helper().ok_or("ResourcePill-Scribe.exe is missing next to ResourcePill.exe")?;
        let mut child = Command::new(exe)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000 | 0x0000_4000)
            .spawn()
            .map_err(|e| format!("could not start the helper: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            if let Some(s) = input {
                let _ = stdin.write_all(s.as_bytes());
            }
        } // dropped here = EOF
        let out = child.stdout.take().ok_or("no output pipe")?;
        let inbox: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let ib = inbox.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                ib.lock().unwrap_or_else(|e| e.into_inner()).push(line);
                notify();
            }
            ib.lock().unwrap_or_else(|e| e.into_inner()).push("EXIT".into());
            notify();
        });
        let text = if kind == JobKind::Explain { "Thinking…".into() } else { "Starting download…".into() };
        Ok(Job { child: Some(child), inbox, kind, state: JobState::Running, title, query, text, progress: (0, 0), at: Instant::now(), note: false })
    }

    /// A download that looks part-way through (for the `--snapshots` pictures of first launch).
    pub fn preview_download(title: &str, done: u64, total: u64) -> Job {
        Job { child: None, inbox: Arc::new(Mutex::new(Vec::new())), kind: JobKind::Download, state: JobState::Running, title: title.into(), query: String::new(), text: "Downloading…".into(), progress: (done, total), at: Instant::now(), note: false }
    }

    /// A finished job that only shows a message (e.g. "highlight some text first").
    pub fn note(kind: JobKind, title: &str, text: &str) -> Job {
        Job {
            child: None,
            inbox: Arc::new(Mutex::new(Vec::new())),
            kind,
            state: JobState::Done,
            title: title.into(),
            query: String::new(),
            text: text.into(),
            progress: (0, 0),
            at: Instant::now(),
            note: true,
        }
    }

    pub fn pump(&mut self) -> bool {
        let msgs: Vec<String> = std::mem::take(&mut *self.inbox.lock().unwrap_or_else(|e| e.into_inner()));
        for m in &msgs {
            let f: Vec<String> = m.split('\t').map(unescape).collect();
            match (f[0].as_str(), f.len()) {
                ("EXPLAIN", 2..) => {
                    self.text = f[1].clone();
                    self.state = JobState::Done;
                }
                ("PROGRESS", 4..) => {
                    self.progress = (f[2].parse().unwrap_or(0), f[3].parse().unwrap_or(0));
                    self.text = "Downloading…".into();
                }
                ("STATUS", 2..) => self.text = f[1].clone(),
                ("INSTALLED", _) => {
                    self.state = JobState::Done;
                    self.text = "Installed · ready to use".into();
                }
                ("ERROR", 2..) => {
                    self.state = JobState::Failed;
                    self.text = f[1].clone();
                }
                ("EXIT", _) => {
                    if let Some(mut c) = self.child.take() {
                        let _ = c.wait();
                    }
                    if self.state == JobState::Running {
                        self.state = JobState::Failed;
                        self.text = "The helper stopped unexpectedly".into();
                    }
                }
                _ => {}
            }
        }
        !msgs.is_empty()
    }

    /// Stop a running job (a partial download resumes next time).
    pub fn cancel(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}

// ---------- performance predictions ----------

/// Total dedicated memory of the largest GPU (bytes), via DXGI.
pub fn vram_total() -> f64 {
    use windows::Win32::Graphics::Dxgi::*;
    unsafe {
        let Ok(f) = CreateDXGIFactory1::<IDXGIFactory1>() else { return 0.0 };
        let mut best = 0usize;
        let mut i = 0;
        while let Ok(a) = f.EnumAdapters1(i) {
            if let Ok(d) = a.GetDesc1() {
                if d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 {
                    best = best.max(d.DedicatedVideoMemory);
                }
            }
            i += 1;
        }
        best as f64
    }
}

/// An NVIDIA graphics card is present (the GPU speech engine needs CUDA).
pub fn nvidia_gpu() -> bool {
    use windows::Win32::Graphics::Dxgi::*;
    unsafe {
        let Ok(f) = CreateDXGIFactory1::<IDXGIFactory1>() else { return false };
        (0..).map_while(|i| f.EnumAdapters1(i).ok()).any(|a| a.GetDesc1().is_ok_and(|d| d.VendorId == 0x10DE))
    }
}

pub fn ram_available() -> f64 {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    let _ = unsafe { GlobalMemoryStatusEx(&mut m) };
    m.ullAvailPhys as f64
}

/// Starts a one-off speed measurement for a speech model (writes bench.txt). Runs at most once per session.
fn bench_in_background(id: &'static str) {
    static STARTED: Mutex<Vec<&str>> = Mutex::new(Vec::new());
    let mut s = STARTED.lock().unwrap_or_else(|e| e.into_inner());
    if s.contains(&id) {
        return;
    }
    s.push(id);
    if let Some(exe) = helper() {
        let _ = Command::new(exe).args(["--bench", id]).stdout(Stdio::null()).stderr(Stdio::null()).creation_flags(0x0800_0000 | 0x0000_4000).spawn();
    }
}

/// e.g. "keeps up live · ~5× faster than speech". `cpu` = current CPU load 0..1.
const VIEWER_PAGE: &str = include_str!("../scribe/src/viewer.html");

/// Open the Recordings window (transcripts, summaries, audio), optionally on one recording folder.
/// Done by the pill itself: no transcriber or model is started just to look at recordings.
pub fn open_viewer(select: Option<&std::path::Path>) {
    let dir = recordings_dir();
    let _ = std::fs::create_dir_all(&dir);
    // recordings made before the window existed: build their data from transcript.md / summary.md
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let d = e.path();
        if d.join("transcript.md").exists() && !d.join("rec.js").exists() {
            if let Some(js) = rec_from_markdown(&d) {
                let _ = std::fs::write(d.join("rec.js"), js);
            }
        }
    }
    // list the folders that have window data; the page loads each one's rec.js itself
    let names: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("rec.js").exists())
        .map(|e| format!("{:?}", e.file_name().to_string_lossy())) // Rust's escaped string is valid JS
        .collect();
    let _ = std::fs::write(dir.join("recordings.js"), format!("window.RP_INDEX = [{}];\n", names.join(",")));
    let page = dir.join("index.html");
    let _ = std::fs::write(&page, VIEWER_PAGE);

    let enc = |s: &str| s.replace('%', "%25").replace(' ', "%20").replace('#', "%23");
    let mut url = format!("file:///{}", enc(&page.to_string_lossy().replace('\\', "/")));
    if let Some(name) = select.and_then(|p| p.file_name()) {
        url += &format!("?rec={}", enc(&name.to_string_lossy()));
    }
    // its own window: Edge's app mode ships with Windows (no tabs, no address bar)
    let edge = ["ProgramFiles(x86)", "ProgramFiles"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|p| PathBuf::from(p).join(r"Microsoft\Edge\Application\msedge.exe"))
        .find(|p| p.exists());
    let launched = edge.is_some_and(|exe| Command::new(exe).arg(format!("--app={url}")).arg("--window-size=1180,800").spawn().is_ok());
    if !launched {
        let _ = Command::new("explorer.exe").arg(&page).spawn();
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' | '\t' => out.push(' '),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Window data for an older recording, from its markdown files
/// ("Recorded <date> · m:ss · speech model: X" and "**[mm:ss] Who:** text" lines).
fn rec_from_markdown(dir: &std::path::Path) -> Option<String> {
    let md = std::fs::read_to_string(dir.join("transcript.md")).ok()?;
    let (mut date, mut duration, mut model) = (String::new(), 0u64, String::new());
    let mut lines: Vec<(u64, String, String)> = Vec::new();
    for l in md.lines() {
        if let Some(rest) = l.strip_prefix("Recorded ") {
            let parts: Vec<&str> = rest.split(" · ").collect();
            date = parts.first().unwrap_or(&"").to_string();
            if let Some((m, s)) = parts.get(1).and_then(|d| d.split_once(':')) {
                duration = m.trim().parse::<u64>().unwrap_or(0) * 60 + s.trim().parse::<u64>().unwrap_or(0);
            }
            model = parts.get(2).map(|m| m.trim_start_matches("speech model: ").to_string()).unwrap_or_default();
        } else if let Some(rest) = l.strip_prefix("**[") {
            let (time, rest) = rest.split_once("] ")?;
            let (who, text) = rest.split_once(":** ")?;
            let (m, s) = time.split_once(':')?;
            let t = m.parse::<u64>().ok()? * 60 + s.parse::<u64>().ok()?;
            lines.push((t, who.to_string(), text.trim().to_string()));
        }
    }
    let summary = std::fs::read_to_string(dir.join("summary.md")).ok().map(|s| s.lines().skip_while(|l| l.starts_with("# ") || l.trim().is_empty()).collect::<Vec<_>>().join("\n"));
    let title_src = summary.as_deref().and_then(|s| s.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with(['-', '*']))).or(lines.first().map(|l| l.2.as_str())).unwrap_or("Recording");
    let title: String = title_src.split_inclusive(['.', '!', '?']).next().unwrap_or(title_src).chars().take(80).collect();
    let mut speakers: Vec<&str> = Vec::new();
    for (_, w, _) in &lines {
        if !speakers.contains(&w.as_str()) {
            speakers.push(w);
        }
    }
    let audio = if dir.join("recording.wav").exists() { "\"recording.wav\"" } else { "null" };
    let json = format!(
        "{{\"title\":{},\"date\":{},\"duration\":{},\"model\":{},\"speakers\":[{}],\"lines\":[{}],\"summary\":{},\"audio\":{audio}}}",
        json_str(title.trim()),
        json_str(&date),
        duration.max(lines.last().map_or(0, |l| l.0)),
        json_str(&model),
        speakers.iter().map(|s| json_str(s)).collect::<Vec<_>>().join(","),
        lines.iter().map(|(t, w, x)| format!("{{\"t\":{t},\"who\":{},\"text\":{}}}", json_str(w), json_str(x))).collect::<Vec<_>>().join(","),
        summary.as_deref().map_or("null".into(), json_str),
    );
    Some(format!("window.RP_ADD && RP_ADD(document.currentScript, {json});\n"))
}

/// The most accurate installed speech model, used for the pass after stopping.
pub fn final_model() -> Option<&'static str> {
    ["cohere-transcribe", "parakeet-v3"].into_iter().find(|id| models_dir().join(id).join("tokens.txt").exists())
}

/// e.g. "Cohere Transcribe · ~14 min per hour of audio". The pass runs on all cores, so it is
/// roughly 1.5× faster than the live benchmark, which used half of them.
pub fn final_prediction(cpu: f32) -> String {
    let Some(id) = final_model() else { return "no speech model installed".into() };
    let name = ASR.iter().find(|a| a.0 == id).map_or(id, |a| a.1);
    match std::fs::read_to_string(models_dir().join(id).join("bench.txt")).ok().and_then(|s| s.trim().parse::<f32>().ok()) {
        Some(rtf) => {
            let mins = rtf * 60.0 / 1.5 * (1.0 + cpu.clamp(0.0, 1.0)) / 1.5;
            format!("{name} · ~{:.0} min per hour of audio", mins.max(1.0))
        }
        None => {
            bench_in_background(id);
            format!("{name} · measuring speed…")
        }
    }
}

pub fn asr_prediction(id: &'static str, cpu: f32) -> String {
    let dir = models_dir().join(id);
    if !dir.join("tokens.txt").exists() {
        return "not downloaded".into();
    }
    let Some(rtf) = std::fs::read_to_string(dir.join("bench.txt")).ok().and_then(|s| s.trim().parse::<f32>().ok()) else {
        bench_in_background(id);
        return "measuring speed on this PC…".into();
    };
    // the benchmark ran at some load; scale by how busy the CPU is now
    let now = rtf * (1.0 + 1.5 * cpu.clamp(0.0, 1.0)) / 1.75;
    if now < 0.35 {
        format!("✓ keeps up live · ~{:.0}× faster than speech", 1.0 / now)
    } else if now < 0.9 {
        format!("✓ keeps up live, CPU will be busy ({:.0}% of real time)", now * 100.0)
    } else {
        format!("⚠ slower than live right now ({:.1}× real time) · finishes after you stop", now)
    }
}

/// e.g. "✓ fits on GPU · ~15 s per summary". Needs the model file size vs free GPU/RAM memory.
pub fn llm_prediction(dir: &str, file: &str, vram_total: f64, vram_used: f64, ram_free: f64) -> String {
    let Ok(meta) = std::fs::metadata(models_dir().join(dir).join(file)) else { return "not downloaded".into() };
    const GB: f64 = 1_073_741_824.0;
    let need = meta.len() as f64 * 1.08 + 0.9 * GB; // weights + context cache + runtime
    let free = (vram_total - vram_used - 0.3 * GB).max(0.0); // leave headroom for the desktop
    if vram_total > 0.0 && need <= free {
        format!("✓ fits on GPU ({:.1} of {:.1} GB free) · fast, ~15–40 s per summary", need / GB, free / GB)
    } else if need - free < ram_free - 1.0 * GB {
        let on_gpu = if need > 0.0 { (free / need * 100.0).clamp(0.0, 100.0) } else { 0.0 };
        format!("⚠ only {on_gpu:.0}% fits on GPU right now · rest on CPU, ~3–6× slower")
    } else {
        format!("✗ not enough free memory now (needs {:.1} GB)", need / GB)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unescape_reverses_helper_escaping() {
        assert_eq!(super::unescape(r"a\nb\tc\\d"), "a\nb\tc\\d");
        assert_eq!(super::unescape("plain"), "plain");
        assert_eq!(super::unescape("trailing\\"), "trailing\\");
    }

    #[test]
    fn saved_is_openable_while_improving() {
        use super::{Recorder, State};
        let mut r = Recorder::new();
        r.state = State::Finishing;
        let feed = |r: &mut Recorder, l: &str| {
            r.inbox.lock().unwrap().push(l.into());
            r.pump();
        };
        feed(&mut r, "SAVED\tC:\\x");
        assert!(r.state == State::Done && !r.busy(), "openable, a new recording may start");
        feed(&mut r, "STATUS\tImproving accuracy · 40%");
        assert_eq!(r.status, "Saved · Improving accuracy · 40% · click to open");
        feed(&mut r, "DONE\tC:\\x");
        assert_eq!(r.status, "Transcript saved · click to open");
        feed(&mut r, "EXIT");
        assert!(r.state == State::Done, "a finished helper exiting is not a failure");
        // the helper dies while improving: the live transcript stays, progress doesn't linger
        feed(&mut r, "SAVED\tC:\\x");
        feed(&mut r, "EXIT");
        assert!(r.state == State::Done && r.status.starts_with("Transcript saved (couldn't"));
    }
}

#[cfg(test)]
mod viewer_tests {
    /// Manual check: opens the Recordings window (run with --ignored).
    #[test]
    #[ignore]
    fn open_viewer_window() {
        super::open_viewer(None);
    }
}

#[cfg(test)]
mod backfill_tests {
    #[test]
    fn builds_window_data_from_markdown() {
        let d = std::env::temp_dir().join("rp-backfill-test");
        let _ = std::fs::create_dir_all(&d);
        std::fs::write(d.join("transcript.md"), "# Transcript\n\nRecorded 2026-11-12 11:49 · 4:11 · speech model: parakeet-v3\n\n**[00:18] You:** So but \"slow\"?\n\n**[01:02] Speaker 1:** Yes.\n").unwrap();
        std::fs::write(d.join("summary.md"), "# Meeting summary\n\n## Summary\nThey talked about speed. Then more.\n").unwrap();
        let js = super::rec_from_markdown(&d).unwrap();
        assert!(js.starts_with("window.RP_ADD && RP_ADD(document.currentScript, {"));
        assert!(js.contains(r#""title":"They talked about speed.""#), "{js}");
        assert!(js.contains(r#""duration":251"#));
        assert!(js.contains(r#"{"t":18,"who":"You","text":"So but \"slow\"?"}"#));
        assert!(js.contains(r#""speakers":["You","Speaker 1"]"#));
        let _ = std::fs::remove_dir_all(&d);
    }
}
