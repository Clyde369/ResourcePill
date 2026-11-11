//! Admin mode: CPU temperature (AMD Ryzen Master SDK), SATA temperatures (SMART), and the
//! elevated login task that makes it start without UAC prompts.

use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::HSTRING;

const TASK: &str = "ResourcePill";
const NO_WINDOW: u32 = 0x0800_0000;

pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut e = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TokenElevation, Some((&mut e as *mut TOKEN_ELEVATION).cast()), size_of::<TOKEN_ELEVATION>() as u32, &mut len).is_ok();
        let _ = CloseHandle(token);
        ok && e.TokenIsElevated != 0
    }
}

// ---------- CPU temperature (AMD Ryzen Master SDK) ----------

/// Latest CPU temperature in °C as f64 bits; NaN = unavailable.
static CPU_TEMP: AtomicU64 = AtomicU64::new(0x7FF8_0000_0000_0000);

pub fn cpu_temp() -> Option<f64> {
    let v = f64::from_bits(CPU_TEMP.load(Ordering::Relaxed));
    v.is_finite().then_some(v)
}

/// Reads the CPU temperature once a second on its own thread, so a stalled driver call can
/// never freeze the rest of the monitor. Does nothing unless elevated (the SDK requires it).
pub fn spawn_cpu_temp() {
    if !is_elevated() {
        return;
    }
    std::thread::spawn(|| {
        if let Err(why) = sdk::open() {
            log(&format!("CPU temperature unavailable: {why}"));
            return;
        }
        let mut logged = false;
        loop {
            let t = sdk::temperature();
            if !logged {
                log(&format!("CPU temperature via Ryzen Master SDK: first reading {t:?}"));
                logged = true;
            }
            CPU_TEMP.store(t.ok().filter(|t| (1.0..125.0).contains(t)).unwrap_or(f64::NAN).to_bits(), Ordering::Relaxed);
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

/// Bridge to src/ryzen_shim.cpp (compiled against AMD's headers by build.rs).
#[cfg(ryzen_sdk)]
mod sdk {
    unsafe extern "C" {
        fn rp_sdk_open(platform_dll: *const u16) -> i32;
        fn rp_sdk_temperature(out: *mut f64) -> i32;
    }

    pub fn open() -> Result<(), String> {
        let dir = std::env::var("AMDRMSDKPATH").unwrap_or_else(|_| r"C:\Program Files\AMD\RyzenMasterSDK\".into());
        let dll = std::path::Path::new(&dir).join("bin").join("Platform.dll");
        let w: Vec<u16> = dll.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        match unsafe { rp_sdk_open(w.as_ptr()) } {
            0 => Ok(()),
            -1 => Err(format!("cannot load {}", dll.display())),
            -3 => Err("SDK platform init failed".into()),
            rc => Err(format!("SDK open failed ({rc})")),
        }
    }

    pub fn temperature() -> Result<f64, i32> {
        let mut t = 0.0;
        match unsafe { rp_sdk_temperature(&mut t) } {
            0 => Ok(t),
            rc => Err(rc),
        }
    }
}

#[cfg(not(ryzen_sdk))]
mod sdk {
    pub fn open() -> Result<(), String> {
        Err("built without the Ryzen Master SDK".into())
    }
    pub fn temperature() -> Result<f64, i32> {
        Err(-1)
    }
}

/// One-line status in %APPDATA%\ResourcePill\admin.log (overwritten each start).
fn log(line: &str) {
    if let Some(dir) = std::env::var_os("APPDATA").map(|a| std::path::PathBuf::from(a).join("ResourcePill")) {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("admin.log"), format!("{line}\n"));
    }
}

// ---------- SATA temperature (SMART attribute 194/190) ----------

const SMART_RCV_DRIVE_DATA: u32 = 0x0007_C088;

/// SMART temperature for PhysicalDrive{n}; needs admin (read/write handle).
pub fn smart_temp(n: u32) -> Option<f64> {
    unsafe {
        let h = CreateFileW(
            &HSTRING::from(format!(r"\\.\PhysicalDrive{n}")),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .ok()?;
        // SENDCMDINPARAMS (packed, 33 bytes): size, IDEREGS, drive number, reserved
        let mut input = [0u8; 33];
        input[0..4].copy_from_slice(&512u32.to_le_bytes());
        input[4..12].copy_from_slice(&[0xD0, 1, 1, 0x4F, 0xC2, 0xA0 | (((n & 1) as u8) << 4), 0xB0, 0]); // SMART READ DATA
        input[12] = n as u8;
        let mut out = [0u8; 16 + 512];
        let mut ret = 0u32;
        let ok = DeviceIoControl(h, SMART_RCV_DRIVE_DATA, Some(input.as_ptr().cast()), 32, Some(out.as_mut_ptr().cast()), out.len() as u32, Some(&mut ret), None);
        let _ = CloseHandle(h);
        ok.ok()?;
        let data = &out[16..];
        // 30 attributes of 12 bytes from offset 2: id, flags(2), value, worst, raw(6), reserved
        let attr = |id: u8| data[2..2 + 30 * 12].chunks(12).find(|a| a[0] == id).map(|a| a[5] as f64);
        attr(194).or_else(|| attr(190)).filter(|t| (1.0..100.0).contains(t))
    }
}

// ---------- elevated login task ----------

fn schtasks(args: &[&str]) -> bool {
    std::process::Command::new("schtasks.exe").args(args).creation_flags(NO_WINDOW).output().is_ok_and(|o| o.status.success())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Registers "start at login with highest privileges" (no time limit, runs on battery). Needs admin.
pub fn install_task() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let user = format!("{}\\{}", std::env::var("USERDOMAIN").unwrap_or_default(), std::env::var("USERNAME").unwrap_or_default());
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>ResourcePill in admin mode (CPU and drive temperatures)</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>
    <Priority>6</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{}</Command><Arguments>--from-task</Arguments></Exec></Actions>
</Task>"#,
        xml_escape(&exe.display().to_string()),
        user = xml_escape(&user)
    );
    let path = std::env::temp_dir().join("ResourcePill-task.xml");
    let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain(xml.encode_utf16().flat_map(|c| c.to_le_bytes())).collect();
    if std::fs::write(&path, utf16).is_err() {
        return false;
    }
    let ok = schtasks(&["/Create", "/TN", TASK, "/XML", &path.display().to_string(), "/F"]);
    let _ = std::fs::remove_file(path);
    ok
}

pub fn remove_task() -> bool {
    schtasks(&["/Delete", "/TN", TASK, "/F"])
}

/// Starts the elevated copy through the task (no UAC prompt). Works from a non-admin process.
pub fn run_task() -> bool {
    schtasks(&["/Run", "/TN", TASK])
}

#[cfg(test)]
mod tests {
    #[test]
    fn probe() {
        println!("elevated: {}", super::is_elevated());
        if super::is_elevated() {
            super::spawn_cpu_temp();
            std::thread::sleep(std::time::Duration::from_secs(3));
            println!("cpu temp: {:?}", super::cpu_temp());
            for n in 0..6 {
                println!("SMART PhysicalDrive{n}: {:?}", super::smart_temp(n));
            }
        }
        assert_eq!(super::xml_escape("a&<\">"), "a&amp;&lt;&quot;&gt;");
    }
}
