//! Volume mixer: the PC's volume plus every app with sound open (Discord, Spotify, a browser…),
//! each with a slider, mute and mouse-wheel control. Same per-app volumes as Windows' own mixer.

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::core::Interface;

use crate::media::volume;
use crate::*;

pub const ROW: f32 = 44.0;
const TOP: f32 = 50.0; // title row

pub struct Row {
    pub name: String,
    pub stem: String, // program name, e.g. "spotify" (empty: the PC row)
    pub vol: f32,
    pub muted: bool,
    ctl: Vec<ISimpleAudioVolume>, // empty: the PC's master volume
}

pub struct Mixer {
    pub rows: Vec<Row>,
    master: Option<IAudioEndpointVolume>,
}

/// "discord" → "Discord", "msedge" → "Edge"
pub fn display_name(stem: &str) -> String {
    let n = media::app_name(stem);
    let mut c = n.chars();
    c.next().map_or(n.clone(), |f| f.to_uppercase().collect::<String>() + c.as_str())
}

impl Mixer {
    /// Read the current sessions: one row per app (all its processes together), loudest name first
    /// alphabetically after the PC row.
    pub fn load() -> Mixer {
        let master = volume::master();
        let mut rows = vec![Row {
            name: "PC volume".into(),
            stem: String::new(),
            vol: master.as_ref().and_then(|m| unsafe { m.GetMasterVolumeLevelScalar() }.ok()).unwrap_or(0.0),
            muted: master.as_ref().and_then(|m| unsafe { m.GetMute() }.ok()).is_some_and(|b| b.as_bool()),
            ctl: Vec::new(),
        }];
        let mut apps: Vec<Row> = Vec::new();
        unsafe {
            let list = volume::device().and_then(|d| d.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None).ok()).and_then(|m| m.GetSessionEnumerator().ok());
            if let Some(list) = list {
                for i in 0..list.GetCount().unwrap_or(0) {
                    let Ok(ctl) = list.GetSession(i) else { continue };
                    if ctl.GetState().is_ok_and(|s| s == AudioSessionStateExpired) {
                        continue;
                    }
                    let Ok(c2) = ctl.cast::<IAudioSessionControl2>() else { continue };
                    let pid = c2.GetProcessId().unwrap_or(0);
                    let Some(stem) = (pid != 0).then(|| volume::exe_stem(pid)).flatten() else { continue }; // pid 0: system sounds
                    if stem.starts_with("resourcepill") {
                        continue; // Peter's own helpers (the AI listener shows up as an audio app)
                    }
                    let Ok(v) = ctl.cast::<ISimpleAudioVolume>() else { continue };
                    let name = display_name(&stem);
                    match apps.iter_mut().find(|r| r.name == name) {
                        Some(r) => r.ctl.push(v),
                        None => apps.push(Row { name, stem, vol: v.GetMasterVolume().unwrap_or(1.0), muted: v.GetMute().is_ok_and(|b| b.as_bool()), ctl: vec![v] }),
                    }
                }
            }
        }
        apps.sort_by_key(|r| r.name.to_lowercase());
        rows.extend(apps);
        Mixer { rows, master }
    }

    /// Set row `i` to your level `v`; `k` is what auto-lower is multiplying it by right now (1 = nothing).
    pub fn set(&mut self, i: usize, v: f32, k: f32) {
        let v = v.clamp(0.0, 1.0);
        let Some(r) = self.rows.get_mut(i) else { return };
        r.vol = v;
        let v = v * k;
        unsafe {
            if r.ctl.is_empty() {
                if let Some(m) = &self.master {
                    let _ = m.SetMasterVolumeLevelScalar(v, std::ptr::null());
                }
            }
            for c in &r.ctl {
                let _ = c.SetMasterVolume(v, std::ptr::null());
            }
        }
    }

    pub fn toggle_mute(&mut self, i: usize) {
        let Some(r) = self.rows.get_mut(i) else { return };
        r.muted = !r.muted;
        unsafe {
            if r.ctl.is_empty() {
                if let Some(m) = &self.master {
                    let _ = m.SetMute(r.muted, std::ptr::null());
                }
            }
            for c in &r.ctl {
                let _ = c.SetMute(r.muted, std::ptr::null());
            }
        }
    }

    pub fn height(&self) -> f32 {
        TOP + self.rows.len() as f32 * ROW + 12.0 + 2.0 * ROW // + the two auto-lower switches
    }
}

