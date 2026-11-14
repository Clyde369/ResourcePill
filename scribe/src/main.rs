//! ResourcePill-Scribe: records what the PC plays plus the microphone, transcribes it live with
//! speaker labels, and writes a summary when stopped. Started by the pill only while recording.
//!
//! Protocol (stdout, one line each, tab-separated):
//!   READY | SEG <secs> <speaker> <text> | STATUS <text> | SUMMARY <path> | DONE <folder> | ERROR <text>
//! stdin: "stop" ends the recording.
//!
//! `--bench <asr>` prints the model's real-time factor on its bundled test audio and exits.

mod audio;
mod listen;
mod llm;
mod net;
mod viewer;
mod wav;
#[path = "../../src/catalog.rs"]
#[allow(dead_code)] // the pill uses the descriptive fields
mod catalog;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

use sherpa_onnx::*;

const RATE: i32 = 16_000;
/// Voice clustering threshold for diarization: higher merges more readily into fewer speakers.
const DIARIZE_THRESHOLD: f32 = 0.7; // tuned with TitaNet voiceprints on real recordings; overridable with --speakers-threshold
/// Two voice clusters this alike (cosine of average voiceprints) are the same person.
const SAME_VOICE: f32 = 0.5;
/// A "voice" with less speech than this in total (an "um", a "yes") is folded into its closest match.
const MIN_VOICE_SECS: f32 = 2.5;

fn emit(kind: &str, fields: &[&str]) {
    // escape so tabs/newlines inside a field cannot break the line protocol (the pill unescapes)
    let clean: Vec<String> = fields.iter().map(|f| escape(f)).collect();
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{kind}\t{}", clean.join("\t"));
    let _ = out.flush();
}

/// Backslash-escape for the line protocol: \\ \t \n (carriage returns dropped).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

struct Args {
    models: PathBuf,
    asr: String,
    llm: Option<PathBuf>,
    out: PathBuf,
    threads: i32,
    bench: bool,
    listen: bool, // --listen: tell the pill when people talk in the call app
    voice_apps: Vec<String>, // --voice-apps discord,chrome: which apps listen mode hears
    summarise: Option<PathBuf>,
    download: Option<String>,
    explain: Option<String>, // "local" | "claude" | "gemini"
    final_asr: Option<String>, // re-transcribe the saved audio with this model after stopping
    keep_audio: bool,
    refine: Option<PathBuf>, // re-run the accurate pass on a recording folder that kept its audio
    diar_threshold: f32,
}

fn args() -> Args {
    let mut a = Args {
        models: PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("ResourcePill").join("models"),
        asr: "parakeet-v3".into(),
        llm: None,
        out: PathBuf::new(),
        threads: (std::thread::available_parallelism().map_or(4, |n| n.get()) / 2).clamp(2, 6) as i32,
        bench: false,
        listen: false,
        voice_apps: Vec::new(),
        summarise: None,
        download: None,
        explain: None,
        final_asr: None,
        keep_audio: false,
        refine: None,
        diar_threshold: DIARIZE_THRESHOLD,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().unwrap_or_default();
        match k.as_str() {
            "--models" => a.models = v().into(),
            "--asr" => a.asr = v(),
            "--llm" => a.llm = Some(v()).filter(|s| !s.is_empty()).map(Into::into),
            "--out" => a.out = v().into(),
            "--threads" => a.threads = v().parse().unwrap_or(a.threads),
            "--summarise" => a.summarise = Some(v().into()),
            "--download" => a.download = Some(v()),
            "--explain" => a.explain = Some(v()),
            "--final" => a.final_asr = Some(v()).filter(|s| !s.is_empty()),
            "--keep-audio" => a.keep_audio = true,
            "--listen" => a.listen = true,
            "--voice-apps" => a.voice_apps = v().split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect(),
            "--refine" => a.refine = Some(v().into()),
            "--speakers-threshold" => a.diar_threshold = v().parse().unwrap_or(DIARIZE_THRESHOLD),
            "--bench" => {
                a.bench = true;
                a.asr = v();
            }
            _ => {}
        }
    }
    a
}

fn s(p: &Path) -> Option<String> {
    Some(p.to_string_lossy().into_owned())
}

/// The speech-to-text model by folder name.
fn recognizer(models: &Path, name: &str, threads: i32) -> Result<OfflineRecognizer, String> {
    let dir = models.join(name);
    let mut c = OfflineRecognizerConfig::default();
    c.model_config.num_threads = threads;
    c.model_config.tokens = s(&dir.join("tokens.txt"));
    match name {
        "parakeet-v3" => {
            c.model_config.transducer = OfflineTransducerModelConfig {
                encoder: s(&dir.join("encoder.int8.onnx")),
                decoder: s(&dir.join("decoder.int8.onnx")),
                joiner: s(&dir.join("joiner.int8.onnx")),
            };
            c.model_config.model_type = Some("nemo_transducer".into());
        }
        "cohere-transcribe" => {
            let find = |stem: &str| std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(stem) && n.to_string_lossy().ends_with(".onnx")));
            c.model_config.cohere_transcribe = OfflineCohereTranscribeModelConfig {
                encoder: find("encoder").and_then(|p| s(&p)),
                decoder: find("decoder").and_then(|p| s(&p)),
                language: Some("en".into()),
                use_punct: true,
                use_itn: true,
            };
        }
        other => return Err(format!("unknown speech model {other}")),
    }
    if !dir.join("tokens.txt").exists() {
        return Err(format!("speech model not downloaded: {}", dir.display()));
    }
    OfflineRecognizer::create(&c).ok_or_else(|| format!("could not load speech model {name}"))
}

