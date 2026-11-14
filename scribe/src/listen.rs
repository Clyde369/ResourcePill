//! Listen mode (`--listen`): for the pill's "lower music when people talk". Hears only the voice
//! apps (Discord, Teams… and any app you mark as a voice, e.g. YouTube in a browser), each through
//! Windows' per-app loopback with its own Silero voice detector, and prints, per app,
//! `SPEECH <app> 1|0` as people start and stop talking and `VOICE <app> <dB>` ten times a second.
//! Audio stays in memory, is never saved and never leaves the PC. Exits when the pill closes its
//! input.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{HRESULT, IUnknown, Interface, PWSTR, implement};

use crate::{Args, RATE, emit};

const VOICE: [&str; 9] = ["discord", "teams", "ms-teams", "zoom", "slack", "skype", "whatsapp", "telegram", "signal"];

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Done(mpsc::SyncSender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Done_Impl {
    fn ActivateCompleted(&self, _op: windows::core::Ref<IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let _ = self.0.try_send(());
        Ok(())
    }
}

/// The processes to listen to: for each app in `apps` (the pill's preference order) that has an
/// audio session, its process (app name, pid).
fn voice_pids(apps: &[String]) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(e) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return out };
        let Some(list) = e.GetDefaultAudioEndpoint(eRender, eMultimedia).ok().and_then(|d| d.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None).ok()).and_then(|m| m.GetSessionEnumerator().ok()) else { return out };
        let mut sessions = Vec::new();
        for i in 0..list.GetCount().unwrap_or(0) {
            let Ok(c) = list.GetSession(i) else { continue };
            let Ok(c2) = c.cast::<IAudioSessionControl2>() else { continue };
            let pid = c2.GetProcessId().unwrap_or(0);
            if let Some(s) = (pid != 0).then(|| exe_stem(pid)).flatten() {
                sessions.push((pid, s));
            }
        }
        for v in apps {
            if let Some((pid, _)) = sessions.iter().find(|(_, s)| s == v || s.starts_with(v.as_str())) {
                out.push((v.clone(), *pid));
            }
        }
    }
    out
}

fn exe_stem(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..n as usize])).and_then(|p| p.rsplit('\\').next().map(|f| f.to_ascii_lowercase().trim_end_matches(".exe").to_string()))
    }
}

/// An audio client that captures only `pid` (and its child processes).
unsafe fn app_client(pid: u32) -> windows::core::Result<IAudioClient> {
    unsafe {
        let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS { TargetProcessId: pid, ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE },
            },
        };
        // never "cleared": its blob points at `params` on our stack, which Windows must not free
        let pv = std::mem::ManuallyDrop::new(PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    Anonymous: PROPVARIANT_0_0_0 { blob: BLOB { cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32, pBlobData: &mut params as *mut _ as *mut u8 } },
                    ..Default::default()
                }),
            },
        });
        let (tx, rx) = mpsc::sync_channel(1);
        let handler: IActivateAudioInterfaceCompletionHandler = Done(tx).into();
        let op = ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*pv), &handler)?;
        rx.recv_timeout(Duration::from_secs(5)).map_err(|_| windows::core::Error::from(HRESULT(0x8000_000Au32 as i32)))?; // E_PENDING
        let (mut hr, mut unk) = (HRESULT(0), None::<IUnknown>);
        op.GetActivateResult(&mut hr, &mut unk)?;
        hr.ok()?;
        unk.ok_or_else(windows::core::Error::empty)?.cast()
    }
}

/// One voice app being listened to.
struct Stream {
    app: String,
    capture: IAudioCaptureClient,
    _client: IAudioClient,
    vad: sherpa_onnx::VoiceActivityDetector,
    pending: Vec<f32>,
    speaking: bool,
    last_audio: Instant,
    level_sum: f32,
    level_n: usize,
    level_seen: usize,
}

