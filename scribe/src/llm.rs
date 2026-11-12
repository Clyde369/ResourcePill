//! Summaries with llama.cpp: start llama-server only for this job (localhost, random port), send the
//! transcript, then shut it down so nothing stays in memory. Long transcripts are summarised in
//! parts first, then combined.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SYSTEM: &str = "You summarise transcripts of meetings, calls and videos recorded on the user's PC. \
Lines are labelled with the speaker: 'You' is the user (their microphone); 'Speaker N' are other voices heard through the PC. \
Write concise, factual Markdown with these sections: \
'## Summary' (3-6 sentences), '## Key points' (bullets), '## Decisions' (bullets, or 'None'), \
'## Action items' (bullets with the owner when it is clear, or 'None'). \
Only use what is in the transcript; do not invent names or details. Reply in the transcript's language.";

const CHUNK_CHARS: usize = 60_000; // ~15-20k tokens per part

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn summarise(models: &Path, gguf: &Path, transcript: &str) -> Result<String, String> {
    if !gguf.exists() {
        return Err(format!("summary model not found: {}", gguf.display()));
    }
    let tokens_needed = transcript.len().min(CHUNK_CHARS) / 3 + 3_000;
    let ctx = tokens_needed.clamp(8_192, 32_768);
    let (port, _server) = start(models, gguf, ctx)?;
    if transcript.len() <= CHUNK_CHARS {
        return chat(port, SYSTEM, &format!("Transcript:\n\n{transcript}"), 1500);
    }
    // map-reduce for very long recordings, splitting on line boundaries
    let mut parts = Vec::new();
    let mut cur = String::new();
    for line in transcript.lines() {
        if cur.len() + line.len() > CHUNK_CHARS && !cur.is_empty() {
            parts.push(std::mem::take(&mut cur));
        }
        cur.push_str(line);
        cur.push('\n');
    }
    parts.push(cur);
    let mut notes = String::new();
    for (i, p) in parts.iter().enumerate() {
        crate::emit("STATUS", &[&format!("Summarising part {} of {}…", i + 1, parts.len())]);
        notes += &format!("### Part {}\n{}\n\n", i + 1, chat(port, SYSTEM, &format!("Transcript:\n\n{p}"), 1500)?);
    }
    crate::emit("STATUS", &["Combining the parts…"]);
    chat(port, SYSTEM, &format!("These are summaries of consecutive parts of one long recording. Merge them into one summary.\n\n{notes}"), 1500)
}

const EXPLAIN: &str = "The user highlighted some text on their screen and wants to know what it means. \
Explain it plainly in 2-4 short sentences: what it means and, if useful, what it is for or where it comes from. \
If it is a word or phrase, define it; if it is code, say what it does; if it is a sentence, explain its meaning. \
No preamble, no headings, no Markdown.";

/// Short explanation of highlighted text with a local model (llama.cpp, unloaded afterwards).
pub fn explain_local(models: &Path, gguf: &Path, text: &str) -> Result<String, String> {
    if !gguf.exists() {
        return Err(format!("text model not found: {}", gguf.display()));
    }
    let (port, _server) = start(models, gguf, 8_192)?;
    chat(port, EXPLAIN, &format!("Highlighted text:\n\n{text}"), 400)
}

/// Same with Claude through the user's own API key (Messages API, raw HTTPS).
pub fn explain_claude(key: &str, text: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "model": "claude-opus-5-5",
        "max_tokens": 2048,
        "output_config": {"effort": "low"},
        "fallbacks": "default",
        "system": EXPLAIN,
        "messages": [{"role": "user", "content": format!("Highlighted text:\n\n{text}")}]
    });
    let headers = format!("x-api-key: {key}\r\nanthropic-version: 2026-10-21\r\nanthropic-beta: server-side-fallback-2026-07-01\r\n");
    let (status, resp) = crate::net::post_json("https://api.anthropic.com/v1/messages", &headers, &body.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&resp).map_err(|_| format!("Claude returned HTTP {status}"))?;
    if status != 200 {
        let msg = v["error"]["message"].as_str().unwrap_or("request failed");
        return Err(format!("Claude: {msg} (HTTP {status})"));
    }
    if v["stop_reason"] == "refusal" {
        return Err("Claude declined to explain this text".into());
    }
    let text: Vec<&str> = v["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
    Ok(text.join("").trim().to_string())
}

