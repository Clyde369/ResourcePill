//! HTTPS via WinHTTP (Windows' own TLS stack, no extra libraries): resumable downloads with
//! progress, SHA-256 checks via CNG, and small JSON POSTs.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;

use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::BCRYPT_SHA256_ALG_HANDLE;
use windows::core::{HSTRING, PCWSTR};

struct H(*mut core::ffi::c_void);

impl Drop for H {
    fn drop(&mut self) {
        if !self.0.is_null() {
            let _ = unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn wide(s: &str) -> HSTRING {
    HSTRING::from(s)
}

/// Opens a request; returns (session, connection, request) handles plus HTTP status.
fn open(method: &str, url: &str, headers: &str, body: &[u8]) -> Result<(H, H, H, u32), String> {
    let (scheme_https, rest) = if let Some(r) = url.strip_prefix("https://") { (true, r) } else if let Some(r) = url.strip_prefix("http://") { (false, r) } else { return Err(format!("bad url {url}")) };
    let (host, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    unsafe {
        let s = H(WinHttpOpen(&wide("ResourcePill/1.0"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if s.0.is_null() {
            return Err("WinHttpOpen failed".into());
        }
        let port = if scheme_https { 443 } else { 80 };
        let c = H(WinHttpConnect(s.0, &wide(host), port, 0));
        if c.0.is_null() {
            return Err(format!("cannot connect to {host}"));
        }
        // generous: a cloud transcription of a long recording can take minutes to answer
        let _ = WinHttpSetTimeouts(s.0, 0, 60_000, 600_000, 900_000);
        let flags = if scheme_https { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let r = H(WinHttpOpenRequest(c.0, &wide(method), &wide(&path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags));
        if r.0.is_null() {
            return Err("WinHttpOpenRequest failed".into());
        }
        let hw: Vec<u16> = headers.encode_utf16().collect();
        let hdrs = if hw.is_empty() { None } else { Some(&hw[..]) };
        let ptr = if body.is_empty() { None } else { Some(body.as_ptr() as *const core::ffi::c_void) };
        WinHttpSendRequest(r.0, hdrs, ptr, body.len() as u32, body.len() as u32, 0).map_err(|e| format!("request to {host} failed: {e}"))?;
        WinHttpReceiveResponse(r.0, std::ptr::null_mut()).map_err(|e| format!("no response from {host}: {e}"))?;
        let mut status = 0u32;
        let mut len = 4u32;
        let _ = WinHttpQueryHeaders(r.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut status as *mut u32 as *mut _), &mut len, std::ptr::null_mut());
        Ok((s, c, r, status))
    }
}

fn content_length(r: &H) -> Option<u64> {
    let mut buf = [0u16; 32];
    let mut len = (buf.len() * 2) as u32;
    unsafe { WinHttpQueryHeaders(r.0, WINHTTP_QUERY_CONTENT_LENGTH, PCWSTR::null(), Some(buf.as_mut_ptr() as *mut _), &mut len, std::ptr::null_mut()) }.ok()?;
    String::from_utf16_lossy(&buf[..len as usize / 2]).trim().parse().ok()
}

fn read_chunk(r: &H, buf: &mut [u8]) -> Result<usize, String> {
    let mut n = 0u32;
    unsafe { WinHttpReadData(r.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut n) }.map_err(|e| format!("download interrupted: {e}"))?;
    Ok(n as usize)
}

/// POST JSON, return (status, body).
pub fn post_json(url: &str, headers: &str, body: &str) -> Result<(u32, String), String> {
    send("POST", url, &format!("Content-Type: application/json\r\n{headers}"), body.as_bytes(), None).map(|(s, b, _)| (s, b))
}

/// Any request; returns (status, body, the value of response header `want` if asked for).
pub fn send(method: &str, url: &str, headers: &str, body: &[u8], want: Option<&str>) -> Result<(u32, String, Option<String>), String> {
    let (_s, _c, r, status) = open(method, url, headers, body)?;
    let header = want.and_then(|name| {
        let mut buf = vec![0u16; 4096];
        let mut len = (buf.len() * 2) as u32;
        unsafe { WinHttpQueryHeaders(r.0, WINHTTP_QUERY_CUSTOM, &wide(name), Some(buf.as_mut_ptr() as *mut _), &mut len, std::ptr::null_mut()) }.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize / 2]))
    });
    Ok((status, read_all(&r)?, header))
}

fn read_all(r: &H) -> Result<String, String> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = read_chunk(r, &mut buf)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Download `url` to `dest`, resuming a partial file. `progress(done, total)` is called as it goes.
pub fn download(url: &str, dest: &Path, mut progress: impl FnMut(u64, u64)) -> Result<(), String> {
    let have = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    let range = if have > 0 { format!("Range: bytes={have}-\r\n") } else { String::new() };
    let (_s, _c, r, status) = open("GET", url, &range, &[])?;
    let (mut done, append) = match status {
        206 => (have, true),
        200 => (0, false),
        416 => return Ok(()), // already complete
        s => return Err(format!("download failed: HTTP {s}")),
    };
    let total = content_length(&r).map(|l| l + done).unwrap_or(0);
    let mut f = OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(dest).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 20];
    let mut last = std::time::Instant::now();
    loop {
        let n = read_chunk(&r, &mut buf)?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if last.elapsed().as_millis() > 300 {
            progress(done, total);
            last = std::time::Instant::now();
        }
    }
    progress(done, total);
    if total > 0 && done < total {
        return Err("download ended early; run it again to resume".into());
    }
    Ok(())
}

/// SHA-256 of a file as lowercase hex, hashed incrementally with Windows CNG.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    use windows::Win32::Security::Cryptography::{BCRYPT_HASH_HANDLE, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptHashData};
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    unsafe {
        let mut h = BCRYPT_HASH_HANDLE::default();
        BCryptCreateHash(BCRYPT_SHA256_ALG_HANDLE, &mut h, None, None, 0).ok().map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 8 << 20];
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            BCryptHashData(h, &buf[..n], 0).ok().map_err(|e| e.to_string())?;
        }
        let mut out = [0u8; 32];
        BCryptFinishHash(h, &mut out, 0).ok().map_err(|e| e.to_string())?;
        let _ = BCryptDestroyHash(h);
        Ok(out.iter().map(|b| format!("{b:02x}")).collect())
    }
}

