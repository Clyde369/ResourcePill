//! Background sampler: system counters every second, per-app breakdown only for the open panel.
//! Sources: PDH busy time, the kernel process snapshot (same as Task Manager),
//! GPU Engine counters, per-disk active time, and physical network adapters.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfEntry2, GetIfTable2, MIB_IF_ROW2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::System::Performance::*;
use windows::Win32::System::SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::core::HSTRING;

const FMT: PDH_FMT = PDH_FMT(PDH_FMT_DOUBLE.0 | 0x8000); // double, NOCAP100

pub const CPU: usize = 0;
pub const RAM: usize = 1;
pub const GPU: usize = 2;
pub const DISK: usize = 3;
pub const NET: usize = 4;
pub const NONE: usize = usize::MAX;

#[derive(Clone, Default)]
pub struct Top {
    pub name: String,
    pub value: String,
    pub frac: f32,
}

#[derive(Clone, Default)]
pub struct Snap {
    pub frac: [f32; 5],    // ring fill 0..1
    pub text: [String; 5], // pill value
    pub detail: [String; 5],
    pub down: String,
    pub up: String,
    pub top_for: usize,
    pub top: Vec<Top>,
    pub ready: bool,
    pub net_rate: f64, // bytes/s, down + up
    pub disk_label: String, // busiest drive, e.g. "DISK C:"
    pub vram_used: f64, // bytes of dedicated GPU memory in use (all processes)
    pub temp: [Option<f64>; 5], // °C per metric slot: CPU, GPU, busiest DISK (RAM/NET unused)
}

pub struct Shared {
    pub snap: Mutex<Snap>,
    pub selected: AtomicUsize,
    pub visible: AtomicBool,
    pub interval_ms: AtomicU32,
    /// 0 = raw readings, 1 = light, 2 = strong smoothing of CPU/GPU/disk %.
    pub smoothing: AtomicU32,
}

/// Starts the sampler thread. Send `()` on the returned channel to sample immediately.
pub fn spawn(shared: Arc<Shared>, notify: impl Fn() + Send + 'static) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut s: Option<Sampler> = None;
        loop {
            if shared.visible.load(Ordering::Relaxed) {
                let sm = s.get_or_insert_with(Sampler::new);
                let sel = shared.selected.load(Ordering::Relaxed);
                // A failure inside a counter/OS call must never take the app down: drop and rebuild.
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sm.sample(sel, shared.smoothing.load(Ordering::Relaxed)))) {
                    Ok(snap) => {
                        *shared.snap.lock().unwrap_or_else(|e| e.into_inner()) = snap;
                        notify();
                    }
                    Err(_) => s = None,
                }
            }
            if let Err(mpsc::RecvTimeoutError::Disconnected) = rx.recv_timeout(Duration::from_millis(shared.interval_ms.load(Ordering::Relaxed).clamp(250, 10_000) as u64)) {
                break;
            }
            while rx.try_recv().is_ok() {} // coalesce bursts of wake-ups
        }
    });
    tx
}

// ---------- PDH ----------

struct Pdh {
    q: PDH_HQUERY,
    cpu: Option<PDH_HCOUNTER>,
    perf: Option<PDH_HCOUNTER>,
    freq: Option<PDH_HCOUNTER>,
    gpu: Option<PDH_HCOUNTER>,
    vram: Option<PDH_HCOUNTER>,
    disk_idle: Option<PDH_HCOUNTER>,
    disk_r: Option<PDH_HCOUNTER>,
    disk_w: Option<PDH_HCOUNTER>,
}

