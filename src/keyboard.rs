//! Keyboard connection: is your external keyboard on Bluetooth, on a cable, or not connected?
//! Each keyboard Windows knows about is traced up its device chain: a Bluetooth radio above it
//! means Bluetooth, a USB port means a cable (or a USB receiver), and the motherboard (a laptop's
//! own keyboard) means built in, which is left out. Windows announces every plug, unplug and
//! Bluetooth connection (WM_DEVICECHANGE), so the pill re-checks right then.

use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::GUID_DEVINTERFACE_KEYBOARD;
use windows::Win32::Devices::Properties::{DEVPKEY_Device_BusReportedDeviceDesc, DEVPKEY_Device_FriendlyName, DEVPKEY_Device_InstanceId, DEVPKEY_Device_LastArrivalDate, DEVPROPTYPE};
use windows::Win32::Foundation::DEVPROPKEY;
use windows::core::PCWSTR;

impl crate::App {
    /// The keyboard cell shows once an external keyboard is known (and the setting is on).
    pub fn show_kbd(&self) -> bool {
        self.cfg.kbd_show && !self.cfg.card && self.kbd_state.is_some()
    }

    /// Re-check the keyboard (startup, and after Windows says a device came or went). When it
    /// changes, a short note says how it is connected now.
    pub fn check_keyboard(&mut self, announce: bool) {
        let now = status();
        if now == self.kbd_state {
            return;
        }
        let resized = now.is_some() != self.kbd_state.is_some();
        if let (true, true, Some(s)) = (announce, self.cfg.kbd_show, &now) {
            let how = match s.connected {
                Some(Link::Bluetooth) => "connected by Bluetooth",
                Some(Link::Cable) => "connected by cable",
                None => "disconnected",
            };
            // a note in the strip (it clears itself), unless something more important is showing
            if self.job.as_ref().is_none_or(|j| j.note) {
                self.job = Some(crate::recorder::Job::note(crate::recorder::JobKind::Explain, "Keyboard", &format!("{} · {how}", s.name)));
            }
        }
        self.kbd_state = now;
        if resized {
            if let Some((x, y)) = self.cfg.pos {
                self.place_at(x, y, None);
            }
        }
        self.after_rec_change();
    }