fn transcribe(rec: &OfflineRecognizer, samples: &[f32]) -> String {
    let st = rec.create_stream();
    st.accept_waveform(RATE, samples);
    rec.decode(&st);
    st.get_result().map(|r| r.text.trim().to_string()).unwrap_or_default()
}

/// Online speaker clustering for the "others" stream: cosine similarity against running centroids.
struct Speakers {
    extractor: SpeakerEmbeddingExtractor,
    known: Vec<(Vec<f32>, u32)>, // centroid (unit length), count
    last: Option<usize>,
}

impl Speakers {
    const SAME: f32 = 0.5; // cosine similarity needed to match a known voice (TitaNet scale)

    fn label(&mut self, samples: &[f32]) -> String {
        let idx = self.embed(samples).map(|e| self.assign(e)).or(self.last).unwrap_or_else(|| {
            self.known.push((Vec::new(), 0));
            self.known.len() - 1
        });
        self.last = Some(idx);
        format!("Speaker {}", idx + 1)
    }

    fn embed(&self, samples: &[f32]) -> Option<Vec<f32>> {
        // under ~1 s a voiceprint is unreliable: keep the previous speaker
        if samples.len() < RATE as usize {
            return None;
        }
        let st = self.extractor.create_stream()?;
        st.accept_waveform(RATE, samples);
        st.input_finished();
        if !self.extractor.is_ready(&st) {
            return None;
        }
        let mut e = self.extractor.compute(&st)?;
        let n = e.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        e.iter_mut().for_each(|x| *x /= n);
        Some(e)
    }

    fn assign(&mut self, e: Vec<f32>) -> usize {
        let best = self
            .known
            .iter()
            .enumerate()
            .filter(|(_, (c, _))| !c.is_empty())
            .map(|(i, (c, _))| (i, c.iter().zip(&e).map(|(a, b)| a * b).sum::<f32>()))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((i, sim)) if sim >= Self::SAME => {
                let (c, n) = &mut self.known[i];
                let w = 1.0 / (*n as f32 + 1.0);
                c.iter_mut().zip(&e).for_each(|(a, b)| *a = *a * (1.0 - w) + b * w);
                let norm = c.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                c.iter_mut().for_each(|x| *x /= norm);
                *n += 1;
                i
            }
            _ => {
                self.known.push((e, 1));
                self.known.len() - 1
            }
        }
    }
}

/// Voice detector. Live uses short phrases so text appears quickly; the final pass uses longer ones
/// (more context per phrase = better accuracy).
fn vad(models: &Path, max_phrase: f32, min_silence: f32) -> Result<VoiceActivityDetector, String> {
    let c = VadModelConfig {
        silero_vad: SileroVadModelConfig {
            model: s(&models.join("vad").join("silero_vad.onnx")),
            threshold: 0.5,
            min_silence_duration: min_silence,
            min_speech_duration: 0.25,
            window_size: 512,
            max_speech_duration: max_phrase,
        },
        sample_rate: RATE,
        num_threads: 1,
        ..Default::default()
    };
    VoiceActivityDetector::create(&c, 60.0).ok_or_else(|| "could not load voice detector".into())
}

/// Feeds audio in the 512-sample windows the voice detector expects and returns finished phrases.
struct Phrases {
    vad: VoiceActivityDetector,
    pending: Vec<f32>,
}

impl Phrases {
    fn push(&mut self, samples: &[f32]) -> Vec<(i64, Vec<f32>)> {
        self.pending.extend_from_slice(samples);
        let whole = self.pending.len() / 512 * 512;
        for w in self.pending[..whole].chunks(512) {
            self.vad.accept_waveform(w);
        }
        self.pending.drain(..whole);
        self.drain()
    }
    fn finish(&mut self) -> Vec<(i64, Vec<f32>)> {
        self.vad.flush();
        self.drain()
    }
    fn drain(&mut self) -> Vec<(i64, Vec<f32>)> {
        let mut out = Vec::new();
        while !self.vad.is_empty() {
            if let Some(seg) = self.vad.front() {
                out.push((seg.start(), seg.samples().to_vec()));
            }
            self.vad.pop();
        }
        out
    }
}

fn bench(a: &Args) -> Result<(), String> {
    let rec = recognizer(&a.models, &a.asr, a.threads)?;
    let dir = a.models.join(&a.asr).join("test_wavs");
    let wavs: Vec<_> = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "wav")).collect();
    // prefer the English sample so the speed figure reflects typical use
    let wav = wavs.iter().find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().starts_with("en"))).or(wavs.first()).ok_or("no test wav")?;
    let w = Wave::read(&wav.to_string_lossy()).ok_or("cannot read test wav")?;
    let secs = w.samples().len() as f64 / w.sample_rate() as f64;
    transcribe(&rec, w.samples()); // warm-up
    let t = Instant::now();
    let text = transcribe(&rec, w.samples());
    let rtf = t.elapsed().as_secs_f64() / secs;
    println!("BENCH	{}	{rtf:.4}	{text}", a.asr);
    // the pill reads this to predict live performance
    let _ = std::fs::write(a.models.join(&a.asr).join("bench.txt"), format!("{rtf:.4}"));
    Ok(())
}

