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
        self.duck.set_media_app(if n.present { &n.app } else { "" }); // what is playing gets lowered automatically
        let resized = n.present != self.now.present;
        if n.art_id != self.now.art_id || n.art.is_none() {
            self.art = n.art.as_ref().and_then(|b| self.decode_art(b));
            self.tint_from_cover(n.art.as_deref().map(|v| v.as_slice()));
        }
        // only the song position (or volume, shown only in the open player) moved: the progress bar is a
        // moving piece and redraws itself, so no full repaint is needed
        let only_time = media::Now { pos: self.now.pos, stamp: self.now.stamp, volume: self.now.volume, ..n.clone() } == self.now && !self.music_open;
        self.now = n;
        if only_time {
            return;
        }
        if !self.now.present {
            self.music_open = false;
            self.tint_from_cover(None); // nothing playing: back to the plain theme
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

    /// The aurora: two soft glows in the cover's colours drifting across a surface on their own
    /// rhythms (9 s and 13 s, so the pattern never quite repeats), breathing with how loud the
    /// music is, and now and then one of Peter's little sparkles twinkles where the light is.
    pub(crate) fn draw_aurora(&self, r: R, rad: f32) {
        use std::f32::consts::TAU;
        let t = (unsafe { windows::Win32::System::SystemInformation::GetTickCount() } % 3_600_000) as f32 / 1000.0;
        // how loud the PC is playing (one cheap meter read a frame), smoothed so it breathes, not flickers
        let level = {
            let mut m = self.beat_meter.borrow_mut();
            if m.is_none() {
                *m = media::volume::device().and_then(|d| unsafe { d.Activate(windows::Win32::System::Com::CLSCTX_ALL, None) }.ok());
            }
            let peak = m.as_ref().and_then(|m| unsafe { m.GetPeakValue() }.ok()).unwrap_or(0.0);
            let b = self.beat.get();
            let b = b + (peak - b) * if peak > b { 0.35 } else { 0.06 }; // quick to rise, slow to fall
            self.beat.set(b);
            b
        };
        let c = |k: usize| ((self.tint[k] as u32) << 16) | ((self.tint[k + 1] as u32) << 8) | self.tint[k + 2] as u32;
        let glow = self.tint_a * (0.55 + 0.6 * level.min(1.0));
        let blobs = [
            // (colour, where along the surface 0..1, radius, strength)
            (c(0), 0.5 + 0.42 * (t * TAU / 9.0).sin(), 0.34, 0.30),
            (mix(c(3), c(0), 0.35), 0.5 + 0.42 * (t * TAU / 13.0 + 2.1).sin(), 0.42, 0.26),
            (mix(c(0), 0xFFFFFF, 0.35), 0.5 + 0.45 * (t * TAU / 7.0 + 4.0).sin(), 0.18, 0.16),
        ];
        unsafe {
            for (i, (rgb, along, size, strength)) in blobs.into_iter().enumerate() {
                let wobble = 0.5 + 0.35 * (t * TAU / (5.0 + i as f32 * 2.3) + i as f32).sin();
                let center = if r.w >= r.h { pt(r.x + r.w * along, r.y + r.h * wobble) } else { pt(r.x + r.w * wobble, r.y + r.h * along) };
                let (rx, ry) = if r.w >= r.h { (r.w * size, r.h * 1.3) } else { (r.w * 1.3, r.h * size) };
                let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: color(rgb, strength * glow * self.alpha) }, D2D1_GRADIENT_STOP { position: 1.0, color: color(rgb, 0.0) }];
                let Ok(coll) = self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { continue };
                let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES { center, gradientOriginOffset: pt(0.0, 0.0), radiusX: rx, radiusY: ry };
                if let Ok(b) = self.rt.CreateRadialGradientBrush(&props, None, &coll) {
                    self.rt.FillRoundedRectangle(&self.rrect(r, rad), &b);
                }
            }
        }
        // Peter's touch: every 6 s a sparkle twinkles for a moment somewhere along the light
        let cycle = t / 6.0;
        let phase = cycle.fract();
        if phase < 0.25 && self.tint_a > 0.5 {
            let k = cycle.floor();
            let spot = 0.12 + 0.76 * ((k * 0.618_034).fract()); // a different spot each time
            let s = (phase / 0.25 * std::f32::consts::PI).sin() * 3.2;
            let (x, y) = if r.w >= r.h { (r.x + r.w * spot, r.y + r.h * 0.28) } else { (r.x + r.w * 0.72, r.y + r.h * spot) };
            self.sparkle(x, y, s);
        }
    }

    /// Point the pill's colours at a cover (or back to the plain theme with None / the setting off).
    pub fn tint_from_cover(&mut self, cover: Option<&[u8]>) {
        let colours = cover.filter(|_| self.cfg.art_tint).and_then(Self::art_palette);
        match colours {
            Some((a, b)) => {
                let f = |c: u32| [((c >> 16) & 255) as f32, ((c >> 8) & 255) as f32, (c & 255) as f32];
                let (fa, fb) = (f(a), f(b));
                self.tint_to = [fa[0], fa[1], fa[2], fb[0], fb[1], fb[2]];
                if self.tint_a < 0.01 {
                    self.tint = self.tint_to; // first colours fade in rather than sweep from black
                }
                self.tint_a_to = 1.0;
            }
            None => self.tint_a_to = 0.0,
        }
        self.kick();
    }

    pub fn toggle_music(&mut self) {
        let open = !self.music_open;
        self.select(NONE);
        self.settings = None;
        self.music_open = open;
        self.mixer = None;
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
    /// The cover's two theme colours: its most vivid colour, and a second one (another strong
    /// colour if there is one, else a deep shade of the first). Read from a 24×24 copy.
    pub(crate) fn art_palette(bytes: &[u8]) -> Option<(u32, u32)> {
        unsafe {
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
            let stream = wic.CreateStream().ok()?;
            stream.InitializeFromMemory(bytes).ok()?;
            let frame = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?.GetFrame(0).ok()?;
            let scaler = wic.CreateBitmapScaler().ok()?;
            scaler.Initialize(&frame, 24, 24, WICBitmapInterpolationModeFant).ok()?;
            let conv = wic.CreateFormatConverter().ok()?;
            conv.Initialize(&scaler, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut).ok()?;
            let mut px = vec![0u8; 24 * 24 * 4];
            conv.CopyPixels(std::ptr::null(), 24 * 4, &mut px).ok()?;
            palette(&px)
        }
    }

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
    pub(crate) fn draw_eq(&self, x: f32, base: f32, h: f32) {
        if self.record(Moving::Eq(x, base, h)) {
            return; // moves every frame: drawn by the moving pass
        }
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
        // while this music is turned down for someone talking, say so (it comes back by itself)
        let lowered_for = (self.duck.own_level(&self.now.app).is_some())
            .then(|| self.duck.talker.lock().ok().and_then(|t| t.as_ref().map(|(n, _)| format!("\u{2193} for {n}"))))
            .flatten();
        match lowered_for {
            Some(note) => self.text(&note, &self.f.small, R { x: tx, y: c.y + 21.0, w: tw, h: 14.0 }, th.accent[0], 0.95),
            None => {
                let sub = if self.now.artist.is_empty() { &self.now.app } else { &self.now.artist };
                self.text(sub, &self.f.small, R { x: tx, y: c.y + 21.0, w: tw, h: 14.0 }, th.fg, 0.55);
            }
        }
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
    pub(crate) fn draw_progress(&self, r: R) {
        if self.record(Moving::Progress(r)) {
            return;
        }
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

/// Theme colours from BGRA pixels: vivid colours are grouped by hue (12 groups) and weighed by
/// how colourful and bright they are. The strongest group is the main colour; a second strong
/// group at least 60° away is the second colour, else a deep shade of the main one (so a purple
/// and black cover gives purple fading to near-black).
pub fn palette(px: &[u8]) -> Option<(u32, u32)> {
    let mut bins = [(0f32, 0f32, 0f32, 0f32); 12]; // weighted r, g, b, weight
    let mut all = (0f32, 0f32, 0f32, 0f32);
    for p in px.chunks_exact(4) {
        let (b, g, r, a) = (p[0] as f32, p[1] as f32, p[2] as f32, p[3]);
        if a < 128 {
            continue;
        }
        all = (all.0 + r, all.1 + g, all.2 + b, all.3 + 1.0);
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        let (s, v) = (if mx > 0.0 { (mx - mn) / mx } else { 0.0 }, mx / 255.0);
        if s < 0.25 || v < 0.2 {
            continue; // greys, whites and near-blacks don't decide the colour
        }
        let d = mx - mn;
        let hue = if mx == r { ((g - b) / d).rem_euclid(6.0) } else if mx == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 } * 60.0;
        let w = s * v * s;
        let k = ((hue / 30.0) as usize).min(11);
        bins[k] = (bins[k].0 + r * w, bins[k].1 + g * w, bins[k].2 + b * w, bins[k].3 + w);
    }
    if all.3 == 0.0 {
        return None;
    }
    let rgb = |(r, g, b, w): (f32, f32, f32, f32)| ((r / w).round() as u32) << 16 | ((g / w).round() as u32) << 8 | (b / w).round() as u32;
    let mut order: Vec<usize> = (0..12).filter(|&k| bins[k].3 > 0.0).collect();
    order.sort_by(|a, b| bins[*b].3.total_cmp(&bins[*a].3));
    let average = rgb(all);
    let Some(&first) = order.first() else {
        return Some((average, crate::mix(average, 0, 0.6))); // no vivid colour: the cover's overall tone
    };
    let main = rgb(bins[first]);
    let far = |k: usize| {
        let d = (k as i32 - first as i32).rem_euclid(12);
        d.min(12 - d) >= 2
    };
    let second = order.iter().skip(1).find(|&&k| far(k) && bins[k].3 >= bins[first].3 * 0.25).map(|&k| rgb(bins[k])).unwrap_or_else(|| crate::mix(main, 0x050508, 0.75));
    Some((main, second))
}

#[cfg(test)]
mod palette_tests {
    fn img(colors: &[(u8, u8, u8)]) -> Vec<u8> {
        (0..576).flat_map(|i| {
            let (r, g, b) = colors[i * colors.len() / 576];
            [b, g, r, 255]
        }).collect()
    }

    #[test]
    fn purple_and_black_cover() {
        let (main, second) = super::palette(&img(&[(140, 40, 220), (0, 0, 0)])).unwrap();
        assert_eq!(main, 0x8C28DC, "the purple leads");
        let (r, g, b) = ((second >> 16) & 255, (second >> 8) & 255, second & 255);
        assert!(r < 60 && g < 30 && b < 80, "then a deep purple-black: {second:06X}");
    }

    #[test]
    fn two_colour_cover_keeps_both() {
        let (main, second) = super::palette(&img(&[(230, 60, 40), (230, 60, 40), (40, 90, 230)])).unwrap();
        assert_eq!(main, 0xE63C28, "the bigger colour leads");
        assert_eq!(second, 0x285AE6, "the other strong colour follows");
    }

    #[test]
    fn grey_cover_uses_its_tone() {
        assert!(super::palette(&img(&[(120, 120, 120)])).is_some());
        assert!(super::palette(&[0u8; 16]).is_none(), "fully transparent: nothing to go on");
    }
}
