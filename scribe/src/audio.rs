//! WASAPI capture: "loopback" of the default playback device (everything the PC plays) and the
//! default microphone. Output is 16 kHz mono f32, padded with silence across gaps so both
//! streams share one timeline that starts when recording starts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use sherpa_onnx::LinearResampler;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Source {
    Playback,
    Mic,
}

pub fn spawn(src: Source, tx: mpsc::Sender<(Source, Vec<f32>)>, stop: Arc<AtomicBool>, start: Instant) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        if let Err(e) = unsafe { run(src, &tx, &stop, start) } {
            // e.g. no microphone: carry on with the other stream
            crate::emit("STATUS", &[&format!("{} unavailable: {e}", if src == Source::Mic { "Microphone" } else { "PC audio" })]);
        }
    })
}

unsafe fn run(src: Source, tx: &mpsc::Sender<(Source, Vec<f32>)>, stop: &AtomicBool, start: Instant) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = devices.GetDefaultAudioEndpoint(if src == Source::Playback { eRender } else { eCapture }, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let fmt = client.GetMixFormat()?;
        let f = &*fmt;
        let (rate, channels, bits) = (f.nSamplesPerSec, f.nChannels as usize, f.wBitsPerSample);
        let tag = f.wFormatTag as u32;
        let float = tag == WAVE_FORMAT_IEEE_FLOAT
            || (tag == WAVE_FORMAT_EXTENSIBLE && { std::ptr::addr_of!((*(fmt as *const WAVEFORMATEXTENSIBLE)).SubFormat).read_unaligned() } == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
        let flags = if src == Source::Playback { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
        // 2 s of room: when the PC is busy (a call, GPU noise removal, the live transcript) and this
        // thread is late, Windows keeps the audio instead of dropping it (no delay: it is a recording)
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 20_000_000, 0, fmt, None)?;
        CoTaskMemFree(Some(fmt as *const _));
        let capture: IAudioCaptureClient = client.GetService()?;
        let resampler = LinearResampler::create(rate as i32, crate::RATE).expect("resampler");
        client.Start()?;

        let mut produced = 0u64; // samples at the source rate, per channel
        let started_at = start.elapsed().as_secs_f64();
        // align this stream with the shared timeline
        let lead = (started_at * rate as f64) as usize;
        if lead > 0 {
            let _ = tx.send((src, resampler.resample(&vec![0.0; lead], false)));
        }
        let mut mono = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(30));
            mono.clear();
            loop {
                let n = capture.GetNextPacketSize()?;
                if n == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let (mut frames, mut bflags) = (0u32, 0u32);
                capture.GetBuffer(&mut data, &mut frames, &mut bflags, None, None)?;
                let silent = bflags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null();
                for i in 0..frames as usize {
                    let mut sum = 0.0f32;
                    if !silent {
                        for c in 0..channels {
                            let k = i * channels + c;
                            sum += match (float, bits) {
                                (true, _) => *(data as *const f32).add(k),
                                (false, 16) => *(data as *const i16).add(k) as f32 / 32768.0,
                                (false, 32) => *(data as *const i32).add(k) as f32 / 2_147_483_648.0,
                                _ => 0.0,
                            };
                        }
                    }
                    mono.push(sum / channels as f32);
                }
                capture.ReleaseBuffer(frames)?;
            }
            produced += mono.len() as u64;
            // loopback delivers nothing while nothing plays: pad silence so timestamps stay true. A mic
            // always delivers, only sometimes late and in a burst (e.g. NVIDIA Broadcast while the GPU is
            // busy): padding then would put silence mid-word and push your voice later, so a mic is
            // padded only once it has really stopped (2 s: unplugged, or the device went away)
            let expected = ((start.elapsed().as_secs_f64() - started_at) * rate as f64) as u64;
            let slack = if src == Source::Mic { rate as u64 * 2 } else { rate as u64 / 5 };
            if expected > produced + slack {
                let gap = (expected - produced) as usize;
                mono.extend(std::iter::repeat_n(0.0, gap));
                produced += gap as u64;
            }
            if !mono.is_empty() && tx.send((src, resampler.resample(&mono, false))).is_err() {
                break;
            }
        }
        let _ = client.Stop();
        Ok(())
    }
}