fn main() {
    let a = args();
    let r = if a.listen {
        listen::listen(&a)
    } else if a.bench {
        bench(&a)
    } else if let Some(dir) = &a.refine {
        refine(&a, dir)
    } else if let Some(id) = &a.download {
        download(&a, id)
    } else if let Some(how) = &a.explain {
        explain(&a, how)
    } else if let Some(f) = &a.summarise {
        resummarise(&a, f)
    } else {
        record(&a)
    };
    if let Err(e) = r {
        emit("ERROR", &[&e]);
        std::process::exit(1);
    }
}

fn record(a: &Args) -> Result<(), String> {
    std::fs::create_dir_all(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    // start capturing first: audio queues up while the models load, so the first words are kept
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<(audio::Source, Vec<f32>)>();
    let start = Instant::now();
    let cap_out = audio::spawn(audio::Source::Playback, tx.clone(), stop.clone(), start);
    let cap_mic = audio::spawn(audio::Source::Mic, tx, stop.clone(), start);
    let rec = recognizer(&a.models, &a.asr, a.threads)?;
    let mut speakers = Speakers {
        extractor: SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
            model: s(&voiceprint_model(&a.models)),
            num_threads: 1,
            debug: false,
            provider: None,
        })
        .ok_or("could not load speaker model")?,
        known: Vec::new(),
        last: None,
    };
    let mut others = Phrases { vad: vad(&a.models, 20.0, 0.5)?, pending: Vec::new() };
    let mut mine = Phrases { vad: vad(&a.models, 20.0, 0.5)?, pending: Vec::new() };

    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                if line.map_or(true, |l| l.trim() == "stop") {
                    break;
                }
            }
            stop.store(true, Ordering::Relaxed); // also on EOF: the pill went away
        });
    }
    emit("READY", &[&a.asr]);

    let mut lines: Vec<(f64, String, String)> = Vec::new();
    let mut playback_speech: Vec<(f64, f64)> = Vec::new(); // to recognise mic echo of the speakers
    let mut handle = |src: audio::Source, start_sample: i64, samples: Vec<f32>, lines: &mut Vec<(f64, String, String)>| {
        let t0 = start_sample as f64 / RATE as f64;
        let t1 = t0 + samples.len() as f64 / RATE as f64;
        let who = match src {
            audio::Source::Playback => {
                playback_speech.push((t0, t1));
                speakers.label(&samples)
            }
            audio::Source::Mic => {
                // without a headset the mic hears the speakers: drop phrases mostly covered by playback speech
                let overlap: f64 = playback_speech.iter().map(|&(a, b)| (b.min(t1) - a.max(t0)).max(0.0)).sum();
                if overlap > 0.6 * (t1 - t0) {
                    return;
                }
                "You".into()
            }
        };
        let text = transcribe(&rec, &samples);
        if text.is_empty() {
            return;
        }
        emit("SEG", &[&format!("{t0:.1}"), &who, &text]);
        lines.push((t0, who, text));
    };

    // the audio is kept (16 kHz mono WAV per stream) for the accurate final pass
    let audio_dir = a.out.join("audio");
    let mut wav_out = wav::Writer::create(&audio_dir.join("pc-audio.wav"));
    let mut wav_mic = wav::Writer::create(&audio_dir.join("microphone.wav"));
    let mut save = |src: audio::Source, samples: &[f32]| match src {
        audio::Source::Playback => wav_out.as_mut().map(|w| w.write(samples)),
        audio::Source::Mic => wav_mic.as_mut().map(|w| w.write(samples)),
    };

    while !stop.load(Ordering::Relaxed) {
        let Ok((src, samples)) = rx.recv_timeout(std::time::Duration::from_millis(200)) else { continue };
        save(src, &samples);
        let segs = match src {
            audio::Source::Playback => others.push(&samples),
            audio::Source::Mic => mine.push(&samples),
        };
        for (s0, seg) in segs {
            handle(src, s0, seg, &mut lines);
        }
    }
    let _ = (cap_out.join(), cap_mic.join());
    let dur = start.elapsed().as_secs(); // recording length, before any post-processing
    while let Ok((src, samples)) = rx.try_recv() {
        save(src, &samples);
        let segs = match src {
            audio::Source::Playback => others.push(&samples),
            audio::Source::Mic => mine.push(&samples),
        };
        for (s0, seg) in segs {
            handle(src, s0, seg, &mut lines);
        }
    }
    for (src, p) in [(audio::Source::Playback, &mut others), (audio::Source::Mic, &mut mine)] {
        for (s0, seg) in p.finish() {
            handle(src, s0, seg, &mut lines);
        }
    }
    drop(save);
    let wavs_ok = [wav_out.take().map(|w| w.finish()), wav_mic.take().map(|w| w.finish())].iter().all(|r| matches!(r, Some(Ok(()))));
    drop(handle);

    // cloud (Gemini, key passed in the environment) or the accurate local pass over the saved audio
    let mut model_label = a.asr.clone();
    let gemini = std::env::var("RP_GEMINI_KEY").ok().filter(|k| !k.trim().is_empty());
    let mut cloud_summary = None;
    let mut cloud_done = false;
    if let (Some(key), true) = (&gemini, wavs_ok) {
        emit("STATUS", &["Gemini · preparing the audio…"]);
        let tmp = a.out.join("gemini-upload.wav");
        let r = mix(&audio_dir.join("pc-audio.wav"), &audio_dir.join("microphone.wav"), &tmp).and_then(|_| llm::gemini_transcribe(key.trim(), &tmp));
        let _ = std::fs::remove_file(&tmp);
        match r {
            Ok((better, summary)) if !better.is_empty() => {
                lines = better;
                cloud_summary = summary;
                cloud_done = true;
                model_label = "Gemini (cloud)".into();
            }
            Ok(_) => emit("STATUS", &["Gemini heard no speech; kept the on-PC transcript"]),
            Err(e) => emit("STATUS", &[&format!("{e} · kept the on-PC transcript")]),
        }
    }
    if let Some(fid) = a.final_asr.as_deref().filter(|_| !cloud_done) {
        if !wavs_ok {
            emit("STATUS", &["Couldn't save the audio, so kept the live transcript"]);
        } else {
            match final_pass(a, fid, &audio_dir.join("pc-audio.wav"), &audio_dir.join("microphone.wav")) {
                Ok(better) => {
                    lines = better;
                    model_label = format!("{fid} (accurate pass after a live {} pass)", a.asr);
                }
                Err(e) => emit("STATUS", &[&format!("Accurate pass skipped ({e}); kept the live transcript")]),
            }
        }
    }
    // one listenable file (your mic + the PC mixed); the separate work streams are removed
    let mut audio_file = None;
    if a.keep_audio && wavs_ok {
        emit("STATUS", &["Saving the audio…"]);
        if mix(&audio_dir.join("pc-audio.wav"), &audio_dir.join("microphone.wav"), &a.out.join("recording.wav")).is_ok() {
            audio_file = Some("recording.wav");
        }
    }
    let _ = std::fs::remove_dir_all(&audio_dir);

    // transcript
    lines.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut md = format!("# Transcript\n\nRecorded {} · {}:{:02} · speech model: {model_label}\n\n", local_stamp(), dur / 60, dur % 60);
    for (t, who, text) in &lines {
        md += &format!("**[{:02}:{:02}] {who}:** {text}\n\n", (*t as u64) / 60, (*t as u64) % 60);
    }
    let tpath = a.out.join("transcript.md");
    std::fs::write(&tpath, &md).map_err(|e| e.to_string())?;
    emit("TRANSCRIPT", &[&tpath.to_string_lossy()]);

    // summary
    let mut summary_text: Option<String> = None;
    if let Some(summary) = cloud_summary {
        let p = a.out.join("summary.md");
        std::fs::write(&p, format!("# Meeting summary\n\n{summary}\n")).map_err(|e| e.to_string())?;
        emit("SUMMARY", &[&p.to_string_lossy()]);
        summary_text = Some(summary);
    } else if let Some(gguf) = &a.llm {
        if lines.is_empty() {
            emit("STATUS", &["Nothing was said, so there is nothing to summarise"]);
        } else {
            emit("STATUS", &["Summarising…"]);
            let plain: String = lines.iter().map(|(t, w, x)| format!("[{:02}:{:02}] {w}: {x}\n", (*t as u64) / 60, (*t as u64) % 60)).collect();
            match llm::summarise(&a.models, gguf, &plain) {
                Ok(summary) => {
                    let p = a.out.join("summary.md");
                    std::fs::write(&p, format!("# Meeting summary\n\n{summary}\n")).map_err(|e| e.to_string())?;
                    emit("SUMMARY", &[&p.to_string_lossy()]);
                    summary_text = Some(summary);
                }
                Err(e) => emit("ERROR", &[&format!("summary failed: {e}")]),
            }
        }
    }

    // data for the Recordings window
    let mut speakers: Vec<&str> = Vec::new();
    for (_, who, _) in &lines {
        if !speakers.contains(&who.as_str()) {
            speakers.push(who);
        }
    }
    let rec = serde_json::json!({
        "title": title(summary_text.as_deref(), &lines),
        "date": local_stamp(),
        "duration": dur,
        "model": model_label,
        "speakers": speakers,
        "lines": lines.iter().map(|(t, who, text)| serde_json::json!({"t": (t * 10.0).round() / 10.0, "who": who, "text": text})).collect::<Vec<_>>(),
        "summary": summary_text,
        "audio": audio_file,
    });
    viewer::write_rec(&a.out, &rec)?;
    emit("DONE", &[&a.out.to_string_lossy()]);
    Ok(())
}

