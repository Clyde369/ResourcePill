//! Temperatures readable without admin rights:
//! - GPU: the kernel graphics interface Task Manager uses for its "GPU 0 (47 °C)" readout.
//! - Drives: the drive's own sensor via IOCTL_STORAGE_QUERY_PROPERTY (NVMe/SATA SMART temperature).

use std::ffi::c_void;
use windows::Win32::Foundation::{CloseHandle, GENERIC_ACCESS_RIGHTS, HANDLE};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::*;
use windows::core::HSTRING;

// ---------- GPU (gdi32 D3DKMT, documented in d3dkmthk.h) ----------

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AdapterInfo {
    h_adapter: u32,
    luid_low: u32,
    luid_high: i32,
    sources: u32,
    precise: i32,
}

#[repr(C)]
struct EnumAdapters2 {
    count: u32,
    adapters: *mut AdapterInfo,
}

#[repr(C)]
struct QueryAdapterInfo {
    h_adapter: u32,
    kind: i32,
    data: *mut c_void,
    size: u32,
}

/// D3DKMT_ADAPTER_PERFDATA
#[repr(C)]
#[derive(Default)]
struct PerfData {
    physical_index: u32,
    mem_freq: u64,
    max_mem_freq: u64,
    max_mem_freq_oc: u64,
    mem_bandwidth: u64,
    pcie_bandwidth: u64,
    fan_rpm: u32,
    power: u32,
    temperature: u32, // deci-°C
    power_state_override: u8,
}

#[repr(C)]
struct CloseAdapter {
    h_adapter: u32,
}

const KMTQAITYPE_ADAPTERPERFDATA: i32 = 62;

#[link(name = "gdi32")]
unsafe extern "system" {
    fn D3DKMTEnumAdapters2(p: *mut EnumAdapters2) -> i32;
    fn D3DKMTQueryAdapterInfo(p: *const QueryAdapterInfo) -> i32;
    fn D3DKMTCloseAdapter(p: *const CloseAdapter) -> i32;
}

/// Hottest GPU in °C (same source as Task Manager), or None if no adapter reports one.
pub fn gpu() -> Option<f64> {
    unsafe {
        let mut e = EnumAdapters2 { count: 0, adapters: std::ptr::null_mut() };
        if D3DKMTEnumAdapters2(&mut e) != 0 || e.count == 0 {
            return None;
        }
        let mut list = vec![AdapterInfo::default(); e.count as usize];
        e.adapters = list.as_mut_ptr();
        if D3DKMTEnumAdapters2(&mut e) != 0 {
            return None;
        }
        let mut best: Option<f64> = None;
        for a in &list[..e.count as usize] {
            let mut p = PerfData::default();
            let q = QueryAdapterInfo { h_adapter: a.h_adapter, kind: KMTQAITYPE_ADAPTERPERFDATA, data: (&mut p as *mut PerfData).cast(), size: size_of::<PerfData>() as u32 };
            if D3DKMTQueryAdapterInfo(&q) == 0 && p.temperature > 0 {
                let t = p.temperature as f64 / 10.0;
                best = Some(best.map_or(t, |b: f64| b.max(t)));
            }
            D3DKMTCloseAdapter(&CloseAdapter { h_adapter: a.h_adapter });
        }
        best
    }
}

// ---------- drives ----------

/// Temperature in °C reported by PhysicalDrive{n}, or None if the drive doesn't expose one.
pub fn drive(n: u32) -> Option<f64> {
    unsafe {
        // Zero access rights: this query needs no read/write permission, so no admin.
        let h: HANDLE = CreateFileW(
            &HSTRING::from(format!(r"\\.\PhysicalDrive{n}")),
            GENERIC_ACCESS_RIGHTS(0).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .ok()?;
        let q = STORAGE_PROPERTY_QUERY { PropertyId: StorageDeviceTemperatureProperty, QueryType: PropertyStandardQuery, ..Default::default() };
        let mut buf = [0u64; 64];
        let mut ret = 0u32;
        let ok = DeviceIoControl(
            h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&q as *const STORAGE_PROPERTY_QUERY).cast()),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(buf.as_mut_ptr().cast()),
            (buf.len() * 8) as u32,
            Some(&mut ret),
            None,
        );
        let _ = CloseHandle(h);
        ok.ok()?;
        let d = &*(buf.as_ptr() as *const STORAGE_TEMPERATURE_DATA_DESCRIPTOR);
        if d.InfoCount == 0 || (ret as usize) < size_of::<STORAGE_TEMPERATURE_DATA_DESCRIPTOR>() {
            return None;
        }
        let t = d.TemperatureInfo[0].Temperature; // index 0 = composite / main sensor
        (t > -40 && t < 150).then_some(t as f64)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn probe() {
        let g = super::gpu();
        println!("gpu temp: {g:?}");
        for n in 0..6 {
            println!("PhysicalDrive{n}: {:?}", super::drive(n));
        }
        if let Some(t) = g {
            assert!((10.0..120.0).contains(&t));
        }
    }
}