impl Pdh {
    fn new() -> Self {
        let mut q = PDH_HQUERY::default();
        unsafe { PdhOpenQueryW(None, 0, &mut q) };
        let add = |path: &str| -> Option<PDH_HCOUNTER> {
            let mut c = PDH_HCOUNTER::default();
            // English names so it works on any Windows display language.
            (unsafe { PdhAddEnglishCounterW(q, &HSTRING::from(path), 0, &mut c) } == 0).then_some(c)
        };
        // Busy time, not "% Processor Utility": utility is scaled by turbo (x1.18 here) and pins at 100%
        // from ~85% real load; busy time also adds up with the per-app CPU figures.
        let cpu = add(r"\Processor Information(_Total)\% Processor Time")
            .or_else(|| add(r"\Processor(_Total)\% Processor Time"));
        let p = Pdh {
            q,
            cpu,
            perf: add(r"\Processor Information(_Total)\% Processor Performance"),
            freq: add(r"\Processor Information(_Total)\Processor Frequency"),
            gpu: add(r"\GPU Engine(*)\Utilization Percentage"),
            vram: add(r"\GPU Adapter Memory(*)\Dedicated Usage"),
            disk_idle: add(r"\PhysicalDisk(*)\% Idle Time"),
            disk_r: add(r"\PhysicalDisk(*)\Disk Read Bytes/sec"),
            disk_w: add(r"\PhysicalDisk(*)\Disk Write Bytes/sec"),
        };
        unsafe { PdhCollectQueryData(p.q) };
        p
    }

    fn value(&self, c: Option<PDH_HCOUNTER>) -> f64 {
        let Some(c) = c else { return 0.0 };
        let mut v = PDH_FMT_COUNTERVALUE::default();
        let r = unsafe { PdhGetFormattedCounterValue(c, FMT, None, &mut v) };
        if r != 0 || v.CStatus > 1 {
            return 0.0;
        }
        unsafe { v.Anonymous.doubleValue }
    }

    fn array(&self, c: Option<PDH_HCOUNTER>) -> Vec<(String, f64)> {
        let Some(c) = c else { return vec![] };
        unsafe {
            let (mut size, mut count) = (0u32, 0u32);
            let r = PdhGetFormattedCounterArrayW(c, FMT, &mut size, &mut count, None);
            if r != PDH_MORE_DATA || size == 0 {
                return vec![];
            }
            let mut buf = vec![0u64; size as usize / 8 + 1]; // u64 for alignment
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(c, FMT, &mut size, &mut count, Some(items)) != 0 {
                return vec![];
            }
            std::slice::from_raw_parts(items, count as usize)
                .iter()
                .filter(|i| i.FmtValue.CStatus <= 1)
                .filter_map(|i| Some((i.szName.to_string().ok()?, i.FmtValue.Anonymous.doubleValue)))
                .collect()
        }
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.q) };
    }
}

// ---------- kernel process snapshot (same source as Task Manager; sees protected processes) ----------

#[repr(C)]
struct UnicodeString {
    len: u16,
    max: u16,
    buf: *const u16,
}

/// SYSTEM_PROCESS_INFORMATION (stable layout since Windows 7).
#[repr(C)]
struct SysProc {
    next: u32,
    threads: u32,
    private_ws: i64,
    _hard_faults: u32,
    _threads_hw: u32,
    _cycles: u64,
    _create: i64,
    user: i64,
    kernel: i64,
    name: UnicodeString,
    _prio: i32,
    pid: usize,
    _parent: usize,
    _handles: u32,
    _session: u32,
    _key: usize,
    _peak_vsize: usize,
    _vsize: usize,
    _faults: u32,
    _peak_ws: usize,
    _ws: usize,
    _q: [usize; 4],
    _pagefile: usize,
    _peak_pagefile: usize,
    _private_pages: usize,
    _ops: [i64; 3],
    read: i64,
    write: i64,
    _other: i64,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQuerySystemInformation(class: u32, buf: *mut core::ffi::c_void, len: u32, ret: *mut u32) -> i32;
}

struct Proc {
    name: String,
    cpu_time: i64, // 100 ns units
    private_ws: u64,
    io: u64,
}