/// A short title: the summary's first sentence, else the first thing said.
fn title(summary: Option<&str>, lines: &[(f64, String, String)]) -> String {
    let from_summary = summary.and_then(|s| s.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with(['-', '*'])));
    let raw = from_summary.or(lines.first().map(|l| l.2.as_str())).unwrap_or("Recording");
    let first = raw.split_inclusive(['.', '!', '?']).next().unwrap_or(raw).trim();
    let t: String = first.chars().take(80).collect();
    if first.chars().count() > 80 { format!("{}…", t.trim_end()) } else { t }
}

/// Mix the PC and mic streams into one file for listening back.
fn mix(pc: &Path, mic: &Path, out: &Path) -> Result<(), String> {
    let load = |p: &Path| Wave::read(&p.to_string_lossy()).map(|w| w.samples().to_vec()).unwrap_or_default();
    let (a, b) = (load(pc), load(mic));
    let mut w = wav::Writer::create(out).ok_or("cannot write audio")?;
    let n = a.len().max(b.len());
    let mut buf = Vec::with_capacity(RATE as usize);
    for i in 0..n {
        let s = a.get(i).copied().unwrap_or(0.0) + b.get(i).copied().unwrap_or(0.0);
        buf.push(s.clamp(-1.0, 1.0));
        if buf.len() == buf.capacity() {
            w.write(&buf);
            buf.clear();
        }
    }
    w.write(&buf);
    w.finish().map_err(|e| e.to_string())
}

