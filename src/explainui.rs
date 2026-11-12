//! Explain mode, made quick: one click on the mascot turns it on. While on, the cursors glow (real
//! system cursors, so the glow never lags), and every fresh highlight gets a small tooltip right
//! by it: ✦ Explain (answer in a bubble right there) or ⌕ Search (web search).

use std::sync::atomic::{AtomicI32, AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::*;

pub const WM_APP_SELECT: u32 = WM_APP + 9; // a highlight just finished (screen point packed in lparam)
pub const WM_APP_DOWN: u32 = WM_APP + 10; // left button pressed somewhere (screen point)
pub const WM_APP_POPCLICK: u32 = WM_APP + 11; // click inside the tooltip / bubble (client point)

static MAIN: AtomicIsize = AtomicIsize::new(0);
static HOOK: AtomicIsize = AtomicIsize::new(0);
static DOWN_X: AtomicI32 = AtomicI32::new(0);
static DOWN_Y: AtomicI32 = AtomicI32::new(0);
static UP_T: AtomicU32 = AtomicU32::new(0);
static UP_X: AtomicI32 = AtomicI32::new(0);
static UP_Y: AtomicI32 = AtomicI32::new(0);

pub fn pack(x: i32, y: i32) -> LPARAM {
    LPARAM((((y as i16 as u16) as isize) << 16) | (x as i16 as u16) as isize)
}

pub fn unpack(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as u16 as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32)
}

fn post(msg: u32, lp: LPARAM) {
    let h = MAIN.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(0), lp);
        }
    }
}

/// Mouse hook: reports button-downs, and button-ups that end a drag or a double-click (a highlight).
extern "system" fn ll_mouse(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let m = &*(lp.0 as *const MSLLHOOKSTRUCT);
            let (x, y) = (m.pt.x, m.pt.y);
            match wp.0 as u32 {
                WM_LBUTTONDOWN => {
                    DOWN_X.store(x, Ordering::Relaxed);
                    DOWN_Y.store(y, Ordering::Relaxed);
                    post(WM_APP_DOWN, pack(x, y));
                }
                WM_LBUTTONUP => {
                    let dragged = (x - DOWN_X.load(Ordering::Relaxed)).abs() + (y - DOWN_Y.load(Ordering::Relaxed)).abs() > 6;
                    let since = m.time.wrapping_sub(UP_T.load(Ordering::Relaxed));
                    let near_last = (x - UP_X.load(Ordering::Relaxed)).abs() + (y - UP_Y.load(Ordering::Relaxed)).abs() < 6;
                    let double = since <= GetDoubleClickTime() && near_last;
                    UP_T.store(m.time, Ordering::Relaxed);
                    UP_X.store(x, Ordering::Relaxed);
                    UP_Y.store(y, Ordering::Relaxed);
                    if dragged || double {
                        post(WM_APP_SELECT, pack(x, y));
                    }
                }
                _ => {}
            }
        }
        CallNextHookEx(None, code, wp, lp)
    }
}

pub fn hook_on(main: HWND) {
    MAIN.store(main.0 as isize, Ordering::Relaxed);
    if HOOK.load(Ordering::Relaxed) != 0 {
        return;
    }
    unsafe {
        let hinst = GetModuleHandleW(None).map(HINSTANCE::from).ok();
        if let Ok(h) = SetWindowsHookExW(WH_MOUSE_LL, Some(ll_mouse), hinst, 0) {
            HOOK.store(h.0 as isize, Ordering::Relaxed);
        }
    }
}

pub fn hook_off() {
    let h = HOOK.swap(0, Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(h as *mut _));
        }
    }
}

// ---------- glowing system cursors ----------

fn flag_path() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(std::env::var_os("APPDATA")?).join("ResourcePill").join("cursors-changed"))
}

/// Replace the arrow, text and hand cursors with glowing copies (exactly in step with the mouse).
pub fn cursors_on(color: u32) {
    if let Some(p) = flag_path() {
        let _ = std::fs::write(p, b"1"); // so a crash is repaired on the next start
    }
    for (idc, ocr) in [(IDC_ARROW, OCR_NORMAL), (IDC_IBEAM, OCR_IBEAM), (IDC_HAND, OCR_HAND)] {
        unsafe {
            if let Ok(cur) = LoadCursorW(None, idc) {
                if let Some(glowing) = glow_cursor(cur, color) {
                    let _ = SetSystemCursor(glowing, ocr); // takes ownership of `glowing`
                }
            }
        }
    }
}

