//! Small extras: the hourly chime, the encrypted API-key store, and grabbing highlighted text.

use std::path::PathBuf;
use std::sync::OnceLock;

use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::core::*;

// ---------- hourly chime ----------

/// A soft two-tone "ding" (C6 + G6 with a gentle bell overtone), synthesised once as an in-memory WAV.
fn chime_wav() -> &'static [u8] {
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    WAV.get_or_init(|| {
        synth(1.6, |t| {
            let tone = |f: f32, decay: f32, delay: f32| {
                let t = t - delay;
                if t < 0.0 {
                    return 0.0;
                }
                (t / 0.006).min(1.0) * (-t / decay).exp() * (std::f32::consts::TAU * f * t).sin()
            };
            (0.55 * tone(1046.5, 0.45, 0.0) + 0.25 * tone(2093.0, 0.25, 0.0) + 0.45 * tone(1568.0, 0.55, 0.12)) * 0.16
        })
    })
}

pub fn play_chime() {
    let wav = chime_wav();
    unsafe {
        let _ = PlaySoundW(PCWSTR(wav.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

/// The half-hour ding: one soft, lower note (G5 with a faint octave), shorter and quieter than the
/// two-note hourly chime so they are easy to tell apart.
fn half_wav() -> &'static [u8] {
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    WAV.get_or_init(|| {
        synth(1.1, |t| {
            let env = (t / 0.01).min(1.0) * (-t / 0.32).exp();
            env * (0.8 * (std::f32::consts::TAU * 783.99 * t).sin() + 0.15 * (std::f32::consts::TAU * 1567.98 * t).sin()) * 0.11
        })
    })
}

pub fn play_half_chime() {
    let wav = half_wav();
    unsafe {
        let _ = PlaySoundW(PCWSTR(wav.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

/// Soft interface cues, all quiet and short.
#[derive(Clone, Copy)]
pub enum Cue {
    RecStart = 0, // two notes rising
    RecStop,      // the same, falling
    Saved,        // a gentle three-note "done" when the transcript is saved
    Tip,          // a tiny tick when the highlight tooltip appears
}

pub fn play_cue(c: Cue) {
    static WAVS: OnceLock<[Vec<u8>; 4]> = OnceLock::new();
    let wavs = WAVS.get_or_init(|| {
        let note = |f: f32, at: f32, decay: f32, t: f32| {
            let t = t - at;
            if t < 0.0 { 0.0 } else { (t / 0.006).min(1.0) * (-t / decay).exp() * (std::f32::consts::TAU * f * t).sin() }
        };
        [
            synth(0.5, move |t| (note(880.0, 0.0, 0.06, t) + note(1318.5, 0.08, 0.11, t)) * 0.07),
            synth(0.5, move |t| (note(1318.5, 0.0, 0.06, t) + note(880.0, 0.08, 0.11, t)) * 0.07),
            synth(0.9, move |t| (note(1046.5, 0.0, 0.08, t) + note(1318.5, 0.09, 0.08, t) + note(1568.0, 0.18, 0.22, t)) * 0.06),
            synth(0.12, move |t| note(1975.5, 0.0, 0.018, t) * 0.05),
        ]
    });
    unsafe {
        let _ = PlaySoundW(PCWSTR(wavs[c as usize].as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

/// 44.1 kHz mono 16-bit WAV of `secs` seconds from a sample function f(t) in -1..1.
fn synth(secs: f32, f: impl Fn(f32) -> f32) -> Vec<u8> {
    const RATE: u32 = 44_100;
    let n = (RATE as f32 * secs) as usize;
    let pcm: Vec<u8> = (0..n).flat_map(|i| ((f(i as f32 / RATE as f32).clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
    let mut w = Vec::with_capacity(44 + pcm.len());
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    w.extend_from_slice(&pcm);
    w
}

// ---------- API key store (DPAPI: only this Windows user can decrypt it) ----------

/// Key providers: "anthropic" or "gemini". Each has its own encrypted file.
fn entropy(provider: &str) -> Vec<u8> {
    format!("ResourcePill.{provider}.v1").into_bytes()
}

fn key_path(provider: &str) -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("ResourcePill").join(format!("{provider}.key")))
}

/// Which provider a pasted key belongs to: Anthropic keys start with sk-ant-; anything else that
/// looks like a key (one token, no spaces) is taken as Google's (AIza…, or its newer formats).
pub fn key_provider(key: &str) -> Option<&'static str> {
    let token = (30..=300).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_graphic());
    match () {
        _ if !token => None,
        _ if key.starts_with("sk-ant-") => Some("anthropic"),
        _ if key.starts_with("sk-") => None, // another provider's key (e.g. OpenAI)
        _ => Some("gemini"),
    }
}

fn blob(b: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB { cbData: b.len() as u32, pbData: b.as_ptr() as *mut u8 }
}

pub fn save_key(provider: &str, key: &str) -> bool {
    let Some(p) = key_path(provider) else { return false };
    unsafe {
        let mut out = CRYPT_INTEGER_BLOB::default();
        let e = entropy(provider);
        let ent = blob(&e);
        if CryptProtectData(&blob(key.as_bytes()), w!("ResourcePill API key"), Some(&ent), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out).is_err() {
            return false;
        }
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut _)));
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
        std::fs::write(p, bytes).is_ok()
    }
}

pub fn load_key(provider: &str) -> Option<String> {
    let enc = std::fs::read(key_path(provider)?).ok()?;
    unsafe {
        let mut out = CRYPT_INTEGER_BLOB::default();
        let e = entropy(provider);
        let ent = blob(&e);
        CryptUnprotectData(&blob(&enc), None, Some(&ent), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out).ok()?;
        let key = String::from_utf8_lossy(std::slice::from_raw_parts(out.pbData, out.cbData as usize)).into_owned();
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut _)));
        Some(key)
    }
}

/// Is a key stored? (no decryption: cheap enough to ask while drawing)
pub fn has_key(provider: &str) -> bool {
    key_path(provider).is_some_and(|p| p.exists())
}

pub fn remove_key(provider: &str) {
    if let Some(p) = key_path(provider) {
        let _ = std::fs::remove_file(p);
    }
}

/// "sk-ant-…a1b2" for display; never shows the full key.
pub fn mask(key: &str) -> String {
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{}…{tail}", key.chars().take(7).collect::<String>())
}

// ---------- clipboard + highlighted text ----------

const CF_UNICODETEXT: u32 = 13;

pub fn clipboard_text(owner: HWND) -> Option<String> {
    unsafe {
        // another app may hold the clipboard for a moment (browsers, clipboard history)
        if !(0..10).any(|i| {
            if i > 0 {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            OpenClipboard(Some(owner)).is_ok()
        }) {
            return None;
        }
        let r = (|| {
            let h = GetClipboardData(CF_UNICODETEXT).ok()?;
            let p = GlobalLock(HGLOBAL(h.0)) as *const u16;
            if p.is_null() {
                return None;
            }
            let len = (0..).take_while(|&i| *p.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let _ = GlobalUnlock(HGLOBAL(h.0));
            Some(s)
        })();
        let _ = CloseClipboard();
        r
    }
}

pub fn set_clipboard_text(owner: HWND, s: &str) {
    let w: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if !s.is_empty() {
            if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
                let p = GlobalLock(h) as *mut u16;
                if !p.is_null() {
                    std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                    let _ = GlobalUnlock(h);
                    let _ = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(h.0)));
                }
            }
        }
        let _ = CloseClipboard();
    }
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() } },
    }
}