fn local_stamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{}-{:02}-{:02} {:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute)
}

/// Summarise an existing transcript file (e.g. with a different model); writes summary.md beside it.
fn resummarise(a: &Args, transcript: &Path) -> Result<(), String> {
    let gguf = a.llm.as_ref().ok_or("--llm <model.gguf> is required")?;
    let text = std::fs::read_to_string(transcript).map_err(|e| format!("{}: {e}", transcript.display()))?;
    let t = Instant::now();
    let summary = llm::summarise(&a.models, gguf, &text)?;
    let out = transcript.with_file_name("summary.md");
    std::fs::write(&out, format!("# Meeting summary\n\n{summary}\n")).map_err(|e| e.to_string())?;
    emit("SUMMARY", &[&out.to_string_lossy(), &format!("{:.1}", t.elapsed().as_secs_f64())]);
    Ok(())
}

/// Install one catalog model: download (resumable), verify SHA-256, unpack, mark installed.
fn download(a: &Args, id: &str) -> Result<(), String> {
    let m = catalog::find(id).ok_or_else(|| format!("unknown model {id}"))?;
    let dir = a.models.join(m.id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for f in m.files {
        let name = f.url.rsplit('/').next().unwrap_or("download");
        let part = dir.join(format!("{name}.part"));
        net::download(f.url, &part, |done, total| {
            emit("PROGRESS", &[m.id, &done.to_string(), &total.to_string()]);
        })?;
        if let Some(want) = f.sha256 {
            emit("STATUS", &[&format!("Checking {}…", m.name)]);
            let got = net::sha256_file(&part)?;
            if got != want {
                let _ = std::fs::remove_file(&part);
                return Err(format!("{} failed its checksum, so it was deleted. Please try again.", m.name));
            }
        }
        if f.unpack {
            emit("STATUS", &[&format!("Unpacking {}…", m.name)]);
            let archive = dir.join(name);
            std::fs::rename(&part, &archive).map_err(|e| e.to_string())?;
            // Windows' built-in tar handles .tar.bz2 and .zip; sherpa archives have one top folder
            let mut cmd = std::process::Command::new("tar");
            cmd.arg("-xf").arg(&archive).arg("-C").arg(&dir);
            if name.ends_with(".tar.bz2") {
                cmd.arg("--strip-components=1");
            }
            let ok = cmd.status().map_err(|e| e.to_string())?.success();
            let _ = std::fs::remove_file(&archive);
            if !ok {
                return Err(format!("could not unpack {}", m.name));
            }
        } else {
            std::fs::rename(&part, dir.join(name)).map_err(|e| e.to_string())?;
        }
    }
    if !dir.join(m.marker).exists() {
        return Err(format!("{} downloaded but looks incomplete", m.name));
    }
    emit("INSTALLED", &[m.id]);
    Ok(())
}

/// Explain highlighted text. stdin: first line = API key (empty for local), the rest = the text.
fn explain(a: &Args, how: &str) -> Result<(), String> {
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).map_err(|e| e.to_string())?;
    let (key, text) = input.split_once('\n').unwrap_or(("", &input));
    let text = text.trim();
    if text.is_empty() {
        return Err("No text was highlighted".into());
    }
    let answer = match how {
        "claude" => llm::explain_claude(key.trim(), text)?,
        "gemini" => llm::explain_gemini(key.trim(), text)?,
        _ => {
            let gguf = a.llm.as_ref().ok_or("no local text model selected")?;
            llm::explain_local(&a.models, gguf, text)?
        }
    };
    emit("EXPLAIN", &[&answer]);
    Ok(())
}

