//! One-file app: `ResourcePill.exe --make-bundle <out.exe>` copies itself with the recording
//! helper and its DLLs packed onto the end. Started from anywhere, that one exe unpacks those
//! files once per version into %LOCALAPPDATA%\ResourcePill\bin and uses them from there.
//!
//! Layout: [the exe][files…][payload start: u64][MAGIC], each file = name length (u32), name,
//! data length (u64), data. Windows ignores bytes after an exe's image, so it still runs.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const MAGIC: &[u8; 8] = b"RPBUNDLE";

/// The packed files: the helper and the DLLs it needs, from next to this exe.
fn payload_files(dir: &Path) -> Vec<PathBuf> {
    let mut v = vec![dir.join("ResourcePill-Scribe.exe")];
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut dlls: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll"))).collect();
        dlls.sort();
        v.extend(dlls);
    }
    v
}

/// The exe's own bytes, without any bundle already on it.
fn exe_part(bytes: &[u8]) -> &[u8] {
    match trailer(bytes) {
        Some(start) => &bytes[..start as usize],
        None => bytes,
    }
}

/// Where the packed files start, if there are any.
pub(crate) fn trailer(bytes: &[u8]) -> Option<u64> {
    let n = bytes.len();
    (n >= 16 && &bytes[n - 8..] == MAGIC).then(|| u64::from_le_bytes(bytes[n - 16..n - 8].try_into().unwrap())).filter(|&s| (s as usize) <= n - 16)
}

/// Pack: this exe + the helper files next to it → `out`.
pub fn make(out: &Path) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = me.parent().ok_or("no folder")?.to_path_buf();
    let own = std::fs::read(&me).map_err(|e| e.to_string())?;
    let mut data = exe_part(&own).to_vec();
    let start = data.len() as u64;
    for f in payload_files(&dir) {
        let bytes = std::fs::read(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        let name = f.file_name().and_then(|n| n.to_str()).ok_or("bad file name")?;
        data.extend((name.len() as u32).to_le_bytes());
        data.extend(name.as_bytes());
        data.extend((bytes.len() as u64).to_le_bytes());
        data.extend(&bytes);
    }
    data.extend(start.to_le_bytes());
    data.extend(MAGIC);
    std::fs::write(out, data).map_err(|e| format!("{}: {e}", out.display()))
}

/// The packed files, read out of `bytes` (a bundled exe).
fn unpack_list(bytes: &[u8]) -> Option<Vec<(String, &[u8])>> {
    let mut at = trailer(bytes)? as usize;
    let end = bytes.len() - 16;
    let mut out = Vec::new();
    while at < end {
        let nlen = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
        at += 4;
        let name = std::str::from_utf8(bytes.get(at..at + nlen)?).ok()?.to_string();
        // plain file names only: nothing may land outside the unpack folder
        if name.is_empty() || name.contains(['/', '\\', ':']) || name.contains("..") {
            return None;
        }
        at += nlen;
        let dlen = u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?) as usize;
        at += 8;
        out.push((name, bytes.get(at..at + dlen)?));
        at += dlen;
    }
    Some(out)
}

static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Where the helper files are: next to the exe (a normal folder install), else unpacked from
/// this exe (once per version, into %LOCALAPPDATA%\ResourcePill\bin\<version>).
pub fn helper_dir() -> Option<PathBuf> {
    DIR.get_or_init(|| {
        let me = std::env::current_exe().ok()?;
        let beside = me.parent()?.to_path_buf();
        if beside.join("ResourcePill-Scribe.exe").exists() {
            return Some(beside);
        }
        // only the last 16 bytes are read to see whether anything is packed
        let mut f = std::fs::File::open(&me).ok()?;
        let len = f.metadata().ok()?.len();
        if len < 16 {
            return None;
        }
        f.seek(SeekFrom::End(-16)).ok()?;
        let mut tail = [0u8; 16];
        f.read_exact(&mut tail).ok()?;
        if &tail[8..] != MAGIC {
            return None;
        }
        // a version key from the exe's size and date: a new build unpacks into a new folder
        let stamp = std::fs::metadata(&me).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        let version = format!("{len}-{stamp}");
        // Elevated (admin mode), the helper must not run from a folder this user can write to, or
        // anything running as them could swap it and get admin rights: use an admin-only folder.
        let dir = if crate::admin::is_elevated() {
            admin_folder(&format!("ResourcePill-bin-{version}"))?
        } else {
            PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("ResourcePill").join("bin").join(&version)
        };
        if !dir.join(".complete").exists() {
            let bytes = std::fs::read(&me).ok()?;
            std::fs::create_dir_all(&dir).ok()?;
            for (name, data) in unpack_list(&bytes)? {
                let mut out = std::fs::File::create(dir.join(&name)).ok()?;
                out.write_all(data).ok()?;
            }
            std::fs::write(dir.join(".complete"), b"").ok()?;
        }
        Some(dir)
    })
    .clone()
}