/// Copy whatever is highlighted in the app you are using (Ctrl+C), then put your clipboard back.
pub fn grab_selection(owner: HWND) -> Option<String> {
    unsafe {
        // wait for hotkey modifiers to be released, or the app would see Ctrl+Alt+C
        for _ in 0..40 {
            let held = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN].iter().any(|k| GetAsyncKeyState(k.0 as i32) as u16 & 0x8000 != 0);
            if !held {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let saved = clipboard_text(owner);
        let seq = GetClipboardSequenceNumber();
        let inputs = [key(VK_CONTROL, false), key(VK_C, false), key(VK_C, true), key(VK_CONTROL, true)];
        SendInput(&inputs, size_of::<INPUT>() as i32);
        let mut changed = false;
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if GetClipboardSequenceNumber() != seq {
                changed = true;
                break;
            }
        }
        if !changed {
            return None; // nothing was highlighted (or the app ignores Ctrl+C)
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
        let text = clipboard_text(owner);
        if let Some(s) = saved {
            set_clipboard_text(owner, &s);
        }
        text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn chime_is_valid_wav_and_key_roundtrips() {
        for w in [super::chime_wav(), super::half_wav()] {
            assert_eq!(&w[..4], b"RIFF");
            assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize, w.len() - 44);
        }
        let w = super::chime_wav();
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize, w.len() - 44);
        assert_eq!(super::mask("sk-ant-api03-abcdefgh1234"), "sk-ant-…1234");
        // DPAPI round trip in a temp APPDATA (locked: another test also changes APPDATA)
        let _env = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("APPDATA", std::env::temp_dir().join("rp-key-test")) };
        for (p, k) in [("anthropic", "sk-ant-test-0000"), ("gemini", "AIzaTest-0000")] {
            assert!(super::save_key(p, k));
            let raw = std::fs::read(super::key_path(p).unwrap()).unwrap();
            assert!(!raw.windows(4).any(|w| w == b"0000"), "key must not be stored in clear text");
            assert_eq!(super::load_key(p).as_deref(), Some(k));
        }
        super::remove_key("anthropic");
        assert!(super::load_key("anthropic").is_none());
        assert!(super::load_key("gemini").is_some(), "keys are stored separately");
        super::remove_key("gemini");
        assert_eq!(super::key_provider("sk-ant-api03-abcdefghijklmnopqrstuvwxyz"), Some("anthropic"));
        assert_eq!(super::key_provider("AIzaSyA-abcdefghijklmnopqrstuvwxyz012"), Some("gemini"));
        assert_eq!(super::key_provider("hello"), None);
        assert_eq!(super::key_provider("sk-proj-abcdefghijklmnopqrstuvwxyz0123"), None);
        assert_eq!(super::key_provider("AQ.Ab8RN6abcdefghijklmnopqrstuvwxyz0123"), Some("gemini"));
        assert_eq!(super::key_provider("my key has spaces in it so it is not a key"), None);
    }
}
