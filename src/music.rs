//! The player: a now-playing cell at the end of the pill (cover, song, artist, play/pause) and a
//! panel with big cover art, shuffle, previous, play/pause and next. Data comes from `media`.

use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows_numerics::Matrix3x2;

use crate::media::Cmd;
use crate::*;

const SHUFFLE: &str = "\u{E8B1}";
const PREV: &str = "\u{E892}";
const NEXT: &str = "\u{E893}";
const PLAY: &str = "\u{E768}";
const PAUSE: &str = "\u{E769}";
const NOTE: &str = "\u{EC4F}";

impl App {
    /// The media thread saw a change: copy it, decode a new cover, resize if the cell came or went.
    pub fn media_changed(&mut self) {
        let Some((shared, _)) = &self.media else { return };
        let n = shared.lock().map(|n| n.clone()).unwrap_or_default();
        let resized = n.present != self.now.present;
        if n.art_id != self.now.art_id || n.art.is_none() {
            self.art = n.art.as_ref().and_then(|b| self.decode_art(b));
        }
        self.now = n;
        if !self.now.present {
            self.music_open = false;
        }
        self.sync_pulse();
        unsafe {
            if resized {
                if let Some((x, y)) = self.cfg.pos {
                    self.place_at(x, y, None);
                }
            }
            self.render();
        }
    }

    pub fn toggle_music(&mut self) {
        let open = !self.music_open;
        self.select(NONE);
        self.settings = None;
        self.music_open = open;
        if let Some((x, y)) = self.cfg.pos {
            self.place_at(x, y, None);
        }
        self.kick();
    }

    fn send(&self, c: Cmd) {
        if let Some((_, tx)) = &self.media {
            let _ = tx.send(c);
        }
    }

    /// JPEG/PNG bytes → a small premultiplied bitmap for the shared render target.
    fn decode_art(&self, bytes: &[u8]) -> Option<ID2D1Bitmap> {
        unsafe {
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
            let stream = wic.CreateStream().ok()?;
            stream.InitializeFromMemory(bytes).ok()?;
            let dec = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
            let frame = dec.GetFrame(0).ok()?;
            let (mut w, mut h) = (0u32, 0u32);
            frame.GetSize(&mut w, &mut h).ok()?;
            let k = (192.0 / w.max(h).max(1) as f32).min(1.0); // big enough for the panel at 2x
            let scaler = wic.CreateBitmapScaler().ok()?;
            scaler.Initialize(&frame, ((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1), WICBitmapInterpolationModeFant).ok()?;
            let conv = wic.CreateFormatConverter().ok()?;
            conv.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut).ok()?;
            self.rt.CreateBitmapFromWicBitmap(&conv, None).ok()
        }
    }