/// `C:\ProgramData\<name>` (the helper: `ResourcePill-bin-<version>`; admin mode's own exe:
/// `ResourcePill-app`), created here so only Administrators and SYSTEM can change it (users may
/// read and run). If it already exists it is used only when it's a real folder (not a link) owned
/// by Administrators or SYSTEM, which a normal user can't fake; otherwise None rather than running
/// something planted.
pub(crate) fn admin_folder(name: &str) -> Option<PathBuf> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::*;
    use windows::Win32::Security::*;
    use windows::Win32::Storage::FileSystem::CreateDirectoryW;
    use windows::Win32::UI::Shell::{FOLDERID_ProgramData, KNOWN_FOLDER_FLAG, SHGetKnownFolderPath};
    use windows::core::{HSTRING, w};

    // from the registry, not %ProgramData% (a user can set their own environment variables)
    let root = unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_ProgramData, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let s = p.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
        PathBuf::from(s?)
    };
    let dir = root.join(name);
    let wide = HSTRING::from(dir.as_os_str());
    unsafe {
        // owner Administrators; protected DACL: SYSTEM + Administrators full, Users read/run
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(w!("O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)"), SDDL_REVISION_1, &mut sd, None).ok()?;
        let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() };
        let mut ok = CreateDirectoryW(&wide, Some(&sa)).is_ok() || admin_owned_dir(&dir);
        // already there but not ours to trust (made by a normal user, or a link): we're elevated, so
        // move it out of the way (renaming a link moves the link, never what it points to) and make
        // a clean one, rather than giving up and running from somewhere a user can change
        if !ok && crate::admin::is_elevated() {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            if std::fs::rename(&dir, root.join(format!("{name}.untrusted-{stamp}"))).is_ok() {
                ok = CreateDirectoryW(&wide, Some(&sa)).is_ok(); // lost a race again: give up (None)
            }
        }
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        ok.then_some(dir)
    }
}

/// A real folder (not a link) owned by Administrators or SYSTEM, which a normal user can't fake.
pub(crate) fn admin_owned_dir(dir: &Path) -> bool {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::*;
    use windows::Win32::Security::*;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    use std::os::windows::fs::MetadataExt;

    let Ok(meta) = std::fs::symlink_metadata(dir) else { return false };
    if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return false;
    }
    unsafe {
        let mut owner = PSID::default();
        let mut desc = PSECURITY_DESCRIPTOR::default();
        if GetNamedSecurityInfoW(&windows::core::HSTRING::from(dir.as_os_str()), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut owner), None, None, None, &mut desc).is_err() {
            return false;
        }
        let trusted = IsWellKnownSid(owner, WinBuiltinAdministratorsSid).as_bool() || IsWellKnownSid(owner, WinLocalSystemSid).as_bool();
        let _ = LocalFree(Some(HLOCAL(desc.0)));
        trusted
    }
}

#[cfg(test)]
mod tests {
    /// Run without admin rights: a folder this user made in ProgramData is not trusted.
    #[test]
    #[ignore]
    fn live_planted_folder_refused() {
        let dir = std::path::PathBuf::from(std::env::var_os("ProgramData").unwrap()).join("ResourcePill-bin-test-planted");
        std::fs::create_dir_all(&dir).unwrap();
        let got = super::admin_folder("ResourcePill-bin-test-planted");
        let _ = std::fs::remove_dir(&dir);
        assert!(got.is_none());
    }

    #[test]
    fn packs_and_unpacks() {
        // a fake exe with two files on the end, built the same way `make` does
        let mut data = b"MZ pretend exe".to_vec();
        let start = data.len() as u64;
        for (name, body) in [("ResourcePill-Scribe.exe", &b"helper"[..]), ("onnxruntime.dll", &b"dll bytes"[..])] {
            data.extend((name.len() as u32).to_le_bytes());
            data.extend(name.as_bytes());
            data.extend((body.len() as u64).to_le_bytes());
            data.extend(body);
        }
        data.extend(start.to_le_bytes());
        data.extend(super::MAGIC);
        let files = super::unpack_list(&data).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!((files[1].0.as_str(), files[1].1), ("onnxruntime.dll", &b"dll bytes"[..]));
        assert_eq!(super::exe_part(&data), b"MZ pretend exe", "re-bundling starts from the bare exe");
        assert!(super::unpack_list(b"MZ no bundle here").is_none());
        // a name that would escape the folder is refused
        let mut bad = b"MZ".to_vec();
        let name = b"..\\evil.dll";
        bad.extend((name.len() as u32).to_le_bytes());
        bad.extend(name);
        bad.extend(1u64.to_le_bytes());
        bad.push(0);
        bad.extend(2u64.to_le_bytes());
        bad.extend(super::MAGIC);
        assert!(super::unpack_list(&bad).is_none());
    }
}