/// Put the user's own cursor scheme back (Windows reloads it from their settings).
pub fn cursors_off() {
    unsafe {
        let _ = SystemParametersInfoW(SPI_SETCURSORS, 0, None, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    if let Some(p) = flag_path() {
        let _ = std::fs::remove_file(p);
    }
}

/// At startup: if the app ended while explain mode was on, restore the normal cursors.
pub fn repair_cursors() {
    if flag_path().is_some_and(|p| p.exists()) {
        cursors_off();
    }
}

const PAD: i32 = 9; // room around the cursor for the glow

fn dib(w: i32, h: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
        ..Default::default()
    }
}

/// Separable box blur with radius `r`.
fn blur(src: &[f32], w: i32, h: i32, r: i32) -> Vec<f32> {
    let n = (2 * r + 1) as f32;
    let mut tmp = vec![0f32; src.len()];
    for y in 0..h {
        for x in 0..w {
            tmp[(y * w + x) as usize] = (-r..=r).map(|d| src[(y * w + (x + d).clamp(0, w - 1)) as usize]).sum::<f32>() / n;
        }
    }
    let mut out = vec![0f32; src.len()];
    for y in 0..h {
        for x in 0..w {
            out[(y * w + x) as usize] = (-r..=r).map(|d| tmp[((y + d).clamp(0, h - 1) * w + x) as usize]).sum::<f32>() / n;
        }
    }
    out
}

/// The cursor drawn on black and on white gives its exact coverage and colour (works for colour
/// and monochrome cursors); a bold halo is grown around that, and the cursor is placed on top.
fn glow_cursor(cur: HCURSOR, color: u32) -> Option<HCURSOR> {
    unsafe {
        let mut ii = ICONINFO::default();
        GetIconInfo(HICON(cur.0), &mut ii).ok()?;
        let mut bm = BITMAP::default();
        let src = if ii.hbmColor.is_invalid() { ii.hbmMask } else { ii.hbmColor };
        GetObjectW(src.into(), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _));
        let (cw, ch) = (bm.bmWidth, if ii.hbmColor.is_invalid() { bm.bmHeight / 2 } else { bm.bmHeight });
        let hot = (ii.xHotspot as i32, ii.yHotspot as i32);
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor.into());
        }
        let _ = DeleteObject(ii.hbmMask.into());
        if cw <= 0 || ch <= 0 || cw > 256 || ch > 256 {
            return None;
        }
        let draw_on = |bg: u32| -> Vec<u32> {
            let dc = CreateCompatibleDC(None);
            let mut bits = std::ptr::null_mut();
            let Ok(b) = CreateDIBSection(Some(dc), &dib(cw, ch), DIB_RGB_COLORS, &mut bits, None, 0) else { return Vec::new() };
            let old = SelectObject(dc, b.into());
            let px = std::slice::from_raw_parts_mut(bits as *mut u32, (cw * ch) as usize);
            px.fill(bg);
            let _ = DrawIconEx(dc, 0, 0, HICON(cur.0), cw, ch, 0, None, DI_NORMAL);
            let out = px.to_vec();
            SelectObject(dc, old);
            let _ = DeleteObject(b.into());
            let _ = DeleteDC(dc);
            out
        };
        let (black, white) = (draw_on(0), draw_on(0x00FF_FFFF));
        if black.is_empty() || white.is_empty() {
            return None;
        }
        let (w, h) = (cw + PAD * 2, ch + PAD * 2);
        let rgb = |p: u32| [((p >> 16) & 255) as f32, ((p >> 8) & 255) as f32, (p & 255) as f32];
        let mut cover = vec![0f32; (w * h) as usize];
        let mut col = vec![[0f32; 3]; (w * h) as usize];
        for y in 0..ch {
            for x in 0..cw {
                let i = (y * cw + x) as usize;
                let (b, wv) = (rgb(black[i]), rgb(white[i]));
                let a = (1.0 - (0..3).map(|k| wv[k] - b[k]).sum::<f32>() / (3.0 * 255.0)).clamp(0.0, 1.0);
                let o = ((y + PAD) * w + x + PAD) as usize;
                cover[o] = a;
                // colour = drawn-on-black / coverage (inverting cursors come out white)
                col[o] = if a > 0.01 { [(b[0] / a).min(255.0), (b[1] / a).min(255.0), (b[2] / a).min(255.0)] } else { [255.0; 3] };
            }
        }
        let halo = blur(&blur(&cover, w, h, 3), w, h, 2);
        let g = rgb(color);
        let mut px = vec![0u32; (w * h) as usize];
        for i in 0..px.len() {
            let ga = (halo[i] * 3.2).min(1.0) * 0.95; // bold, solid near the edge
            let ca = cover[i];
            let a = ca + ga * (1.0 - ca);
            if a <= 0.0 {
                continue;
            }
            let mixc = |k: usize| ((col[i][k] * ca + g[k] * ga * (1.0 - ca)) / a).round().clamp(0.0, 255.0) as u32;
            px[i] = ((a * 255.0) as u32) << 24 | mixc(0) << 16 | mixc(1) << 8 | mixc(2);
        }
        let mut bits = std::ptr::null_mut();
        let color_bm = CreateDIBSection(None, &dib(w, h), DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u32, px.len());
        let mask = CreateBitmap(w, h, 1, 1, None);
        let info = ICONINFO { fIcon: false.into(), xHotspot: (hot.0 + PAD) as u32, yHotspot: (hot.1 + PAD) as u32, hbmMask: mask, hbmColor: color_bm };
        let out = CreateIconIndirect(&info).ok().map(|i| HCURSOR(i.0));
        let _ = DeleteObject(color_bm.into());
        let _ = DeleteObject(mask.into());
        out
    }
}