    /// Cover art in a rounded square (or a note on the accent colour while there is none).
    fn draw_art(&self, r: R, rad: f32) {
        let th = self.theme();
        unsafe {
            if let Some(bmp) = &self.art {
                if let Ok(brush) = self.rt.CreateBitmapBrush(bmp, None, None) {
                    let s = bmp.GetSize();
                    // cover-fit: fill the square, crop the longer side
                    let k = (r.w / s.width).max(r.h / s.height);
                    let (ox, oy) = (r.x + (r.w - s.width * k) / 2.0, r.y + (r.h - s.height * k) / 2.0);
                    brush.SetTransform(&Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: ox, M32: oy });
                    brush.SetOpacity(self.alpha);
                    self.rt.FillRoundedRectangle(&self.rrect(r, rad), &brush);
                    self.rt.DrawRoundedRectangle(&self.rrect(r, rad), self.color(th.fg, 0.12), 1.0, None);
                    return;
                }
            }
            self.rt.FillRoundedRectangle(&self.rrect(r, rad), self.color(th.accent[0], 0.9));
            self.rt.FillRoundedRectangle(&self.rrect(r, rad), self.color(th.accent[1], 0.35));
        }
        self.text(NOTE, if r.w > 40.0 { &self.f.icon_big } else { &self.f.icon }, r, 0xFFFFFF, 0.95);
    }

    /// Three little bars that dance while music plays.
    fn draw_eq(&self, x: f32, base: f32, h: f32) {
        let t = (unsafe { windows::Win32::System::SystemInformation::GetTickCount() } % 3_600_000) as f32 / 1000.0;
        for k in 0..3 {
            let v = if self.now.playing { 0.35 + 0.65 * (0.5 + 0.5 * (t * (5.0 + k as f32 * 1.7) + k as f32 * 2.1).sin()) } else { 0.25 };
            let bh = h * v;
            unsafe {
                self.rt.FillRoundedRectangle(&self.rrect(R { x: x + k as f32 * 3.5, y: base - bh, w: 2.2, h: bh }, 1.1), self.color(self.theme().accent[1], 0.95));
            }
        }
    }

    /// The cell's own controls, right-aligned: previous, play/pause, next (horizontal pill only).
    fn cell_buttons(c: R) -> [(Cmd, R); 3] {
        let b = |right: f32, w: f32| R { x: c.x + c.w - right, y: c.y + c.h / 2.0 - 13.0, w, h: 26.0 };
        [(Cmd::Prev, b(80.0, 24.0)), (Cmd::PlayPause, b(55.0, 26.0)), (Cmd::Next, b(28.0, 24.0))]
    }

    pub fn draw_music_cell(&self, c: R, vertical: bool) {
        let th = self.theme();
        if vertical {
            let a = R { x: c.x + c.w / 2.0 - 16.0, y: c.y + 6.0, w: 32.0, h: 32.0 };
            self.draw_art(a, 8.0);
            if self.now.playing {
                self.draw_eq(a.x + a.w - 4.0, a.y + a.h + 1.0, 8.0);
            }
            return;
        }
        let a = R { x: c.x + 6.0, y: c.y + c.h / 2.0 - 14.0, w: 28.0, h: 28.0 };
        self.draw_art(a, 7.0);
        let tx = a.x + a.w + 8.0;
        let tw = c.x + c.w - 84.0 - tx;
        let eq = if self.now.playing { 13.0 } else { 0.0 };
        if self.now.playing {
            self.draw_eq(tx, c.y + 19.0, 9.0);
        }
        self.text(&self.now.title, &self.f.label, R { x: tx + eq, y: c.y + 7.0, w: tw - eq, h: 13.0 }, th.fg, 0.95);
        let sub = if self.now.artist.is_empty() { &self.now.app } else { &self.now.artist };
        self.text(sub, &self.f.small, R { x: tx, y: c.y + 21.0, w: tw, h: 14.0 }, th.fg, 0.55);
        self.draw_progress(R { x: tx, y: c.y + c.h - 6.0, w: tw, h: 2.0 });
        for (cmd, b) in Self::cell_buttons(c) {
            let icon = match cmd {
                Cmd::Prev => PREV,
                Cmd::Next => NEXT,
                _ => {
                    unsafe {
                        self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(b.x + b.w / 2.0, b.y + b.h / 2.0), radiusX: 12.0, radiusY: 12.0 }, self.color(th.fg, 0.08));
                    }
                    if self.now.playing { PAUSE } else { PLAY }
                }
            };
            self.text(icon, &self.f.icon, b, th.fg, if matches!(cmd, Cmd::PlayPause) { 0.95 } else { 0.75 });
        }
    }

    /// How far through the song: a track with the played part in the accent colour.
    fn draw_progress(&self, r: R) {
        if self.now.len <= 0.0 {
            return; // live streams have no length
        }
        let f = (self.now.position() / self.now.len).clamp(0.0, 1.0) as f32;
        let th = self.theme();
        unsafe {
            self.rt.FillRoundedRectangle(&self.rrect(r, r.h / 2.0), self.color(th.fg, 0.14));
            if f > 0.0 {
                self.rt.FillRoundedRectangle(&self.rrect(R { w: (r.w * f).max(r.h), ..r }, r.h / 2.0), self.color(th.accent[0], 1.0));
            }
        }
    }

    /// Big art on the left; app, song, artist, progress and the controls on the right.
    fn panel_parts(x: f32, y: f32, w: f32) -> (R, [(Cmd, R); 4]) {
        let art = R { x: x + 18.0, y: y + 18.0, w: 136.0, h: 136.0 };
        let cx = art.x + art.w + 18.0;
        let cw = x + w - 16.0 - cx;
        let cy = y + 116.0;
        let step = cw / 4.0;
        let btn = |i: f32, s: f32| R { x: cx + step * i + step / 2.0 - s / 2.0, y: cy + 18.0 - s / 2.0, w: s, h: s };
        (art, [(Cmd::Shuffle, btn(0.0, 30.0)), (Cmd::Prev, btn(1.0, 30.0)), (Cmd::PlayPause, btn(2.0, 40.0)), (Cmd::Next, btn(3.0, 30.0))])
    }

    pub fn draw_music_panel(&self, x: f32, y: f32, w: f32) {
        let th = self.theme();
        let (art, btns) = Self::panel_parts(x, y, w);
        self.draw_art(art, 12.0);
        let cx = art.x + art.w + 18.0;
        let cw = x + w - 16.0 - cx;
        let state = if self.now.playing { "PLAYING" } else { "PAUSED" };
        let head = if self.now.app.is_empty() { state.to_string() } else { format!("{} · {state}", self.now.app.to_uppercase()) };
        if self.now.playing {
            self.draw_eq(cx, y + 31.0, 9.0);
        }
        let eq = if self.now.playing { 14.0 } else { 0.0 };
        self.text(&head, &self.f.label, R { x: cx + eq, y: y + 20.0, w: cw - eq, h: 13.0 }, th.accent[0], 0.95);
        self.text(&self.now.title, &self.f.title, R { x: cx, y: y + 38.0, w: cw, h: 22.0 }, th.fg, 1.0);
        self.text(&self.now.artist, &self.f.row, R { x: cx, y: y + 61.0, w: cw, h: 18.0 }, th.fg, 0.6);
        if self.now.len > 0.0 {
            self.draw_progress(R { x: cx, y: y + 90.0, w: cw, h: 4.0 });
            let left = (self.now.len - self.now.position()).max(0.0);
            self.text(&media::clock(self.now.position()), &self.f.small, R { x: cx, y: y + 96.0, w: cw / 2.0, h: 14.0 }, th.fg, 0.55);
            self.text(&format!("-{}", media::clock(left)), &self.f.detail, R { x: cx + cw / 2.0, y: y + 96.0, w: cw / 2.0, h: 14.0 }, th.fg, 0.55);
        }
        self.draw_volume(x, y, w);
        for (cmd, r) in btns {
            let (icon, on) = match cmd {
                Cmd::Shuffle => (SHUFFLE, self.now.shuffle),
                Cmd::Prev => (PREV, None),
                Cmd::Next => (NEXT, None),
                Cmd::PlayPause | Cmd::Volume(_) => (if self.now.playing { PAUSE } else { PLAY }, None),
            };
            unsafe {
                if matches!(cmd, Cmd::PlayPause) {
                    self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(r.x + r.w / 2.0, r.y + r.h / 2.0), radiusX: r.w / 2.0, radiusY: r.h / 2.0 }, self.color(th.accent[0], 1.0));
                    self.text(icon, &self.f.icon_big, r, 0xFFFFFF, 1.0);
                    continue;
                }
            }
            let (rgb, a) = match (cmd, on) {
                (Cmd::Shuffle, None) => (th.fg, 0.22),         // the app can't shuffle
                (Cmd::Shuffle, Some(true)) => (th.accent[0], 1.0), // shuffle on
                _ => (th.fg, 0.85),
            };
            self.text(icon, &self.f.icon_big, r, rgb, a);
            if matches!((cmd, on), (Cmd::Shuffle, Some(true))) {
                unsafe {
                    self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(r.x + r.w / 2.0, r.y + r.h + 1.0), radiusX: 1.8, radiusY: 1.8 }, self.color(th.accent[0], 1.0));
                }
            }
        }
    }

    /// The volume slider along the bottom of the player (track, fill, knob, icon, percent).
    fn volume_bar(x: f32, y: f32, w: f32) -> R {
        R { x: x + 50.0, y: y + 176.0, w: w - 50.0 - 60.0, h: 4.0 }
    }

    fn draw_volume(&self, x: f32, y: f32, w: f32) {
        let Some((v, own)) = self.now.volume else { return };
        let th = self.theme();
        let bar = Self::volume_bar(x, y, w);
        let icon = if v <= 0.001 { "\u{E74F}" } else if v < 0.34 { "\u{E993}" } else if v < 0.67 { "\u{E994}" } else { "\u{E995}" };
        self.text(icon, &self.f.icon, R { x: x + 18.0, y: bar.y - 10.0, w: 24.0, h: 24.0 }, th.fg, 0.8);
        unsafe {
            self.rt.FillRoundedRectangle(&self.rrect(bar, 2.0), self.color(th.fg, 0.14));
            self.rt.FillRoundedRectangle(&self.rrect(R { w: (bar.w * v).max(4.0), ..bar }, 2.0), self.color(th.accent[0], 1.0));
            self.rt.FillEllipse(&D2D1_ELLIPSE { point: pt(bar.x + bar.w * v, bar.y + 2.0), radiusX: 6.0, radiusY: 6.0 }, self.color(0xFFFFFF, 0.95));
        }
        let label = format!("{}%{}", (v * 100.0).round(), if own { "" } else { " PC" });
        self.text(&label, &self.f.detail, R { x: bar.x + bar.w + 6.0, y: bar.y - 8.0, w: 48.0, h: 20.0 }, th.fg, 0.7);
    }

    /// Press on the volume row (in DIPs, panel = the player's final rect)?
    pub fn volume_hit(&self, x: f32, y: f32, panel: R) -> bool {
        let b = Self::volume_bar(panel.x, panel.y, panel.w);
        self.now.volume.is_some() && x >= b.x - 8.0 && x <= b.x + b.w + 8.0 && (y - b.y - 2.0).abs() < 12.0
    }

    /// Set the volume from a point on the slider (dragging) or by a step (mouse wheel).
    pub fn volume_at(&mut self, x: f32, panel: R) {
        let b = Self::volume_bar(panel.x, panel.y, panel.w);
        self.set_volume((x - b.x) / b.w);
    }

    pub fn volume_step(&mut self, notches: f32) {
        if let Some((v, _)) = self.now.volume {
            self.set_volume(v + notches * 0.05);
        }
    }

    fn set_volume(&mut self, v: f32) {
        let v = v.clamp(0.0, 1.0);
        if let Some((_, own)) = self.now.volume {
            self.now.volume = Some((v, own)); // show it straight away; the media thread confirms
        }
        self.send(Cmd::Volume(v));
        unsafe { self.render() };
    }

    /// Clicks in the pill cell: previous / play-pause / next act straight away; anything else
    /// opens the player.
    pub fn music_cell_click(&mut self, x: f32, y: f32, c: R, vertical: bool) {
        match Self::cell_buttons(c).iter().find(|(_, b)| !vertical && b.contains(x, y)) {
            Some((cmd, _)) => self.send(*cmd),
            None => self.toggle_music(),
        }
    }

    pub fn music_panel_click(&mut self, x: f32, y: f32, panel: R) {
        let (_, btns) = Self::panel_parts(panel.x, panel.y, panel.w);
        if let Some((cmd, _)) = btns.iter().find(|(_, r)| R { x: r.x - 6.0, y: r.y - 6.0, w: r.w + 12.0, h: r.h + 12.0 }.contains(x, y)) {
            if !(matches!(cmd, Cmd::Shuffle) && self.now.shuffle.is_none()) {
                self.send(*cmd);
            }
        }
    }
}