/// Switch row `j` under the apps (0 auto-lower, 1 AI detection): (label, switch).
fn duck_parts(x: f32, y: f32, w: f32, rows: usize, j: usize) -> (R, R) {
    let ry = y + TOP + (rows + j) as f32 * ROW + 12.0;
    (R { x: x + 18.0, y: ry, w: w - 90.0, h: ROW }, R { x: x + w - 60.0, y: ry + ROW / 2.0 - 11.0, w: 40.0, h: 22.0 })
}

/// The small role tag under an app's name ("voice", "lowered"…): click it to change the role.
fn role_chip(name: R) -> R {
    R { x: name.x - 2.0, y: name.y + name.h - 19.0, w: name.w, h: 15.0 }
}

/// One row's parts: (name, slider track, percent, mute button).
fn row_parts(x: f32, y: f32, w: f32, i: usize) -> (R, R, R, R) {
    let ry = y + TOP + i as f32 * ROW;
    (
        R { x: x + 18.0, y: ry, w: 116.0, h: ROW },
        R { x: x + 142.0, y: ry + ROW / 2.0 - 2.0, w: w - 142.0 - 92.0, h: 4.0 },
        R { x: x + w - 84.0, y: ry, w: 42.0, h: ROW },
        R { x: x + w - 44.0, y: ry + ROW / 2.0 - 14.0, w: 28.0, h: 28.0 },
    )
}

impl App {
    pub fn toggle_mixer(&mut self) {
        let open = self.mixer.is_none();
        self.select(NONE);
        self.settings = None;
        self.music_open = false;
        self.mixer = open.then(Mixer::load);
        if let Some((x, y)) = self.cfg.pos {
            self.place_at(x, y, None);
        }
        self.kick();
    }

    /// Re-read the apps (some start or stop making sound); not while a slider is held.
    pub fn refresh_mixer(&mut self) {
        if self.mixer.is_some() && self.mix_drag.is_none() {
            self.mixer = Some(Mixer::load());
        }
    }

    /// Auto-lower started or ended: load the talking app's icon (once), redraw.
    pub fn duck_changed(&mut self) {
        let path = self.duck.talker.lock().ok().and_then(|t| t.as_ref().map(|(_, p)| p.clone()));
        if let Some(p) = path.filter(|p| !self.icons.contains_key(p)) {
            if let Some(b) = self.app_icon(&p) {
                self.icons.insert(p, b);
            }
        }
        self.sync_pulse(); // the badge breathes while it shows
        unsafe { self.render() };
    }

    /// A program's own icon (from its .exe) as a bitmap for the pill.
    pub(crate) fn app_icon(&self, path: &str) -> Option<ID2D1Bitmap> {
        use windows::Win32::Graphics::Imaging::*;
        unsafe {
            let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
            let mut large = HICON::default();
            if ExtractIconExW(PCWSTR(wide.as_ptr()), 0, Some(&mut large), None, 1) == 0 || large.is_invalid() {
                return None;
            }
            let bmp = (|| {
                let wic: IWICImagingFactory = windows::Win32::System::Com::CoCreateInstance(&CLSID_WICImagingFactory, None, windows::Win32::System::Com::CLSCTX_INPROC_SERVER).ok()?;
                let src = wic.CreateBitmapFromHICON(large).ok()?;
                let conv = wic.CreateFormatConverter().ok()?;
                conv.Initialize(&src, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut).ok()?;
                self.rt.CreateBitmapFromWicBitmap(&conv, None).ok()
            })();
            let _ = DestroyIcon(large);
            bmp
        }
    }