/// Accurate pass after stopping: longer phrases with padding, the chosen (usually most accurate)
/// model on all cores, echo removal, and speaker labels re-grouped with the whole recording in view.
fn final_pass(a: &Args, model: &str, pc: &Path, mic: &Path) -> Result<Vec<(f64, String, String)>, String> {
    emit("STATUS", &["Improving accuracy · loading model…"]);
    let threads = (std::thread::available_parallelism().map_or(4, |n| n.get()) - 1).clamp(2, 12) as i32;
    let rec = recognizer(&a.models, model, threads)?;
    let load = |p: &Path| Wave::read(&p.to_string_lossy()).map(|w| w.samples().to_vec()).unwrap_or_default();
    let (pc, mic) = (load(pc), load(mic));

    // phrases: up to 30 s, padded 0.25 s each side so edge words are not clipped
    let phrases = |audio: &[f32]| -> Result<Vec<(usize, usize)>, String> {
        let mut p = Phrases { vad: vad(&a.models, 30.0, 0.35)?, pending: Vec::new() };
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for chunk in audio.chunks(RATE as usize) {
            spans.extend(p.push(chunk).into_iter().map(|(s, v)| (s as usize, s as usize + v.len())));
        }
        spans.extend(p.finish().into_iter().map(|(s, v)| (s as usize, s as usize + v.len())));
        let pad = RATE as usize / 4;
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (s, e) in spans {
            let (s, e) = (s.saturating_sub(pad), (e + pad).min(audio.len()));
            match out.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e), // padding made them touch: one phrase
                _ => out.push((s, e)),
            }
        }
        Ok(out)
    };
    let (pc_spans, mic_spans) = (phrases(&pc)?, phrases(&mic)?);
    let total: usize = pc_spans.iter().chain(&mic_spans).map(|(s, e)| e - s).sum::<usize>().max(1);
    let mut done = 0usize;
    let mut last_pct = 0;
    let mut tick = |n: usize| {
        done += n;
        let pct = done * 100 / total;
        if pct >= last_pct + 5 {
            last_pct = pct;
            emit("STATUS", &[&format!("Improving accuracy · {pct}%")]);
        }
    };

    // the other side of the call: text + voiceprints
    let mut speakers = Speakers {
        extractor: SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
            model: s(&voiceprint_model(&a.models)),
            num_threads: 2,
            debug: false,
            provider: None,
        })
        .ok_or("could not load speaker model")?,
        known: Vec::new(),
        last: None,
    };
    let mut theirs: Vec<(f64, f64, String, Option<Vec<f32>>)> = Vec::new();
    let mut ids: Vec<Option<usize>> = Vec::new();
    if let Some(turns) = diarize(&a.models, &pc, threads, a.diar_threshold) {
        // speaker turns from the diarizer: one voice per turn, even with no pause between people
        emit("STATUS", &[&format!("Improving accuracy · {} voices found", turns.iter().map(|t| t.2).collect::<std::collections::HashSet<_>>().len())]);
        let pc_total: usize = pc_spans.iter().map(|(s, e)| e - s).sum();
        let turn_total: usize = turns.iter().map(|(s, e, _)| e - s).sum::<usize>().max(1);
        for &(s, e, who) in &turns {
            let text = transcribe(&rec, &pc[s..e]);
            tick((e - s) * pc_total / turn_total); // keep the progress estimate in the same units
            if !text.is_empty() {
                theirs.push((s as f64 / RATE as f64, e as f64 / RATE as f64, text, None));
                ids.push(Some(who as usize));
            }
        }
    } else {
        // fallback without the segmentation model: one voice per pause-separated phrase
        for &(s, e) in &pc_spans {
            let text = transcribe(&rec, &pc[s..e]);
            tick(e - s);
            if !text.is_empty() {
                theirs.push((s as f64 / RATE as f64, e as f64 / RATE as f64, text, speakers.embed(&pc[s..e])));
            }
        }
        // first an online pass to find the voices, then every phrase is re-assigned to the closest
        // voice with the whole recording known; voiceless short phrases take the nearest label
        for t in &theirs {
            if let Some(e) = &t.3 {
                speakers.assign(e.clone());
            }
        }
        let centroids: Vec<Vec<f32>> = speakers.known.iter().filter(|(c, n)| !c.is_empty() && *n >= 2).map(|(c, _)| c.clone()).collect();
        let closest = |e: &[f32]| -> Option<usize> {
            centroids.iter().enumerate().map(|(i, c)| (i, c.iter().zip(e).map(|(x, y)| x * y).sum::<f32>())).max_by(|x, y| x.1.total_cmp(&y.1)).map(|(i, _)| i)
        };
        ids = theirs.iter().map(|t| t.3.as_deref().and_then(closest)).collect();
        for i in 0..ids.len() {
            if ids[i].is_none() {
                ids[i] = (0..ids.len()).filter(|&j| ids[j].is_some()).min_by(|&x, &y| (theirs[x].0 - theirs[i].0).abs().total_cmp(&(theirs[y].0 - theirs[i].0).abs())).and_then(|j| ids[j]);
            }
        }
    }
    // number speakers by when they first speak
    let mut order: Vec<usize> = Vec::new();
    for id in ids.iter().flatten() {
        if !order.contains(id) {
            order.push(*id);
        }
    }
    let mut lines: Vec<(f64, String, String)> = theirs
        .iter()
        .zip(&ids)
        .map(|(t, id)| (t.0, format!("Speaker {}", id.and_then(|i| order.iter().position(|&o| o == i)).map_or(1, |p| p + 1)), t.2.clone()))
        .collect();

    // you: skip phrases that are mostly the speakers leaking into the mic
    for &(s, e) in &mic_spans {
        let (t0, t1) = (s as f64 / RATE as f64, e as f64 / RATE as f64);
        let overlap: f64 = theirs.iter().map(|t| (t.1.min(t1) - t.0.max(t0)).max(0.0)).sum();
        if overlap > 0.6 * (t1 - t0) {
            tick(e - s);
            continue;
        }
        let text = transcribe(&rec, &mic[s..e]);
        tick(e - s);
        if !text.is_empty() {
            lines.push((t0, "You".into(), text));
        }
    }
    lines.sort_by(|x, y| x.0.total_cmp(&y.0));
    Ok(lines)
}