fn processes(buf: &mut Vec<u64>) -> HashMap<u32, Proc> {
    let mut out = HashMap::new();
    unsafe {
        loop {
            let mut ret = 0u32;
            let st = NtQuerySystemInformation(5, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut ret);
            if st == 0xC000_0004u32 as i32 && buf.len() < 64 << 20 {
                // STATUS_INFO_LENGTH_MISMATCH: grow with headroom for new processes
                buf.resize((ret as usize / 8 + 1) * 2, 0);
                continue;
            }
            if st != 0 {
                return out;
            }
            break;
        }
        let base = buf.as_ptr() as *const u8;
        let end = buf.len() * 8;
        let mut off = 0usize;
        loop {
            if off + size_of::<SysProc>() > end {
                break;
            }
            let p = &*(base.add(off) as *const SysProc);
            let name = if p.name.buf.is_null() || p.pid == 0 {
                String::new()
            } else {
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(p.name.buf, p.name.len as usize / 2));
                match s.len().checked_sub(4) {
                    Some(n) if s[n..].eq_ignore_ascii_case(".exe") => s[..n].to_string(),
                    _ => s,
                }
            };
            out.insert(
                p.pid as u32,
                Proc {
                    name,
                    cpu_time: p.user + p.kernel,
                    private_ws: p.private_ws.max(0) as u64,
                    io: (p.read + p.write).max(0) as u64,
                },
            );
            if p.next == 0 {
                break;
            }
            off += p.next as usize;
        }
    }
    out
}

// ---------- sampler ----------

struct Sampler {
    pdh: Pdh,
    ncpu: f64,
    last: Instant,
    buf: Vec<u64>,
    prev: HashMap<u32, Proc>,
    prev_at: Option<Instant>,
    net_prev: HashMap<u64, (u64, u64)>,
    ifs: Vec<(u64, String)>,
    ifs_at: Option<Instant>,
    ema: [Option<f64>; 3],
    smooth_k: f64,
    temp_drives: Vec<u32>, // drives that report a temperature
    temp_probe_at: Option<Instant>,
    temp_read_at: Option<Instant>,
    drive_temps: HashMap<u32, f64>,
    elevated: bool,
}

impl Sampler {
    fn new() -> Self {
        Sampler {
            pdh: Pdh::new(),
            ncpu: std::thread::available_parallelism().map_or(1, |n| n.get()) as f64,
            last: Instant::now(),
            buf: vec![0; 64 * 1024],
            prev: HashMap::new(),
            prev_at: None,
            net_prev: HashMap::new(),
            ifs: Vec::new(),
            ifs_at: None,
            ema: [None; 3],
            smooth_k: 1.0,
            temp_drives: Vec::new(),
            temp_probe_at: None,
            temp_read_at: None,
            drive_temps: HashMap::new(),
            elevated: crate::admin::is_elevated(),
        }
    }