    /// A small four-point star (Peter's AI sparkle), centred at (x, y).
    pub fn sparkle(&self, x: f32, y: f32, s: f32) {
        unsafe {
            let Ok(g) = self.d2d.CreatePathGeometry() else { return };
            let Ok(sink) = g.Open() else { return };
            sink.BeginFigure(pt(x, y - s), D2D1_FIGURE_BEGIN_FILLED);
            for (dx, dy) in [(0.28, -0.28), (1.0, 0.0), (0.28, 0.28), (0.0, 1.0), (-0.28, 0.28), (-1.0, 0.0), (-0.28, -0.28)] {
                sink.AddLine(pt(x + dx * s, y + dy * s));
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            if sink.Close().is_ok() {
                self.rt.FillGeometry(&g, self.color(0xFFFFFF, 0.95), None);
            }
        }
    }

    /// Click on an app's role tag: auto → voice → lowered → not affected → auto. Saved.
    fn cycle_role(&mut self, stem: &str) {
        let mut roles = duck::parse_roles(&self.cfg.duck_roles);
        let current = roles.iter().position(|(k, _)| k == stem);
        // auto → the two roles it isn't (most useful first) → back to auto: e.g. a music app that
        // is "lowered · auto" goes to "voice", then "not affected", then automatic again
        let auto = duck::auto_role_playing(stem, &self.duck.media_app.lock().map(|m| m.clone()).unwrap_or_default());
        let others: Vec<duck::Role> = [duck::Role::Lowered, duck::Role::Voice, duck::Role::Ignored].into_iter().filter(|r| *r != auto).collect();
        let next = match current.map(|i| roles[i].1) {
            None => Some(others[0]),
            Some(r) if r == others[0] => Some(others[1]),
            _ => None, // back to automatic
        };
        if let Some(i) = current {
            roles.remove(i);
        }
        if let Some(r) = next {
            roles.push((stem.to_string(), r));
        }
        self.cfg.duck_roles = duck::format_roles(&roles);
        self.duck.set_roles(roles);
        self.cfg.save();
        unsafe { self.render() };
    }

    pub fn draw_mixer_button(&self, r: R) {
        let th = self.theme();
        let open = self.mixer.is_some();
        // while music is lowered for someone talking: their app's icon, plus a sparkle when the AI hears them
        if self.duck.ducking.load(std::sync::atomic::Ordering::Relaxed) {
            if self.record(Moving::Badge(r)) {
                return; // it breathes: drawn by the moving pass
            }
            let talker = self.duck.talker.lock().ok().and_then(|t| t.clone());
            let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
            let t = (unsafe { windows::Win32::System::SystemInformation::GetTickCount() } % 3_600_000) as f32 / 1000.0;
            unsafe {
                // a soft breathing ring: "making room for a voice"
                let breath = 0.5 + 0.5 * (t * 2.2).sin();
                self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(cx, cy), radiusX: 13.0, radiusY: 13.0 }, self.color(th.accent[0], 0.14 + 0.1 * breath));
                match talker.as_ref().and_then(|(_, path)| self.icons.get(path)) {
                    Some(icon) => {
                        let ir = R { x: cx - 9.0, y: cy - 9.0, w: 18.0, h: 18.0 };
                        self.rt.DrawBitmap(icon, Some(&ir.d2d()), self.alpha, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                    }
                    None => self.text("\u{E720}", &self.f.icon, r, th.accent[0], 1.0), // a microphone
                }
            }
            if self.duck.mode.load(std::sync::atomic::Ordering::Relaxed) == 1 {
                self.sparkle(cx + 10.0, cy - 10.0, 3.2 * (0.8 + 0.2 * (t * 2.3).sin()));
            }
            return;
        }
        unsafe {
            if open {
                self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(r.x + r.w / 2.0, r.y + r.h / 2.0), radiusX: 12.0, radiusY: 12.0 }, self.color(th.accent[0], 0.25));
            }
        }
        self.text("\u{E767}", &self.f.icon, r, if open { th.accent[0] } else { th.fg }, if open { 1.0 } else { 0.7 });
    }

    pub fn draw_mixer(&self, x: f32, y: f32, w: f32) {
        let Some(m) = &self.mixer else { return };
        let th = self.theme();
        unsafe {
            self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(x + 20.0, y + 22.0), radiusX: 4.0, radiusY: 4.0 }, self.color(th.accent[0], 1.0));
        }
        self.text("Volume mixer", &self.f.title, R { x: x + 32.0, y: y + 12.0, w: 200.0, h: 20.0 }, th.fg, 1.0);
        self.text("scroll or drag", &self.f.detail, R { x: x + w - 160.0, y: y + 12.0, w: 144.0, h: 20.0 }, th.fg, 0.45);
        unsafe {
            self.rt.FillRectangle(&R { x: x + 16.0, y: y + 40.0, w: w - 32.0, h: 1.0 }.d2d(), self.color(th.fg, 0.07));
        }
        for (i, r) in m.rows.iter().enumerate() {
            let (name, bar, pct, mute) = row_parts(x, y, w, i);
            let lowered = self.duck.own_level(&r.name);
            let vol = lowered.unwrap_or(r.vol); // your level, even while it's dipped for a voice
            let a = if r.muted { 0.4 } else { 1.0 };
            if i == 0 {
                self.text(&r.name, &self.f.label, name, th.fg, 0.95 * a);
            } else {
                // the app's name, and under it its auto-lower role (click to change)
                self.text(&r.name, &self.f.row, R { h: name.h - 14.0, ..name }, th.fg, 0.95 * a);
                let (role, auto) = self.duck.role(&r.stem);
                let (label, rgb) = match role {
                    duck::Role::Voice => ("voice", th.accent[1]),
                    duck::Role::Lowered => (if lowered.is_some() { "lowered now" } else { "lowered" }, th.accent[0]),
                    duck::Role::Ignored => ("not affected", th.fg),
                };
                let chip = role_chip(name);
                let text = if auto { format!("{label} · auto") } else { label.to_string() };
                unsafe {
                    self.rt.FillRoundedRectangle(&self.rrect(chip, chip.h / 2.0), self.color(rgb, if role == duck::Role::Ignored { 0.06 } else { 0.16 }));
                }
                self.text(&text, &self.f.small, R { x: chip.x + 7.0, w: chip.w - 8.0, ..chip }, if role == duck::Role::Ignored { th.fg } else { rgb }, if auto { 0.7 } else { 1.0 });
            }
            let accent = if i == 0 { th.accent[1] } else { th.accent[0] };
            unsafe {
                self.rt.FillRoundedRectangle(&self.rrect(bar, 2.0), self.color(th.fg, 0.14));
                self.rt.FillRoundedRectangle(&self.rrect(R { w: (bar.w * vol).max(4.0), ..bar }, 2.0), self.color(accent, a));
                self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(bar.x + bar.w * vol, bar.y + 2.0), radiusX: 6.5, radiusY: 6.5 }, self.color(0xFFFFFF, if r.muted { 0.5 } else { 0.95 }));
            }
            self.text(&format!("{}%", (vol * 100.0).round()), &self.f.row_val, pct, th.fg, 0.75 * a);
            let icon = if r.muted { "\u{E74F}" } else { "\u{E767}" };
            unsafe {
                self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(mute.x + 14.0, mute.y + 14.0), radiusX: 13.0, radiusY: 13.0 }, self.color(if r.muted { th.hot } else { th.fg }, if r.muted { 0.25 } else { 0.07 }));
            }
            self.text(icon, &self.f.icon, mute, if r.muted { th.hot } else { th.fg }, 0.9);
        }
        // the auto-lower switches
        unsafe {
            self.rt.FillRectangle(&R { x: x + 16.0, y: duck_parts(x, y, w, m.rows.len(), 0).0.y - 6.0, w: w - 32.0, h: 1.0 }.d2d(), self.color(th.fg, 0.07));
        }
        let mode = self.duck.mode.load(std::sync::atomic::Ordering::Relaxed);
        let ai_note = match (self.cfg.duck_ai, mode) {
            (true, 1) => "Listening with AI · on this PC, never saved",
            (true, 2) => "Starting… (using the level meanwhile)",
            (true, _) => "Waits for a call app · on this PC, never saved",
            (false, _) => "Off: uses the level, ignoring steady noise",
        };
        let rows = [("Lower music when people talk", "Just enough for voices to come through", self.cfg.duck, true), ("Smarter voice detection (AI)", ai_note, self.cfg.duck_ai, self.cfg.duck)];
        for (j, (title, note, on, live)) in rows.into_iter().enumerate() {
            let (label, sw) = duck_parts(x, y, w, m.rows.len(), j);
            let a = if live { 1.0 } else { 0.4 };
            self.text(title, &self.f.row, R { h: label.h / 2.0 + 4.0, ..label }, th.fg, 0.95 * a);
            self.text(note, &self.f.small, R { y: label.y + label.h / 2.0 + 1.0, h: 14.0, ..label }, th.fg, 0.5 * a);
            let knob = if on { sw.x + sw.w - 11.0 } else { sw.x + 11.0 };
            unsafe {
                self.rt.FillRoundedRectangle(&self.rrect(sw, sw.h / 2.0), self.color(if on { th.accent[0] } else { th.fg }, if on { a } else { 0.18 }));
                self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(knob, sw.y + sw.h / 2.0), radiusX: 8.0, radiusY: 8.0 }, self.color(0xFFFFFF, 0.97 * a));
            }
        }
    }

    /// Row whose slider is at (x, y), for press-and-drag.
    pub fn mixer_slider_at(&self, x: f32, y: f32, panel: R) -> Option<usize> {
        let m = self.mixer.as_ref()?;
        (0..m.rows.len()).find(|&i| {
            let (_, bar, _, _) = row_parts(panel.x, panel.y, panel.w, i);
            x >= bar.x - 8.0 && x <= bar.x + bar.w + 8.0 && (y - bar.y - 2.0).abs() < ROW / 2.0
        })
    }

    /// Dragging a slider sets your level (while dipped for a voice, the dip is kept on top of it).
    pub fn mixer_drag(&mut self, i: usize, x: f32, panel: R) {
        let (_, bar, _, _) = row_parts(panel.x, panel.y, panel.w, i);
        let k = self.dip_factor(i);
        if let Some(m) = self.mixer.as_mut() {
            m.set(i, (x - bar.x) / bar.w, k);
        }
        unsafe { self.render() };
    }

    /// What auto-lower is multiplying row `i` by right now (1 when it isn't dipped).
    fn dip_factor(&self, i: usize) -> f32 {
        let lowered = self.mixer.as_ref().and_then(|m| m.rows.get(i)).is_some_and(|r| self.duck.own_level(&r.name).is_some());
        if lowered { self.duck.factor() } else { 1.0 }
    }

    /// Clicks: the speaker button mutes / unmutes that row.
    pub fn mixer_click(&mut self, x: f32, y: f32, panel: R) {
        let Some(n) = self.mixer.as_ref().map(|m| m.rows.len()) else { return };
        for (j, id) in [(0, 714), (1, 715)] {
            let (label, sw) = duck_parts(panel.x, panel.y, panel.w, n, j);
            if sw.contains(x, y) || label.contains(x, y) {
                self.command(id); // the whole row flips its switch (saved with the settings)
                return;
            }
        }
        let stem = (1..n).find(|&i| role_chip(row_parts(panel.x, panel.y, panel.w, i).0).contains(x, y)).and_then(|i| self.mixer.as_ref().map(|m| m.rows[i].stem.clone()));
        if let Some(stem) = stem.filter(|s| !s.is_empty()) {
            self.cycle_role(&stem);
            return;
        }
        if let Some(i) = (0..n).find(|&i| row_parts(panel.x, panel.y, panel.w, i).3.contains(x, y)) {
            if let Some(m) = self.mixer.as_mut() {
                m.toggle_mute(i);
            }
            unsafe { self.render() };
        }
    }

    /// Mouse wheel over a row: 5% per notch.
    pub fn mixer_wheel(&mut self, x: f32, y: f32, panel: R, notches: f32) {
        let _ = x;
        let i = ((y - panel.y - TOP) / ROW).floor();
        let n = self.mixer.as_ref().map_or(0, |m| m.rows.len());
        if i >= 0.0 && (i as usize) < n {
            let i = i as usize;
            let k = self.dip_factor(i);
            let own = self.mixer.as_ref().and_then(|m| m.rows.get(i)).and_then(|r| self.duck.own_level(&r.name));
            let Some(m) = self.mixer.as_mut() else { return };
            let v = own.unwrap_or(m.rows[i].vol) + notches * 0.05;
            m.set(i, v, k);
            unsafe { self.render() };
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn friendly_names() {
        assert_eq!(super::display_name("discord"), "Discord");
        assert_eq!(super::display_name("spotify"), "Spotify");
        assert_eq!(super::display_name("msedge"), "Edge");
    }
}

#[cfg(test)]
mod live {
    /// `cargo test --release live_mixer -- --ignored --nocapture`: lists what the mixer sees right now.
    #[test]
    #[ignore]
    fn live_mixer() {
        let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
        for r in super::Mixer::load().rows {
            println!("{:<20} {:>4.0}% {}", r.name, r.vol * 100.0, if r.muted { "muted" } else { "" });
        }
    }
}