/// Re-transcribe a recording folder (with kept audio) accurately and rewrite its transcript.
fn refine(a: &Args, dir: &Path) -> Result<(), String> {
    let model = a.final_asr.clone().unwrap_or_else(|| a.asr.clone());
    let audio = dir.join("audio");
    let t = Instant::now();
    // separate PC + mic streams if kept; otherwise the mixed recording.wav (then no "You" label)
    let separate = audio.join("pc-audio.wav").exists();
    let (pc_in, mic_in) = if separate { (audio.join("pc-audio.wav"), audio.join("microphone.wav")) } else { (dir.join("recording.wav"), dir.join("no-microphone.wav")) };
    let lines = final_pass(a, &model, &pc_in, &mic_in)?;
    let mut md = format!("# Transcript\n\nRe-transcribed {} · speech model: {model}\n\n", local_stamp());
    for (t, who, text) in &lines {
        md += &format!("**[{:02}:{:02}] {who}:** {text}\n\n", (*t as u64) / 60, (*t as u64) % 60);
    }
    let p = dir.join("transcript.md");
    std::fs::write(&p, md).map_err(|e| e.to_string())?;
    emit("TRANSCRIPT", &[&p.to_string_lossy(), &format!("{:.1}", t.elapsed().as_secs_f64())]);
    // refresh the listenable audio, the summary and the Recordings window data too
    let pc = if separate { audio.join("pc-audio.wav") } else { dir.join("recording.wav") };
    // separate streams: (re)mix them for listening; mixed-only recordings keep their recording.wav
    let audio_file = if separate { mix(&pc, &audio.join("microphone.wav"), &dir.join("recording.wav")).ok().map(|_| "recording.wav") } else { Some("recording.wav") };
    let plain: String = lines.iter().map(|(t, w, x)| format!("[{:02}:{:02}] {w}: {x}\n", (*t as u64) / 60, (*t as u64) % 60)).collect();
    let summary = a.llm.as_ref().filter(|_| !lines.is_empty()).and_then(|g| llm::summarise(&a.models, g, &plain).ok());
    if let Some(s) = &summary {
        let _ = std::fs::write(dir.join("summary.md"), format!("# Meeting summary\n\n{s}\n"));
    }
    let mut speakers: Vec<&str> = Vec::new();
    for (_, who, _) in &lines {
        if !speakers.contains(&who.as_str()) {
            speakers.push(who);
        }
    }
    let len = |p: &Path| Wave::read(&p.to_string_lossy()).map_or(0, |w| w.samples().len() as u64);
    let secs = len(&pc).max(len(&audio.join("microphone.wav"))) / RATE as u64;
    let rec = serde_json::json!({
        "title": title(summary.as_deref(), &lines),
        "date": local_stamp(),
        "duration": secs,
        "model": model,
        "speakers": speakers,
        "lines": lines.iter().map(|(t, who, text)| serde_json::json!({"t": (t * 10.0).round() / 10.0, "who": who, "text": text})).collect::<Vec<_>>(),
        "summary": summary,
        "audio": audio_file,
    });
    viewer::write_rec(dir, &rec)?;
    Ok(())
}

/// Speaker diarization (pyannote segmentation 3.0 + voiceprints): who spoke when, as turns of one
/// speaker each (sample start, sample end, speaker). None if the segmentation model is missing.
fn diarize(models: &Path, audio: &[f32], threads: i32, threshold: f32) -> Option<Vec<(usize, usize, i32)>> {
    let seg = models.join("segmentation").join("model.onnx");
    if !seg.exists() || audio.len() < RATE as usize * 2 {
        return None;
    }
    let config = OfflineSpeakerDiarizationConfig {
        segmentation: OfflineSpeakerSegmentationModelConfig {
            pyannote: OfflineSpeakerSegmentationPyannoteModelConfig { model: s(&seg), ..Default::default() },
            num_threads: threads,
            debug: false,
            provider: None,
        },
        embedding: SpeakerEmbeddingExtractorConfig {
            model: s(&voiceprint_model(models)),
            num_threads: threads,
            debug: false,
            provider: None,
        },
        clustering: FastClusteringConfig { num_clusters: -1, threshold, compute_confidence: false },
        min_duration_on: 0.3,
        min_duration_off: 0.5,
    };
    let d = OfflineSpeakerDiarization::create(&config)?;
    let segs = d.process(audio)?.sort_by_start_time();
    // merge consecutive pieces of the same voice into turns (short gaps, up to 30 s per turn)
    let pad = RATE as usize / 8;
    let mut turns: Vec<(usize, usize, i32)> = Vec::new();
    for sg in segs {
        let (s0, e0) = ((sg.start * RATE as f32) as usize, ((sg.end * RATE as f32) as usize).min(audio.len()));
        if e0 <= s0 {
            continue;
        }
        match turns.last_mut() {
            Some(t) if t.2 == sg.speaker && s0.saturating_sub(t.1) < RATE as usize * 4 / 5 && e0 - t.0 < RATE as usize * 30 => t.1 = e0,
            _ => turns.push((s0, e0, sg.speaker)),
        }
    }
    merge_voices(models, audio, &mut turns);
    // a little padding so edge words are kept, without reaching into the next speaker
    let n = turns.len();
    for i in 0..n {
        let lo = if i > 0 { turns[i - 1].1 } else { 0 };
        let hi = if i + 1 < n { turns[i + 1].0 } else { audio.len() };
        turns[i].0 = turns[i].0.saturating_sub(pad).max(lo.min(turns[i].0));
        turns[i].1 = (turns[i].1 + pad).min(hi.max(turns[i].1)).min(audio.len());
    }
    Some(turns)
}