fn open_stream(app: &str, pid: u32, a: &Args) -> Result<Stream, String> {
    let client = unsafe { app_client(pid) }.map_err(|e| format!("cannot hear {app}: {e}"))?;
    let fmt = WAVEFORMATEX { wFormatTag: WAVE_FORMAT_PCM as u16, nChannels: 1, nSamplesPerSec: RATE as u32, nAvgBytesPerSec: RATE as u32 * 2, nBlockAlign: 2, wBitsPerSample: 16, cbSize: 0 };
    let capture: IAudioCaptureClient = unsafe {
        client
            .Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, 2_000_000, 0, &fmt, None)
            .map_err(|e| format!("cannot open {app}'s audio: {e}"))?;
        let c = client.GetService().map_err(|e| e.to_string())?;
        client.Start().map_err(|e| e.to_string())?;
        c
    };
    let vad = crate::vad(&a.models, 30.0, 0.45)?; // 0.45 s of quiet ends a stretch of speech
    Ok(Stream { app: app.to_string(), capture, _client: client, vad, pending: Vec::new(), speaking: false, last_audio: Instant::now(), level_sum: 0.0, level_n: 0, level_seen: 0 })
}

impl Stream {
    /// Take in what the app played since last time, and report speech starting/stopping and its level.
    fn step(&mut self) {
        let mut got = false;
        unsafe {
            while self.capture.GetNextPacketSize().unwrap_or(0) > 0 {
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0u32, 0u32);
                if self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None).is_err() {
                    break;
                }
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null();
                for i in 0..frames as usize {
                    self.pending.push(if silent { 0.0 } else { *(data as *const i16).add(i) as f32 / 32768.0 });
                }
                let _ = self.capture.ReleaseBuffer(frames);
                got = true;
            }
        }
        if got {
            self.last_audio = Instant::now();
        } else if self.last_audio.elapsed() > Duration::from_millis(100) {
            self.pending.extend(std::iter::repeat_n(0.0, RATE as usize * 16 / 1000)); // nothing playing: feed quiet so speech ends
        }
        // how loud the voices are (the app's real audio): RMS over ~0.1 s
        let fresh = self.pending.len().saturating_sub(self.level_seen);
        self.level_sum += self.pending[self.pending.len() - fresh..].iter().map(|x| x * x).sum::<f32>();
        self.level_n += fresh;
        if self.level_n >= RATE as usize / 10 {
            let db = 10.0 * (self.level_sum / self.level_n as f32).max(1e-10).log10();
            emit("VOICE", &[&self.app, &format!("{db:.1}")]);
            (self.level_sum, self.level_n) = (0.0, 0);
        }
        let whole = self.pending.len() / 512 * 512;
        self.level_seen = self.pending.len() - whole;
        for w in self.pending[..whole].chunks(512) {
            self.vad.accept_waveform(w);
        }
        self.pending.drain(..whole);
        self.vad.clear(); // only "is someone talking now?" matters: drop the finished phrases
        let now = self.vad.detected();
        if now != self.speaking {
            self.speaking = now;
            emit("SPEECH", &[&self.app, if now { "1" } else { "0" }]);
        }
    }
}

pub fn listen(a: &Args) -> Result<(), String> {
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    // the pill closes our input when it exits or no longer needs us
    let (gone_tx, gone) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut Vec::new());
        let _ = gone_tx.send(());
    });
    // which apps count as "voice": the pill's list (your choices), else the usual call apps
    let apps: Vec<String> = if a.voice_apps.is_empty() { VOICE.iter().map(|s| s.to_string()).collect() } else { a.voice_apps.clone() };
    let mut streams: Vec<Stream> = Vec::new();
    for (app, pid) in voice_pids(&apps) {
        match open_stream(&app, pid, a) {
            Ok(s) => streams.push(s),
            Err(e) => emit("STATUS", &[&e]),
        }
    }
    if streams.is_empty() {
        return Err("no voice app is playing sound".into());
    }
    emit("READY", &[&streams.iter().map(|s| s.app.as_str()).collect::<Vec<_>>().join(",")]);
    loop {
        if gone.try_recv().is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(16));
        for s in &mut streams {
            s.step();
        }
    }
}