    /// Current processes plus seconds since the previous snapshot (CPU % and I/O are deltas).
    fn procs(&mut self) -> (HashMap<u32, Proc>, HashMap<u32, Proc>, f64) {
        if self.prev_at.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) {
            self.prev = processes(&mut self.buf);
            self.prev_at = Some(Instant::now());
            std::thread::sleep(Duration::from_millis(300));
        }
        let dt = self.prev_at.map_or(1.0, |t| t.elapsed().as_secs_f64()).max(0.05);
        let now = processes(&mut self.buf);
        self.prev_at = Some(Instant::now());
        let prev = std::mem::take(&mut self.prev);
        (now, prev, dt)
    }

    /// Drive temperature: NVMe sensor query (no admin), else SMART when running as admin (SATA).
    fn drive_temp(&self, n: u32) -> Option<f64> {
        crate::temps::drive(n).or_else(|| if self.elevated { crate::admin::smart_temp(n) } else { None })
    }

    /// Exponential smoothing of a percentage (k = 1 means raw).
    fn smooth(&mut self, slot: usize, v: f64) -> f64 {
        let s = self.ema[slot].map_or(v, |p| p + (v - p) * self.smooth_k);
        self.ema[slot] = Some(s);
        s
    }

    fn sample(&mut self, sel: usize, smoothing: u32) -> Snap {
        self.smooth_k = [1.0, 0.5, 0.3][(smoothing as usize).min(2)];
        let dt = self.last.elapsed().as_secs_f64().max(0.05);
        self.last = Instant::now();
        unsafe { PdhCollectQueryData(self.pdh.q) };
        let mut s = Snap { top_for: sel, ready: true, ..Default::default() };

        // CPU busy %, plus the live clock speed.
        let cpu = self.pdh.value(self.pdh.cpu).clamp(0.0, 100.0);
        let cpu = self.smooth(0, cpu);
        let ghz = self.pdh.value(self.pdh.freq) * self.pdh.value(self.pdh.perf) / 100.0 / 1000.0;
        s.frac[CPU] = cpu as f32 / 100.0;
        s.text[CPU] = format!("{cpu:.0}%");
        s.temp[CPU] = crate::admin::cpu_temp();
        let temp = s.temp[CPU].map_or(String::new(), |t| format!("{t:.0} °C · "));
        s.detail[CPU] = if ghz > 0.0 { format!("{temp}{ghz:.2} GHz · up {}", uptime()) } else { format!("{temp}up {}", uptime()) };

        // RAM: in use = total - available (Task Manager's definition), GiB like Task Manager.
        let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        let _ = unsafe { GlobalMemoryStatusEx(&mut m) };
        let total = m.ullTotalPhys.max(1);
        let used = total.saturating_sub(m.ullAvailPhys);
        s.frac[RAM] = used as f32 / total as f32;
        s.text[RAM] = format!("{:.0}%", s.frac[RAM] * 100.0);
        s.detail[RAM] = format!("{:.1} / {:.1} GB", gib(used), gib(total));

        // GPU: busiest engine (Task Manager's definition), summed over processes.
        let engines = self.pdh.array(self.pdh.gpu);
        let mut per_engine: HashMap<&str, f64> = HashMap::new();
        let mut per_pid: HashMap<u32, HashMap<&str, f64>> = HashMap::new();
        for (name, v) in &engines {
            // "pid_1234_luid_0x..._phys_0_eng_3_engtype_3D"
            let Some((pid, eng)) = name.strip_prefix("pid_").and_then(|r| r.split_once('_')) else { continue };
            let Ok(pid) = pid.parse::<u32>() else { continue };
            *per_engine.entry(eng).or_default() += v;
            *per_pid.entry(pid).or_default().entry(eng).or_default() += v;
        }
        let gpu = per_engine.values().fold(0.0f64, |a, &b| a.max(b)).clamp(0.0, 100.0);
        let gpu = self.smooth(1, gpu);
        let vram: f64 = self.pdh.array(self.pdh.vram).iter().map(|(_, v)| v).sum();
        s.vram_used = vram;
        s.frac[GPU] = gpu as f32 / 100.0;
        s.text[GPU] = format!("{gpu:.0}%");
        s.temp[GPU] = crate::temps::gpu();
        s.detail[GPU] = match s.temp[GPU] {
            Some(t) => format!("{t:.0} °C · VRAM {:.1} GB", gib(vram as u64)),
            None => format!("VRAM {:.1} GB", gib(vram as u64)),
        };

        // Disks: one entry per physical drive, like Task Manager. The pill shows the busiest one.
        let reads: HashMap<String, f64> = self.pdh.array(self.pdh.disk_r).into_iter().collect();
        let writes: HashMap<String, f64> = self.pdh.array(self.pdh.disk_w).into_iter().collect();
        let mut disks: Vec<Drive> = self
            .pdh
            .array(self.pdh.disk_idle)
            .into_iter()
            .filter(|(n, _)| n != "_Total")
            .map(|(inst, idle)| Drive {
                active: (100.0 - idle).clamp(0.0, 100.0),
                read: reads.get(&inst).copied().unwrap_or(0.0),
                write: writes.get(&inst).copied().unwrap_or(0.0),
                temp: None,
                inst,
            })
            .collect();
        disks.sort_by_key(|d| d.index());
        // Which drives expose a sensor rarely changes: re-probe every 60 s, read the known ones every tick.
        if self.temp_probe_at.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
            self.temp_drives = disks.iter().map(|d| d.index()).filter(|&n| self.drive_temp(n).is_some()).collect();
            self.temp_probe_at = Some(Instant::now());
        }
        // drive sensors (SMART on SATA) are read every 10 s; temperatures change slowly
        if self.temp_read_at.is_none_or(|t| t.elapsed() > Duration::from_secs(10)) {
            self.drive_temps = self.temp_drives.iter().filter_map(|&n| Some((n, self.drive_temp(n)?))).collect();
            self.temp_read_at = Some(Instant::now());
        }
        for d in &mut disks {
            d.temp = self.drive_temps.get(&d.index()).copied();
        }
        let (dr, dw) = disks.iter().fold((0.0, 0.0), |(r, w), d| (r + d.read, w + d.write));
        let busiest = disks.iter().max_by(|a, b| a.active.total_cmp(&b.active));
        let active = busiest.map_or(0.0, |d| d.active);
        let active = self.smooth(2, active);
        s.frac[DISK] = active as f32 / 100.0;
        s.text[DISK] = format!("{active:.0}%");
        s.disk_label = busiest.map_or_else(|| "DISK".into(), |d| d.short());
        s.temp[DISK] = busiest.and_then(|d| d.temp);
        s.detail[DISK] = format!("R {} · W {}", rate(dr), rate(dw));

        // Network: physical adapters only (no loopback / virtual / filter duplicates), in bits like Task Manager.
        let mut ifs = Vec::new();
        let (mut down, mut up) = (0.0, 0.0);
        let mut seen = HashMap::new();
        // Full adapter enumeration is slow (~7 ms); redo it only every 30 s.
        if self.ifs_at.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
            self.ifs = adapters();
            self.ifs_at = Some(Instant::now());
        }
        let live: Vec<_> = self.ifs.iter().filter_map(|(l, a)| Some((*l, a.clone(), counters(*l)?))).collect();
        for (luid, alias, (rx, tx)) in live {
            let (prx, ptx) = self.net_prev.get(&luid).copied().unwrap_or((rx, tx));
            let (r, t) = (rx.saturating_sub(prx) as f64 / dt, tx.saturating_sub(ptx) as f64 / dt);
            down += r;
            up += t;
            ifs.push((alias, r, t));
            seen.insert(luid, (rx, tx));
        }
        self.net_prev = seen;
        s.text[NET] = bits(down);
        s.down = bits(down);
        s.net_rate = down + up;
        s.up = bits(up);
        s.detail[NET] = format!("R {} · S {}", bits(down), bits(up));

        // Per-app breakdown, only for the open panel.
        let mut by_app: HashMap<String, f64> = HashMap::new();
        if matches!(sel, CPU | RAM | GPU | DISK) {
            let (now, prev, pdt) = self.procs();
            for (&pid, p) in &now {
                if pid == 0 {
                    continue; // System Idle Process
                }
                let old = prev.get(&pid);
                let v = match sel {
                    CPU => old.map_or(0.0, |o| (p.cpu_time - o.cpu_time).max(0) as f64 / (pdt * 1e7 * self.ncpu) * 100.0),
                    RAM => p.private_ws as f64,
                    DISK => old.map_or(0.0, |o| p.io.saturating_sub(o.io) as f64 / pdt),
                    _ => per_pid.get(&pid).map_or(0.0, |e| e.values().fold(0.0f64, |a, &b| a.max(b))),
                };
                let name = if p.name.is_empty() { format!("PID {pid}") } else { p.name.clone() };
                *by_app.entry(name).or_default() += v;
            }
            self.prev = now;
        }
        s.top = match sel {
            NET => {
                ifs.sort_by(|a, b| (b.1 + b.2).total_cmp(&(a.1 + a.2)));
                let max = ifs.first().map_or(1.0, |i| (i.1 + i.2).max(1.0));
                ifs.into_iter()
                    .take(3)
                    .map(|(n, r, t)| Top { name: n, value: format!("R {}  S {}", bits(r), bits(t)), frac: ((r + t) / max) as f32 })
                    .collect()
            }
            _ => {
                // Disk panel: every drive first, then the busiest apps under a section header.
                let mut rows: Vec<Top> = Vec::new();
                if sel == DISK {
                    rows.extend(disks.iter().map(|d| Top {
                        name: d.long(),
                        value: match d.temp {
                            Some(t) => format!("{:.0}%  ·  {}  ·  {t:.0} °C", d.active, rate(d.read + d.write)),
                            None => format!("{:.0}%  ·  {}", d.active, rate(d.read + d.write)),
                        },
                        frac: (d.active / 100.0) as f32,
                    }));
                    rows.push(Top { name: "Top apps by disk I/O".into(), value: String::new(), frac: -1.0 });
                }
                let mut v: Vec<_> = by_app.into_iter().filter(|(_, v)| *v > 0.0).collect();
                v.sort_by(|a, b| b.1.total_cmp(&a.1));
                let top_max = v.first().map_or(1.0, |t| t.1.max(1.0));
                rows.extend(v.into_iter()
                    .take(3)
                    .map(|(name, x)| {
                        let (value, frac) = match sel {
                            RAM => (bytes(x), x / total as f64),
                            // process I/O also counts network/device bytes, so scale against the busiest app
                            DISK => (rate(x), x / top_max),
                            _ => (format!("{:.1}%", x.min(100.0)), x / 100.0),
                        };
                        Top { name, value, frac: frac.clamp(0.0, 1.0) as f32 }
                    }));
                rows
            }
        };
        s
    }
}