/// Second look at the diarizer's voices: clusters whose average voiceprints sound alike are the
/// same person, and a "voice" with only a few seconds of speech (a "yes", an "um") is too thin to
/// be a separate person, so it joins the voice it sounds closest to.
fn merge_voices(models: &Path, audio: &[f32], turns: &mut [(usize, usize, i32)]) {
    let Some(extractor) = SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
        model: s(&voiceprint_model(models)),
        num_threads: 2,
        debug: false,
        provider: None,
    }) else {
        return;
    };
    let sp = Speakers { extractor, known: Vec::new(), last: None };
    // per voice: summed voiceprint (longest turns first, up to 20) and total speaking time
    let mut voices: std::collections::BTreeMap<i32, (Vec<f32>, usize)> = std::collections::BTreeMap::new();
    let mut order: Vec<usize> = (0..turns.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(turns[i].1 - turns[i].0));
    let mut used: std::collections::HashMap<i32, usize> = std::collections::HashMap::new();
    for i in order {
        let (s0, e0, v) = turns[i];
        let entry = voices.entry(v).or_insert((Vec::new(), 0));
        entry.1 += e0 - s0;
        let n = used.entry(v).or_insert(0);
        if *n < 20 {
            if let Some(e) = sp.embed(&audio[s0..e0]) {
                if entry.0.is_empty() {
                    entry.0 = vec![0.0; e.len()];
                }
                entry.0.iter_mut().zip(&e).for_each(|(a, b)| *a += b);
                *n += 1;
            }
        }
    }
    let unit = |v: &[f32]| {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        v.iter().map(|x| x / n).collect::<Vec<f32>>()
    };
    let mut c: Vec<(i32, Vec<f32>, usize)> = voices.into_iter().map(|(k, (v, d))| (k, if v.is_empty() { v } else { unit(&v) }, d)).collect();
    let sim = |a: &[f32], b: &[f32]| if a.is_empty() || b.is_empty() { -1.0 } else { a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() };
    if std::env::var_os("RP_DEBUG_VOICES").is_some() {
        for i in 0..c.len() {
            for j in i + 1..c.len() {
                emit("STATUS", &[&format!("voices {} ({:.1}s) / {} ({:.1}s): similarity {:.2}", c[i].0, c[i].2 as f32 / RATE as f32, c[j].0, c[j].2 as f32 / RATE as f32, sim(&c[i].1, &c[j].1))]);
            }
        }
    }
    let mut remap: std::collections::HashMap<i32, i32> = std::collections::HashMap::new();
    loop {
        if c.len() < 2 {
            break;
        }
        // most similar pair, and the thinnest voice
        let mut best = (0, 1, -2.0f32);
        for i in 0..c.len() {
            for j in i + 1..c.len() {
                let s = sim(&c[i].1, &c[j].1);
                if s > best.2 {
                    best = (i, j, s);
                }
            }
        }
        let thin = (0..c.len()).min_by_key(|&i| c[i].2).unwrap();
        let (from, into) = if best.2 >= SAME_VOICE {
            (best.1, best.0)
        } else if (c[thin].2 as f32) < RATE as f32 * MIN_VOICE_SECS {
            let target = (0..c.len()).filter(|&j| j != thin).max_by(|&x, &y| sim(&c[thin].1, &c[x].1).total_cmp(&sim(&c[thin].1, &c[y].1))).unwrap();
            (thin, target)
        } else {
            break;
        };
        if std::env::var_os("RP_DEBUG_VOICES").is_some() {
            emit("STATUS", &[&format!("merge voice {} into {} (similarity {:.2}, {:.1}s)", c[from].0, c[into].0, sim(&c[from].1, &c[into].1), c[from].2 as f32 / RATE as f32)]);
        }
        let (fk, fv, fd) = c.remove(from);
        let into = if from < into { into - 1 } else { into };
        let (ik, iv, id) = &mut c[into];
        // weighted voiceprint of the merged voice
        if !fv.is_empty() && !iv.is_empty() {
            let w = fd as f32 / (fd + *id) as f32;
            let mixed: Vec<f32> = iv.iter().zip(&fv).map(|(a, b)| a * (1.0 - w) + b * w).collect();
            *iv = unit(&mixed);
        } else if iv.is_empty() {
            *iv = fv;
        }
        *id += fd;
        remap.insert(fk, *ik);
    }
    for t in turns.iter_mut() {
        let mut v = t.2;
        while let Some(&n) = remap.get(&v) {
            v = n;
        }
        t.2 = v;
    }
}

/// The best installed voiceprint model: TitaNet-Large if present (sharper at telling voices apart),
/// otherwise CAM++. RP_VOICEPRINT=<file> overrides it for testing.
fn voiceprint_model(models: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("RP_VOICEPRINT") {
        return PathBuf::from(p);
    }
    let titanet = models.join("voiceprint").join("nemo_en_titanet_large.onnx");
    if titanet.exists() { titanet } else { models.join("speaker").join("3dspeaker_speech_campplus_sv_en_voxceleb_16k.onnx") }
}
