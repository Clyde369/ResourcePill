//! In-app updates from GitHub Releases. The helper asks the API (`--update-check`) and downloads
//! the ResourcePill.exe asset over HTTPS (`--update-get`); this side decides whether it is newer,
//! checks the SHA-256 GitHub published for it, and swaps the exe.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone)]
pub struct Release {
    pub tag: String,
    pub url: String,
    sha256: String, // lowercase hex, from the API's "digest"
}

static LATEST: Mutex<Option<Release>> = Mutex::new(None);

/// A newer release, once a check has found one.
pub fn available() -> Option<Release> {
    LATEST.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Checks shortly after start, then once a day, on its own thread. Offline, no releases or
/// anything odd = no update, silently.
pub fn spawn_checks() {
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(30));
        // the copy left by the last update (still in use when the new one started, so retried here)
        if let Some(me) = std::env::current_exe().ok().filter(|_| safe_to_replace()) {
            let _ = std::fs::remove_file(old_path(&me));
        }
        if let Some(r) = check() {
            *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        }
        std::thread::sleep(Duration::from_secs(24 * 3600));
    });
}

fn check() -> Option<Release> {
    // a folder install (helper beside the exe, e.g. a dev build) is updated by hand
    let me = std::env::current_exe().ok()?;
    if crate::bundle::helper_dir().as_deref() == me.parent() {
        return None;
    }
    let out = Command::new(crate::recorder::helper()?).arg("--update-check").stdin(Stdio::null()).stderr(Stdio::null()).creation_flags(0x0800_0000 | 0x0000_4000).output().ok()?;
    let line = String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix("RELEASE\t").map(str::to_string))?;
    parse(&line, env!("CARGO_PKG_VERSION"))
}

/// `tag \t url \t digest` from the helper → a release newer than `current` with a SHA-256 digest.
fn parse(line: &str, current: &str) -> Option<Release> {
    let mut f = line.split('\t').map(crate::recorder::unescape);
    let (tag, url, digest) = (f.next()?, f.next()?, f.next()?);
    if version(&tag)? <= version(current)? {
        return None;
    }
    Some(Release { sha256: sha256_hex(&digest)?, tag, url })
}

/// "v1.2.3" or "1.2.3" → (1, 2, 3); anything else (pre-releases too) → None.
fn version(s: &str) -> Option<(u64, u64, u64)> {
    let mut p = s.strip_prefix('v').unwrap_or(s).split('.').map(|x| x.parse::<u64>().ok());
    let v = (p.next()??, p.next()??, p.next()??);
    p.next().is_none().then_some(v)
}

/// GitHub's `"sha256:<64 hex>"` → the hex, lowercase. No digest (or another algorithm) → None.
fn sha256_hex(digest: &str) -> Option<String> {
    let h = digest.strip_prefix("sha256:")?.to_ascii_lowercase();
    (h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
}

fn sha256(bytes: &[u8]) -> Option<String> {
    use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut out = [0u8; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, bytes, &mut out) }.is_ok().then(|| out.iter().map(|b| format!("{b:02x}")).collect())
}

fn old_path(me: &Path) -> PathBuf {
    me.with_extension("exe.old")
}

/// Where the helper saves the download: the helper's own folder, which is admin-only when
/// elevated (see bundle.rs), so another program can't plant a link there for an admin write.
pub fn download_path() -> Option<PathBuf> {
    Some(crate::bundle::helper_dir()?.join("update.download"))
}

/// Whether this copy may replace its own exe: never when elevated. An admin-owned folder can still
/// grant users write access, and anything running as the user could swap the file in between and
/// have it run as admin, so the releases page is offered instead.
// ponytail: elevated installs update by hand; add a DACL check + handle-verified launch if that matters
pub fn safe_to_replace() -> bool {
    !crate::admin::is_elevated()
}