/// One physical disk. PDH instance names look like "3 C:" or "0 D: H:".
struct Drive {
    inst: String,
    active: f64,
    read: f64,
    write: f64,
    temp: Option<f64>,
}

impl Drive {
    fn index(&self) -> u32 {
        self.inst.split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(u32::MAX)
    }
    fn letters(&self) -> &str {
        self.inst.split_once(' ').map_or("", |(_, l)| l.trim())
    }
    /// "Disk 3 (C:)", as in Task Manager.
    fn long(&self) -> String {
        match (self.inst.split(' ').next(), self.letters()) {
            (Some(n), "") => format!("Disk {n}"),
            (Some(n), l) => format!("Disk {n} ({l})"),
            _ => self.inst.clone(),
        }
    }
    /// Pill label: first drive letter ("C:"), or "DISK 2" for a disk without letters.
    fn short(&self) -> String {
        match self.letters().split(' ').next() {
            Some(l) if !l.is_empty() => format!("DISK {l}"),
            _ => format!("DISK {}", self.index()),
        }
    }
}

/// (luid, alias) of physical adapters that are up.
fn adapters() -> Vec<(u64, String)> {
    let mut out = Vec::new();
    unsafe {
        let mut t: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut t).is_err() || t.is_null() {
            return out;
        }
        let rows = std::slice::from_raw_parts((*t).Table.as_ptr(), (*t).NumEntries as usize);
        for r in rows {
            let f = r.InterfaceAndOperStatusFlags._bitfield;
            let hardware = f & 1 != 0;
            let filter = f & 2 != 0;
            if !hardware || filter || r.OperStatus != IfOperStatusUp {
                continue;
            }
            let len = r.Alias.iter().position(|&c| c == 0).unwrap_or(r.Alias.len());
            out.push((r.InterfaceLuid.Value, String::from_utf16_lossy(&r.Alias[..len])));
        }
        FreeMibTable(t as *const _);
    }
    out
}