    /// A keyboard with a small badge: Bluetooth rune (blue), plug (green), or a red slash when
    /// it isn't connected.
    pub fn draw_keyboard(&self, r: crate::R) {
        let Some(s) = &self.kbd_state else { return };
        let th = self.theme();
        let p = crate::pt;
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0 - 1.0);
        let off = s.connected.is_none();
        self.text("\u{E765}", &self.f.icon, crate::R { x: cx - 12.0, y: cy - 12.0, w: 24.0, h: 24.0 }, th.fg, if off { 0.35 } else { 0.85 });
        let (bx, by) = (cx + 8.0, cy + 8.0); // the badge, bottom right
        unsafe {
            match s.connected {
                Some(Link::Bluetooth) => {
                    let c = 0x3B8EEA;
                    self.rt.FillEllipse(&windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE { point: p(bx, by), radiusX: 5.5, radiusY: 5.5 }, self.color(c, 1.0));
                    // the Bluetooth rune
                    let (h, w) = (3.4, 2.0);
                    let lines = [((bx, by - h), (bx, by + h)), ((bx, by - h), (bx + w, by - h / 2.0)), ((bx + w, by - h / 2.0), (bx - w, by + h / 2.0)), ((bx, by + h), (bx + w, by + h / 2.0)), ((bx + w, by + h / 2.0), (bx - w, by - h / 2.0))];
                    for ((x0, y0), (x1, y1)) in lines {
                        self.rt.DrawLine(p(x0, y0), p(x1, y1), self.color(0xFFFFFF, 1.0), 1.0, &self.round);
                    }
                }
                Some(Link::Cable) => {
                    let c = 0x2FB86E;
                    self.rt.FillEllipse(&windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE { point: p(bx, by), radiusX: 5.5, radiusY: 5.5 }, self.color(c, 1.0));
                    // a little plug: two prongs, a body, a cord
                    self.rt.DrawLine(p(bx - 1.3, by - 3.4), p(bx - 1.3, by - 1.6), self.color(0xFFFFFF, 1.0), 1.0, &self.round);
                    self.rt.DrawLine(p(bx + 1.3, by - 3.4), p(bx + 1.3, by - 1.6), self.color(0xFFFFFF, 1.0), 1.0, &self.round);
                    self.rt.FillRoundedRectangle(&self.rrect(crate::R { x: bx - 2.6, y: by - 1.8, w: 5.2, h: 3.0 }, 1.0), self.color(0xFFFFFF, 1.0));
                    self.rt.DrawLine(p(bx, by + 1.2), p(bx, by + 3.6), self.color(0xFFFFFF, 1.0), 1.0, &self.round);
                }
                None => {
                    // not connected: a red slash across the keyboard
                    self.rt.DrawLine(p(cx - 9.0, cy + 7.0), p(cx + 9.0, cy - 7.0), self.color(th.hot, 0.95), 2.0, &self.round);
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Link {
    Bluetooth,
    Cable,
}

/// How a keyboard is connected, from the device IDs of it and everything above it (nearest
/// first); None for a built-in or virtual keyboard.
pub fn classify(chain: &[String]) -> Option<Link> {
    let up: Vec<String> = chain.iter().map(|s| s.to_ascii_uppercase()).collect();
    if up.iter().any(|id| id.starts_with("BTHLE") || id.starts_with("BTHENUM") || id.starts_with("BTHHFENUM") || id.contains("BTHLEDEVICE")) {
        Some(Link::Bluetooth)
    } else if up.iter().any(|id| id.starts_with("USB\\")) {
        Some(Link::Cable)
    } else {
        None // ACPI / I2C (a laptop's own keyboard), ROOT (virtual, e.g. remote desktop)
    }
}

/// Your keyboard's state: connected (how) or not, and its name.
#[derive(Clone, PartialEq, Debug)]
pub struct Status {
    pub connected: Option<Link>, // None: not connected to this PC right now
    pub last: Link,              // how it was connected most recently
    pub name: String,
}

/// Your external keyboard, as Windows remembers it: the one connected now (a cable one first),
/// else the one connected most recently (shown as disconnected). Mice that pose as keyboards for
/// their macro buttons are left out. None: no external keyboard has ever been used here.
pub fn status() -> Option<Status> {
    let mut found: Vec<(bool, u64, Link, String)> = Vec::new(); // (present, last connected, how, name)
    for path in keyboard_interfaces() {
        let Some(id) = interface_instance(&path) else { continue };
        let Some(node) = locate(&id) else { continue };
        let chain = ancestry(node);
        let Some(link) = classify(&chain.iter().map(|(_, id)| id.clone()).collect::<Vec<_>>()) else { continue };
        let present = unsafe {
            let (mut st, mut problem) = (CM_DEVNODE_STATUS_FLAGS::default(), CM_PROB::default());
            CM_Get_DevNode_Status(&mut st, &mut problem, node, 0) == CR_SUCCESS
        };
        let arrived = chain.iter().filter_map(|(n, _)| filetime_prop(*n, &DEVPKEY_Device_LastArrivalDate)).max().unwrap_or(0);
        // a readable name: the Bluetooth device's name, or what the USB device calls itself
        let name = chain
            .iter()
            .find(|(_, id)| {
                let id = id.to_ascii_uppercase();
                match link {
                    Link::Bluetooth => id.starts_with("BTHLE\\") || id.starts_with("BTHENUM\\") || id.starts_with("BTHLEDEVICE"),
                    Link::Cable => id.starts_with("USB\\") && !id.contains("ROOT_HUB"),
                }
            })
            .and_then(|(n, _)| string_prop(*n, &DEVPKEY_Device_FriendlyName).or_else(|| string_prop(*n, &DEVPKEY_Device_BusReportedDeviceDesc)))
            .or_else(|| string_prop(node, &DEVPKEY_Device_BusReportedDeviceDesc))
            .unwrap_or_else(|| "Keyboard".into());
        if name.to_ascii_lowercase().contains("mouse") || chain.iter().any(|(n, _)| string_prop(*n, &DEVPKEY_Device_BusReportedDeviceDesc).is_some_and(|d| d.to_ascii_lowercase().contains("mouse"))) {
            continue; // a gaming mouse's macro keys, not a keyboard
        }
        found.push((present, arrived, link, tidy(&name)));
    }
    pick(found)
}

/// Connected ones first (a cable over Bluetooth), then the most recently connected.
fn pick(mut found: Vec<(bool, u64, Link, String)>) -> Option<Status> {
    found.sort_by_key(|(present, arrived, link, _)| (std::cmp::Reverse(*present), std::cmp::Reverse(*link == Link::Cable && *present), std::cmp::Reverse(*arrived)));
    let (present, _, link, name) = found.into_iter().next()?;
    Some(Status { connected: present.then_some(link), last: link, name })
}

/// A date property (FILETIME ticks), e.g. when the device last connected.
fn filetime_prop(node: u32, key: &DEVPROPKEY) -> Option<u64> {
    let mut buf = [0u8; 8];
    let mut size = 8u32;
    let mut ty = DEVPROPTYPE::default();
    (unsafe { CM_Get_DevNode_PropertyW(node, key, &mut ty, Some(buf.as_mut_ptr()), &mut size, 0) } == CR_SUCCESS && size == 8).then(|| u64::from_le_bytes(buf))
}

/// "Keychron K2 Keyboard" stays; generic names become just "Keyboard".
fn tidy(name: &str) -> String {
    let n = name.trim();
    if n.is_empty() || n.eq_ignore_ascii_case("USB Input Device") || n.eq_ignore_ascii_case("HID Keyboard Device") || n.starts_with("Bluetooth LE") {
        "Keyboard".into()
    } else {
        n.to_string()
    }
}

/// Paths of every keyboard Windows knows (connected now or remembered).
fn keyboard_interfaces() -> Vec<String> {
    unsafe {
        let mut len = 0u32;
        if CM_Get_Device_Interface_List_SizeW(&mut len, &GUID_DEVINTERFACE_KEYBOARD, PCWSTR::null(), CM_GET_DEVICE_INTERFACE_LIST_ALL_DEVICES) != CR_SUCCESS || len <= 1 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if CM_Get_Device_Interface_ListW(&GUID_DEVINTERFACE_KEYBOARD, PCWSTR::null(), &mut buf, CM_GET_DEVICE_INTERFACE_LIST_ALL_DEVICES) != CR_SUCCESS {
            return Vec::new();
        }
        buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

fn interface_instance(path: &str) -> Option<String> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u8; 1024];
    let mut size = buf.len() as u32;
    let mut ty = DEVPROPTYPE::default();
    unsafe {
        if CM_Get_Device_Interface_PropertyW(PCWSTR(wide.as_ptr()), &DEVPKEY_Device_InstanceId, &mut ty, Some(buf.as_mut_ptr()), &mut size, 0) != CR_SUCCESS {
            return None;
        }
    }
    Some(utf16(&buf[..size as usize]))
}

fn locate(id: &str) -> Option<u32> {
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    let mut node = 0u32;
    (unsafe { CM_Locate_DevNodeW(&mut node, PCWSTR(wide.as_ptr()), CM_LOCATE_DEVNODE_PHANTOM) } == CR_SUCCESS).then_some(node) // also ones not connected now
}

/// The device and everything above it: (node, device ID), nearest first.
fn ancestry(mut node: u32) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for _ in 0..16 {
        let mut buf = [0u16; 512];
        if unsafe { CM_Get_Device_IDW(node, &mut buf, 0) } != CR_SUCCESS {
            break;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        out.push((node, String::from_utf16_lossy(&buf[..len])));
        let mut parent = 0u32;
        if unsafe { CM_Get_Parent(&mut parent, node, 0) } != CR_SUCCESS {
            break;
        }
        node = parent;
    }
    out
}

fn string_prop(node: u32, key: &DEVPROPKEY) -> Option<String> {
    let mut buf = vec![0u8; 1024];
    let mut size = buf.len() as u32;
    let mut ty = DEVPROPTYPE::default();
    unsafe {
        if CM_Get_DevNode_PropertyW(node, key, &mut ty, Some(buf.as_mut_ptr()), &mut size, 0) != CR_SUCCESS {
            return None;
        }
    }
    Some(utf16(&buf[..size as usize])).filter(|s| !s.is_empty())
}

fn utf16(bytes: &[u8]) -> String {
    let words: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let len = words.iter().position(|&c| c == 0).unwrap_or(words.len());
    String::from_utf16_lossy(&words[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tells_bluetooth_cable_and_built_in_apart() {
        // a Bluetooth LE keyboard
        assert_eq!(classify(&chain(&["HID\\{00001812-0000-1000-8000-00805F9B34FB}_DEV_VID&02046D_PID&B35B&COL01\\9&1", "BTHLEDEVICE\\{00001812-0000-1000-8000-00805F9B34FB}_DEV_VID&02046D", "BTHLE\\DEV_D7E1B2", "BTH\\MS_BTHLE\\6&1", "USB\\VID_8087&PID_0026\\5&1"])), Some(Link::Bluetooth), "a Bluetooth radio on USB is still Bluetooth");
        // a classic Bluetooth keyboard
        assert_eq!(classify(&chain(&["HID\\{00001124-0000-1000-8000-00805F9B34FB}_VID&0002046D", "BTHENUM\\{00001124-0000-1000-8000-00805F9B34FB}_VID&0002046D"])), Some(Link::Bluetooth));
        // the same keyboard on its cable
        assert_eq!(classify(&chain(&["HID\\VID_3434&PID_0220&MI_00\\8&1", "USB\\VID_3434&PID_0220&MI_00\\7&1", "USB\\VID_3434&PID_0220\\6&1", "USB\\ROOT_HUB30\\4&1"])), Some(Link::Cable));
        // a laptop's own keyboard, and remote desktop's virtual one
        assert_eq!(classify(&chain(&["ACPI\\MSFT0001\\4&1", "ACPI\\PNP0A08\\0", "ACPI_HAL\\PNP0C08\\0"])), None);
        assert_eq!(classify(&chain(&["HID\\CONVERTEDDEVICE&COL01\\5&1", "ACPI\\INT33D5\\0"])), None);
        assert_eq!(classify(&chain(&["ROOT\\RDP_KBD\\0000"])), None);
    }

    #[test]
    fn friendly_names() {
        assert_eq!(tidy("Keychron K2"), "Keychron K2");
        assert_eq!(tidy("USB Input Device"), "Keyboard");
        assert_eq!(tidy("  "), "Keyboard");
    }

    /// `cargo test --release live_keyboard -- --ignored --nocapture`: what is connected right now.
    #[test]
    #[ignore]
    fn live_keyboard() {
        for path in keyboard_interfaces() {
            let id = interface_instance(&path).unwrap_or_default();
            let chain: Vec<String> = locate(&id).map(ancestry).unwrap_or_default().into_iter().map(|(_, i)| i).collect();
            let node = locate(&id).unwrap_or(0);
            let st = unsafe { let (mut s, mut p) = (CM_DEVNODE_STATUS_FLAGS::default(), CM_PROB::default()); CM_Get_DevNode_Status(&mut s, &mut p, node, 0) };
            println!("{:?} status={:?} present={:?} {}", classify(&chain), st, bool_prop(node, &windows::Win32::Devices::Properties::DEVPKEY_Device_IsPresent), chain.first().cloned().unwrap_or_default());
        }
        println!("status: {:?}", status());
    }
}

#[cfg(test)]
mod pick_tests {
    use super::*;

    #[test]
    fn picks_your_keyboard() {
        let k = |present, arrived, link, name: &str| (present, arrived, link, name.to_string());
        // connected over Bluetooth beats an older cable connection
        let s = pick(vec![k(false, 100, Link::Cable, "K2"), k(true, 50, Link::Bluetooth, "K2")]).unwrap();
        assert_eq!((s.connected, s.name.as_str()), (Some(Link::Bluetooth), "K2"));
        // nothing connected: the most recent one, shown as disconnected (how it was last connected)
        let s = pick(vec![k(false, 100, Link::Cable, "Old"), k(false, 300, Link::Bluetooth, "K2")]).unwrap();
        assert_eq!((s.connected, s.last, s.name.as_str()), (None, Link::Bluetooth, "K2"));
        // two connected: the cable one
        let s = pick(vec![k(true, 300, Link::Bluetooth, "A"), k(true, 100, Link::Cable, "B")]).unwrap();
        assert_eq!(s.name, "B");
        assert!(pick(Vec::new()).is_none(), "never had one: nothing to show");
    }
}

/// A yes/no property.
#[cfg(test)]
fn bool_prop(node: u32, key: &DEVPROPKEY) -> Option<bool> {
    let mut buf = [0u8; 1];
    let mut size = 1u32;
    let mut ty = DEVPROPTYPE::default();
    (unsafe { CM_Get_DevNode_PropertyW(node, key, &mut ty, Some(buf.as_mut_ptr()), &mut size, 0) } == CR_SUCCESS).then(|| buf[0] != 0)
}