/// Same with Gemini through the user's own Google AI Studio key (generateContent, raw HTTPS).
pub fn explain_gemini(key: &str, text: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "system_instruction": {"parts": [{"text": EXPLAIN}]},
        "contents": [{"role": "user", "parts": [{"text": format!("Highlighted text:\n\n{text}")}]}]
    });
    let headers = format!("x-goog-api-key: {key}\r\n");
    let url = "https://generativelanguage.googleapis.com/v1beta/models/gemini-flash-latest:generateContent";
    let (status, resp) = crate::net::post_json(url, &headers, &body.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&resp).map_err(|_| format!("Gemini returned HTTP {status}"))?;
    if status != 200 {
        let msg = v["error"]["message"].as_str().unwrap_or("request failed");
        return Err(format!("Gemini: {msg} (HTTP {status})"));
    }
    let text: Vec<&str> = v["candidates"][0]["content"]["parts"].as_array().into_iter().flatten().filter_map(|p| p["text"].as_str()).collect();
    if text.is_empty() {
        return Err("Gemini gave no answer for this text".into());
    }
    Ok(text.join("").trim().to_string())
}

const GEMINI: &str = "https://generativelanguage.googleapis.com";

/// Cloud transcription: upload the recording to Gemini (Files API), then ask for a speaker-labelled
/// transcript plus a summary as JSON. Returns (lines as (seconds, speaker, text), summary).
pub fn gemini_transcribe(key: &str, wav: &Path) -> Result<(Vec<(f64, String, String)>, Option<String>), String> {
    use crate::net::send;
    let audio = std::fs::read(wav).map_err(|e| e.to_string())?;
    let auth = format!("x-goog-api-key: {key}\r\n");
    // 1. start a resumable upload, 2. send the bytes and finalise
    let start = format!("{auth}X-Goog-Upload-Protocol: resumable\r\nX-Goog-Upload-Command: start\r\nX-Goog-Upload-Header-Content-Length: {}\r\nX-Goog-Upload-Header-Content-Type: audio/wav\r\nContent-Type: application/json\r\n", audio.len());
    let (status, body, url) = send("POST", &format!("{GEMINI}/upload/v1beta/files"), &start, br#"{"file":{"display_name":"ResourcePill recording"}}"#, Some("x-goog-upload-url"))?;
    let url = url.filter(|_| status == 200).ok_or_else(|| gemini_error("upload", status, &body))?;
    crate::emit("STATUS", &["Gemini · uploading the audio…"]);
    let (status, body, _) = send("POST", &url, "X-Goog-Upload-Offset: 0\r\nX-Goog-Upload-Command: upload, finalize\r\n", &audio, None)?;
    if status != 200 {
        return Err(gemini_error("upload", status, &body));
    }
    let file: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let (name, uri) = (file["file"]["name"].as_str().unwrap_or_default().to_string(), file["file"]["uri"].as_str().unwrap_or_default().to_string());
    // 3. wait until Google has processed it (usually seconds)
    for _ in 0..120 {
        let (status, body, _) = send("GET", &format!("{GEMINI}/v1beta/{name}"), &auth, b"", None)?;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        match v["state"].as_str() {
            Some("ACTIVE") => break,
            Some("FAILED") => return Err("Gemini could not process the audio".into()),
            _ if status != 200 => return Err(gemini_error("processing", status, &body)),
            _ => std::thread::sleep(std::time::Duration::from_secs(2)),
        }
    }
    crate::emit("STATUS", &["Gemini · transcribing and summarising…"]);
    let prompt = "Transcribe this recording verbatim, in the language spoken. Split it into utterances. \
Label speakers consistently as \"Speaker 1\", \"Speaker 2\", … (use a person's name only if it is clearly said). \
Give each utterance its start time in seconds from the beginning. Then write a summary of the whole recording \
in Markdown with short sections: Overview, Key points, Decisions, Action items (with owners if known). \
If nothing intelligible is said, return no utterances and an empty summary.";
    let body = serde_json::json!({
        "contents": [{"role": "user", "parts": [{"file_data": {"mime_type": "audio/wav", "file_uri": uri}}, {"text": prompt}]}],
        "generationConfig": {
            "responseMimeType": "application/json",
            "maxOutputTokens": 65536, // ponytail: one response; very long meetings (hours) may truncate, chunk the audio if that matters
            "responseSchema": {
                "type": "OBJECT",
                "properties": {
                    "utterances": {"type": "ARRAY", "items": {"type": "OBJECT", "properties": {
                        "start": {"type": "NUMBER"}, "speaker": {"type": "STRING"}, "text": {"type": "STRING"}
                    }, "required": ["start", "speaker", "text"]}},
                    "summary": {"type": "STRING"}
                },
                "required": ["utterances", "summary"]
            }
        }
    });
    let (status, resp) = crate::net::post_json(&format!("{GEMINI}/v1beta/models/gemini-flash-latest:generateContent"), &auth, &body.to_string())?;
    let _ = send("DELETE", &format!("{GEMINI}/v1beta/{name}"), &auth, b"", None); // don't leave the audio on Google's side
    if status != 200 {
        return Err(gemini_error("transcription", status, &resp));
    }
    let v: serde_json::Value = serde_json::from_str(&resp).map_err(|e| e.to_string())?;
    let json: String = v["candidates"][0]["content"]["parts"].as_array().into_iter().flatten().filter_map(|p| p["text"].as_str()).collect();
    parse_transcript(&json)
}

fn parse_transcript(json: &str) -> Result<(Vec<(f64, String, String)>, Option<String>), String> {
    let out: serde_json::Value = serde_json::from_str(json).map_err(|_| "Gemini's answer was cut short or unreadable".to_string())?;
    let lines = out["utterances"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| Some((u["start"].as_f64()?.max(0.0), u["speaker"].as_str()?.trim().to_string(), u["text"].as_str()?.trim().to_string())))
        .filter(|l| !l.2.is_empty())
        .collect();
    let summary = out["summary"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    Ok((lines, summary))
}

fn gemini_error(step: &str, status: u32, body: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    format!("Gemini {step}: {} (HTTP {status})", v["error"]["message"].as_str().unwrap_or("request failed"))
}

#[cfg(test)]
mod gemini_tests {
    #[test]
    fn parses_gemini_transcript_json() {
        let (lines, summary) = super::parse_transcript(r#"{"utterances":[{"start":1.5,"speaker":"Speaker 1","text":" Hello "},{"start":3,"speaker":"Speaker 2","text":""}],"summary":"Overview: a test."}"#).unwrap();
        assert_eq!(lines, vec![(1.5, "Speaker 1".to_string(), "Hello".to_string())]);
        assert_eq!(summary.as_deref(), Some("Overview: a test."));
        assert!(super::parse_transcript("{\"utterances\":[{\"start\":1").is_err());
    }
}

fn start(models: &Path, gguf: &Path, ctx: usize) -> Result<(u16, Server), String> {
    let exe = models.join("llama.cpp").join("llama-server.exe");
    if !exe.exists() {
        return Err(format!("llama.cpp not found: {}", exe.display()));
    }
    let port = TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map_err(|e| e.to_string())?.port();
    // CUDA runtime DLLs ship in a separate folder
    let path = format!("{};{};{}", models.join("cudart").display(), models.join("llama.cpp").display(), std::env::var("PATH").unwrap_or_default());
    let child = Command::new(&exe)
        .args(["-m", &gguf.to_string_lossy(), "-c", &ctx.to_string(), "--host", "127.0.0.1", "--port", &port.to_string(), "--jinja", "-np", "1"])
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags_no_window()
        .spawn()
        .map_err(|e| format!("could not start llama-server: {e}"))?;
    let server = Server(child);
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(180) {
        if http(port, "GET", "/health", "").is_ok_and(|(code, _)| code == 200) {
            return Ok((port, server));
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    Err("the summary model did not start within 3 minutes".into())
}

fn chat(port: u16, system: &str, user: &str, max_tokens: u32) -> Result<String, String> {
    let body = serde_json::json!({
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "temperature": 0.3,
        "max_tokens": max_tokens,
        "chat_template_kwargs": {"enable_thinking": false}
    });
    let (code, resp) = http(port, "POST", "/v1/chat/completions", &body.to_string())?;
    if code != 200 {
        return Err(format!("llama-server returned {code}: {}", resp.chars().take(300).collect::<String>()));
    }
    let v: serde_json::Value = serde_json::from_str(&resp).map_err(|e| e.to_string())?;
    let text = v["choices"][0]["message"]["content"].as_str().unwrap_or_default();
    // drop any reasoning block a model emits despite enable_thinking=false
    let text = match text.find("</think>") {
        Some(i) => &text[i + 8..],
        None => text,
    };
    Ok(text.trim().to_string())
}

/// Minimal HTTP/1.1 client for the local server (no TLS, Connection: close).
fn http(port: u16, method: &str, path: &str, body: &str) -> Result<(u16, String), String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(900))).ok();
    let req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let raw = String::from_utf8_lossy(&raw);
    let (head, rest) = raw.split_once("\r\n\r\n").ok_or("bad HTTP response")?;
    let code = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") { dechunk(rest) } else { rest.to_string() };
    Ok((code, body))
}

fn dechunk(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some((size, tail)) = rest.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 || tail.len() < n {
            break;
        }
        out.push_str(&tail[..n]);
        rest = tail[n..].trim_start_matches("\r\n");
    }
    out
}

trait NoWindow {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

impl NoWindow for Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(0x0800_0000)
    }
}