/// (rx, tx) byte counters for one adapter; None if it is gone or down.
fn counters(luid: u64) -> Option<(u64, u64)> {
    let mut row = MIB_IF_ROW2::default();
    row.InterfaceLuid.Value = luid;
    unsafe { GetIfEntry2(&mut row) }.ok().ok()?;
    (row.OperStatus == IfOperStatusUp).then_some((row.InOctets, row.OutOctets))
}

fn uptime() -> String {
    let m = unsafe { GetTickCount64() } / 60_000;
    let (d, h, m) = (m / 1440, m / 60 % 24, m % 60);
    if d > 0 { format!("{d}d {h}h") } else { format!("{h}h {m}m") }
}

fn gib(b: u64) -> f64 {
    b as f64 / (1u64 << 30) as f64
}

pub fn bytes(b: f64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut i) = (b.max(0.0), 0);
    while v >= 1000.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    match (i, v < 10.0) {
        (0, _) => format!("{v:.0} B"),
        (_, true) => format!("{v:.1} {}", U[i]),
        _ => format!("{v:.0} {}", U[i]),
    }
}

pub fn rate(b: f64) -> String {
    format!("{}/s", bytes(b))
}

/// Bytes/s → Task Manager style bit rate ("8.0 Kbps", "12 Mbps").
pub fn bits(bytes_per_s: f64) -> String {
    let k = bytes_per_s.max(0.0) * 8.0 / 1000.0;
    let (v, u) = if k >= 1e6 { (k / 1e6, "Gbps") } else if k >= 1000.0 { (k / 1000.0, "Mbps") } else { (k, "Kbps") };
    if v == 0.0 { format!("0 {u}") } else if v < 100.0 { format!("{v:.1} {u}") } else { format!("{v:.0} {u}") }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats() {
        assert_eq!(bytes(0.0), "0 B");
        assert_eq!(bytes(512.0), "512 B");
        assert_eq!(bytes(1536.0), "1.5 KB");
        assert_eq!(bytes(5.0 * 1024.0 * 1024.0 * 1024.0), "5.0 GB");
        assert_eq!(bytes(300.0 * 1024.0 * 1024.0), "300 MB");
        assert_eq!(rate(2048.0), "2.0 KB/s");
        assert_eq!(bits(0.0), "0 Kbps");
        assert_eq!(bits(1000.0), "8.0 Kbps");
        assert_eq!(bits(1_500_000.0), "12.0 Mbps");
        assert_eq!(bits(125_000_000.0), "1.0 Gbps");
    }
    #[test]
    fn process_snapshot_sees_system_processes() {
        let mut buf = vec![0; 1024];
        let p = processes(&mut buf);
        assert!(p.len() > 20);
        assert!(p.get(&4).is_some_and(|s| s.name == "System"));
        assert!(p.values().any(|s| s.name.eq_ignore_ascii_case("explorer") && s.private_ws > 0));
    }
    #[test]
    fn samples() {
        let mut s = Sampler::new();
        std::thread::sleep(Duration::from_millis(500));
        for sel in [CPU, RAM, GPU, DISK, NET] {
            let snap = s.sample(sel, 0);
            assert!(snap.frac.iter().all(|f| (0.0..=1.0).contains(f)));
            assert!(snap.frac[RAM] > 0.0);
            if sel == RAM {
                assert_eq!(snap.top.len(), 3)
            }
            println!("{sel}: {:?} {:?} {:?}", snap.text, snap.detail, snap.top.iter().map(|t| (&t.name, &t.value)).collect::<Vec<_>>());
        }
    }
}
#[cfg(test)]
#[test]
fn sample_cost() {
    let mut s = Sampler::new();
    std::thread::sleep(Duration::from_millis(1000));
    let t = Instant::now();
    for _ in 0..5 { s.sample(NONE, 0); }
    println!("sample(NONE) avg {:?}", t.elapsed() / 5);
    let t = Instant::now(); unsafe { PdhCollectQueryData(s.pdh.q) }; println!("pdh collect {:?}", t.elapsed());
    let t = Instant::now(); let n = s.pdh.array(s.pdh.gpu).len(); println!("gpu array {:?} ({n} instances)", t.elapsed());
    let t = Instant::now(); adapters(); println!("adapters {:?}", t.elapsed());
    let mut b = vec![0; 1024]; let t = Instant::now(); processes(&mut b); println!("process snapshot {:?}", t.elapsed());
}
#[cfg(test)]
#[test]
fn collect_cost() {
    let s = Sampler::new();
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(1000));
        let t = Instant::now(); unsafe { PdhCollectQueryData(s.pdh.q) }; let c = t.elapsed();
        let t = Instant::now(); let g = s.pdh.array(s.pdh.gpu); let a = t.elapsed();
        let t = Instant::now(); let mut x = 0.0; for c in [s.pdh.cpu, s.pdh.perf, s.pdh.freq] { x += s.pdh.value(c) } let v = t.elapsed();
        println!("collect {c:?} gpu-array {a:?} ({}) values {v:?} {x}", g.len());
    }
}
#[cfg(test)]
#[test]
fn tick_cost() {
    let mut s = Sampler::new();
    s.sample(NONE, 0);
    let mut total = Duration::ZERO;
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(1000));
        let t = Instant::now(); s.sample(NONE, 0); total += t.elapsed();
    }
    let t = Instant::now(); for (l, _) in &s.ifs { counters(*l); } let c = t.elapsed();
    println!("steady tick avg {:?}, adapter counters {c:?} for {}", total / 4, s.ifs.len());
}
#[cfg(test)]
#[test]
#[ignore]
fn cpu_trace() {
    let mut s = Sampler::new();
    for _ in 0..8 {
        std::thread::sleep(Duration::from_millis(1000));
        let snap = s.sample(NONE, 0);
        println!("pill {} at {:?}", snap.text[CPU], std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() % 100000);
    }
}