// ---------- the tooltip / answer bubble window ----------

extern "system" fn pop_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        match m {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize), // keep the highlight in the other app
            WM_LBUTTONUP => {
                post(WM_APP_POPCLICK, l);
                LRESULT(0)
            }
            _ => DefWindowProcW(h, m, w, l),
        }
    }
}

/// Small layered, topmost window that never takes focus (so the highlighted text stays selected).
pub struct Pop {
    pub hwnd: HWND,
    pub memdc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    pub size: (i32, i32),
}

impl Pop {
    pub fn create() -> Option<Pop> {
        unsafe {
            let hinst = GetModuleHandleW(None).map(HINSTANCE::from).ok()?;
            let class = w!("ResourcePill.Pop");
            let wc = WNDCLASSW { lpfnWndProc: Some(pop_proc), hInstance: hinst, lpszClassName: class, hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(), ..Default::default() };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, class, w!(""), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst), None).ok()?;
            Some(Pop { hwnd, memdc: CreateCompatibleDC(None), bmp: HBITMAP::default(), old: HGDIOBJ::default(), size: (0, 0) })
        }
    }

    pub fn ensure(&mut self, w: i32, h: i32) -> bool {
        if self.size == (w, h) && !self.bmp.is_invalid() {
            return true;
        }
        unsafe {
            if !self.bmp.is_invalid() {
                SelectObject(self.memdc, self.old);
                let _ = DeleteObject(self.bmp.into());
            }
            let mut bits = std::ptr::null_mut();
            let Ok(b) = CreateDIBSection(Some(self.memdc), &dib(w, h), DIB_RGB_COLORS, &mut bits, None, 0) else { return false };
            self.bmp = b;
            self.old = SelectObject(self.memdc, b.into());
            self.size = (w, h);
        }
        true
    }

    pub fn present(&self, at: POINT) {
        let size = SIZE { cx: self.size.0, cy: self.size.1 };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8, ..Default::default() };
        unsafe {
            let _ = UpdateLayeredWindow(self.hwnd, None, Some(&at), Some(&size), Some(self.memdc), Some(&POINT::default()), COLORREF(0), Some(&blend), ULW_ALPHA);
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn rect(&self) -> RECT {
        let mut r = RECT::default();
        unsafe {
            if IsWindowVisible(self.hwnd).as_bool() {
                let _ = GetWindowRect(self.hwnd, &mut r);
            }
        }
        r
    }
}

impl Drop for Pop {
    fn drop(&mut self) {
        unsafe {
            if !self.bmp.is_invalid() {
                SelectObject(self.memdc, self.old);
                let _ = DeleteObject(self.bmp.into());
            }
            let _ = DeleteDC(self.memdc);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Percent-encode for a search URL.
pub fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn packs_screen_points_and_encodes_urls() {
        for (x, y) in [(0, 0), (-1280, 15), (3839, 1079), (-5, -10)] {
            assert_eq!(super::unpack(super::pack(x, y)), (x, y));
        }
        assert_eq!(super::url_encode("what is RAM? 50%"), "what+is+RAM%3F+50%25");
    }
}