/// Checks the download against the published SHA-256, puts it in place of this exe (Windows lets
/// a running exe be renamed) and starts it. The caller then quits.
pub fn install(r: &Release) -> Result<(), String> {
    if !safe_to_replace() {
        return Err("Update available · download it from the releases page".into());
    }
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let dl = download_path().ok_or("no download folder")?;
    // read once and check these exact bytes: nothing can swap the file between the check and the copy
    let bytes = std::fs::read(&dl).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&dl);
    verify(&bytes, &r.sha256)?;
    swap(&me, &bytes)?;
    // the new copy waits for this one to exit before taking over
    Command::new(&me).args(["--after-update", &std::process::id().to_string()]).spawn().map(|_| ()).map_err(|e| format!("Updated, but could not restart: {e}"))
}

/// The published checksum, and a one-file build (the app with its helper packed on), never a bare exe.
fn verify(bytes: &[u8], sha256_hex: &str) -> Result<(), String> {
    if sha256(bytes).as_deref() != Some(sha256_hex) {
        return Err("The update didn't match its published checksum, so it was not installed".into());
    }
    if crate::bundle::trailer(bytes).is_none() {
        return Err("The update isn't a one-file ResourcePill build, so it was not installed".into());
    }
    Ok(())
}

/// `me` → `me.exe.old`, `bytes` → `me`; on any failure `me` is left as it was.
fn swap(me: &Path, bytes: &[u8]) -> Result<(), String> {
    let new = me.with_extension("exe.new");
    let old = old_path(me);
    std::fs::write(&new, bytes).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(me, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(e.to_string());
    }
    if let Err(e) = std::fs::rename(&new, me) {
        let _ = std::fs::remove_file(&new);
        return match std::fs::rename(&old, me) {
            Ok(()) => Err(e.to_string()),
            Err(_) => Err(format!("Update failed ({e}); the app is saved as {}", old.display())),
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_and_digests() {
        let d = format!("sha256:{}", "AB".repeat(32));
        let r = super::parse(&format!("v0.2.0\thttps://github.com/x.exe\t{d}"), "0.1.0").unwrap();
        assert_eq!((r.tag.as_str(), r.sha256.as_str()), ("v0.2.0", "ab".repeat(32).as_str()));
        assert!(super::parse(&format!("v0.10.0\tu\t{d}"), "0.9.9").is_some(), "numeric, not text, order");
        for (tag, cur) in [("v0.1.0", "0.1.0"), ("v0.0.9", "0.1.0"), ("v1.0.0-beta", "0.1.0"), ("v1.0", "0.1.0"), ("latest", "0.1.0")] {
            assert!(super::parse(&format!("{tag}\tu\t{d}"), cur).is_none(), "{tag}");
        }
        for bad in ["", "sha512:ab", "sha256:abc", &format!("sha256:{}", "zz".repeat(32)), &format!("md5:{}", "ab".repeat(32))] {
            assert!(super::parse(&format!("v9.0.0\tu\t{bad}"), "0.1.0").is_none(), "{bad}");
        }
        assert_eq!(super::sha256(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        // only a one-file build with the right checksum is installed
        let bundled = [&b"MZ"[..], &2u64.to_le_bytes(), b"RPBUNDLE"].concat();
        assert!(super::verify(&bundled, &super::sha256(&bundled).unwrap()).is_ok());
        assert!(super::verify(&bundled, &"0".repeat(64)).is_err(), "wrong checksum");
        assert!(super::verify(b"MZ bare exe", &super::sha256(b"MZ bare exe").unwrap()).is_err(), "not a bundle");
    }

    /// The swap on real files: the new bytes end up at the exe path, the old ones beside it.
    #[test]
    fn swaps_files() {
        let dir = std::env::temp_dir().join(format!("rp-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let me = dir.join("ResourcePill.exe");
        std::fs::write(&me, b"old").unwrap();
        super::swap(&me, b"new").unwrap();
        assert_eq!(std::fs::read(&me).unwrap(), b"new");
        assert_eq!(std::fs::read(super::old_path(&me)).unwrap(), b"old");
        assert!(!me.with_extension("exe.new").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
