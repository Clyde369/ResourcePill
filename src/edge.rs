//! Edge tuck: the pill slides off the nearest screen edge leaving a thin glowing line.
//! Throw it against a screen edge (or Ctrl+Alt+M) to tuck; with auto-hide on it also tucks when
//! the mouse is away. A tap on the line brings it back.

use std::time::{Duration, Instant};

use crate::*;

impl App {
    /// How far (physical px) to slide so only EDGE_LINE px of the pill stay on screen, towards the
    /// nearest monitor edge.
    pub fn tuck_shift(&self, lay: &Layout) -> (i32, i32) {
        let (px, py) = self.cfg.pos.unwrap_or((0, 0));
        let (w, h) = ((lay.pill.w * self.scale) as i32, (lay.pill.h * self.scale) as i32);
        let m = monitor_rect(POINT { x: px + w / 2, y: py + h / 2 });
        let options = [
            (py - m.top, (0, -(py - m.top + h - EDGE_LINE))),
            (m.bottom - (py + h), (0, m.bottom - py - EDGE_LINE)),
            (px - m.left, (-(px - m.left + w - EDGE_LINE), 0)),
            (m.right - (px + w), (m.right - px - EDGE_LINE, 0)),
        ];
        options.iter().min_by_key(|o| o.0).map_or((0, 0), |o| o.1)
    }

    fn in_use(&self) -> bool {
        self.settings.is_some() || self.sel != NONE || self.dragging || self.press.is_some() || self.explain_mode || self.menu_open
    }

    /// Ctrl+Alt+M: tuck away now, or bring it back.
    pub fn toggle_tuck(&mut self) {
        if !self.want_visible {
            self.set_visible(true); // fully hidden (tray): show first
            return;
        }
        self.tucked = !self.tucked;
        if self.tucked {
            self.settings = None;
            self.select(NONE);
        } else {
            self.near_at = Instant::now() + Duration::from_millis(1500); // a moment before auto-hide may tuck again
        }
        self.sync_edge_timer();
        self.sync_pulse();
        self.kick();
    }

    /// The mouse check runs only while it can matter: auto-hide on and the pill out.
    pub fn sync_edge_timer(&self) {
        unsafe {
            if self.cfg.auto_hide && !self.tucked {
                SetTimer(Some(self.hwnd), TIMER_EDGE, 80, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_EDGE);
            }
        }
    }

    pub fn edge_tick(&mut self) {
        let mut c = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut c);
        }
        // tucked: only a tap on the line brings it back (WM_LBUTTONDOWN), so nothing to watch
        if !self.tucked {
            let mut r = RECT::default();
            unsafe {
                let _ = GetWindowRect(self.hwnd, &mut r);
            }
            let near = c.x >= r.left && c.x < r.right && c.y >= r.top && c.y < r.bottom;
            if near || self.in_use() {
                if self.near_at < Instant::now() {
                    self.near_at = Instant::now();
                }
            } else if self.cfg.auto_hide && self.near_at.elapsed() > Duration::from_millis(900) {
                self.tucked = true;
                self.hover = NONE;
                self.sync_pulse();
                self.kick();
            }
        }
        self.sync_edge_timer();
    }

    /// The tucked pill's edge: a soft gradient line hugging the screen edge with a glow of colour
    /// fading onto the screen. Recording: it breathes red. Music: the light glides back and forth.
    pub fn draw_edge_line(&self, lay: &Layout) {
        if self.tuck < 0.05 {
            return;
        }
        let th = self.theme();
        let (dx, dy) = self.tuck_shift(lay);
        let p = lay.pill;
        let t = (unsafe { windows::Win32::System::SystemInformation::GetTickCount() } % 3_600_000) as f32 / 1000.0;
        let px = EDGE_LINE as f32 / self.scale; // line thickness in DIPs (what stays on screen)
        const GLOW: f32 = 8.0; // how far the colour spills onto the screen (inside the window margin)
        // which screen edge: `along` = horizontal line; `e` = the screen edge; `s` = towards the screen
        let (along, e, s) = if dy < 0 {
            (true, p.y + p.h - px, 1.0)
        } else if dy > 0 {
            (true, p.y + px, -1.0)
        } else if dx < 0 {
            (false, p.x + p.w - px, 1.0)
        } else {
            (false, p.x + px, -1.0)
        };
        let (a0, len) = if along { (p.x + p.w * 0.1, p.w * 0.8) } else { (p.y + p.h * 0.1, p.h * 0.8) };
        let rec = self.rec.busy();
        let (base, hi) = if rec { (th.hot, th.hot) } else { (th.accent[0], th.accent[1]) };
        let k = self.tuck * if rec { 0.65 + 0.35 * (t * 4.0).sin() } else { 1.0 };
        // where the brightest point sits: centre, or gliding while music plays
        let pos = if self.now.playing && !rec { 0.2 + 0.6 * (0.5 - 0.5 * (t * 1.4).cos()) } else { 0.5 };
        let at = |along_v: f32, out: f32| if along { pt(along_v, e + s * out) } else { pt(e + s * out, along_v) };
        let rect = |out: f32| {
            let (o0, o1) = (e.min(e + s * out), e.max(e + s * out));
            if along { R { x: a0, y: o0, w: len, h: o1 - o0 } } else { R { x: o0, y: a0, w: o1 - o0, h: len } }
        };
        let stops = |list: &[(f32, u32, f32)]| -> Option<ID2D1GradientStopCollection> {
            let v: Vec<D2D1_GRADIENT_STOP> = list.iter().map(|&(position, rgb, a)| D2D1_GRADIENT_STOP { position, color: color(rgb, a * k) }).collect();
            unsafe { self.rt.CreateGradientStopCollection(&v, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) }.ok()
        };
        unsafe {
            // the glow: an oval of colour centred on the bright point, fading away from the edge
            if let Some(g) = stops(&[(0.0, hi, 0.5), (0.4, base, 0.22), (1.0, base, 0.0)]) {
                let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES { center: at(a0 + len * pos, 0.0), gradientOriginOffset: pt(0.0, 0.0), radiusX: if along { len * 0.55 } else { px + GLOW }, radiusY: if along { px + GLOW } else { len * 0.55 } };
                if let Ok(b) = self.rt.CreateRadialGradientBrush(&props, None, &g) {
                    self.rt.FillRectangle(&rect(px + GLOW).d2d(), &b);
                }
            }
            // the line: transparent at both ends, brightest at `pos`
            if let Some(g) = stops(&[(0.0, base, 0.0), (0.15, base, 0.75), (pos, hi, 1.0), (0.85, base, 0.75), (1.0, base, 0.0)]) {
                let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: at(a0, 0.0), endPoint: at(a0 + len, 0.0) };
                if let Ok(b) = self.rt.CreateLinearGradientBrush(&props, None, &g) {
                    self.rt.FillRoundedRectangle(&self.rrect(rect(px), px / 2.0), &b);
                }
            }
        }
    }
}
