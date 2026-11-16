//! Settings: one list of items, shown two ways — as a panel drawn in the pill's own style (gear
//! button / right-click on the pill) and as a standard menu from the tray icon (works while the
//! pill is hidden). Both run the same `App::command` ids.

use crate::*;

pub enum MI {
    Act { id: usize, label: String, right: String, checked: bool, enabled: bool },
    Sub { label: String, right: String, items: Vec<MI> },
    Sep,
    Head(String),
}

fn act(id: usize, label: &str, checked: bool) -> MI {
    MI::Act { id, label: label.into(), right: String::new(), checked, enabled: true }
}

fn act_r(id: usize, label: &str, right: &str, checked: bool) -> MI {
    MI::Act { id, label: label.into(), right: right.into(), checked, enabled: true }
}

fn sub(label: &str, right: &str, items: Vec<MI>) -> MI {
    MI::Sub { label: label.into(), right: right.into(), items }
}

const ROW: f32 = 30.0;
const SEP: f32 = 9.0;
const HEAD: f32 = 24.0;
const TOP: f32 = 44.0; // panel header
const MAX_H: f32 = 460.0;

/// Right-hand notes longer than this go on a second line under the label (never cut off).
const LONG_NOTE: usize = 24;

fn long_note(right: &str) -> bool {
    right.chars().count() > LONG_NOTE
}

fn item_h(i: &MI) -> f32 {
    match i {
        MI::Act { right, .. } | MI::Sub { right, .. } if long_note(right) => ROW + 14.0,
        MI::Act { .. } | MI::Sub { .. } => ROW,
        MI::Sep => SEP,
        MI::Head(_) => HEAD,
    }
}

/// Commands that leave the panel (they open something or change what is on screen).
fn closes_panel(id: usize) -> bool {
    matches!(id, 1 | 999 | 713 | 800 | 1200 | 1201 | 1204 | 1205 | 1300 | 1499 | 1500) || (600..700).contains(&id) || (1400..1499).contains(&id)
}

impl App {
    pub fn settings_items(&self) -> Vec<MI> {
        let cfg = &self.cfg;
        let cpu = self.snap.frac[stats::CPU];
        let ram_free = recorder::ram_available();
        let recording = self.rec.busy();
        let pick = |base: usize, names: &[&str], cur: usize| names.iter().enumerate().map(|(i, n)| act(base + i, n, cur == i)).collect::<Vec<_>>();

        let appearance = sub(
            "Appearance",
            THEMES[cfg.theme.min(THEMES.len() - 1)].name,
            vec![
                MI::Sub { label: "Theme".into(), right: THEMES[cfg.theme.min(THEMES.len() - 1)].name.into(), items: pick(100, &THEMES.iter().map(|t| t.name).collect::<Vec<_>>(), cfg.theme) },
                sub("Size", SIZES[cfg.size].0, pick(300, &SIZES.iter().map(|s| s.0).collect::<Vec<_>>(), cfg.size)),
                sub("Background opacity", OPACITY[cfg.opacity].0, pick(400, &OPACITY.iter().map(|s| s.0).collect::<Vec<_>>(), cfg.opacity)),
                {
                    let level = match (cfg.fps, cfg.anim) { (3, 0) => "High", (2, 0) => "Medium", (0, 1) => "Low", _ => "Custom" };
                    sub("Performance", level, vec![
                        MI::Head("Higher is smoother but uses more of your PC".into()),
                        act_r(930, "High", "60 fps · every animation", level == "High"),
                        act_r(931, "Medium", "30 fps · lighter", level == "Medium"),
                        act_r(932, "Low", "almost nothing · no animations", level == "Low"),
                    ])
                },
                act(716, "Colours from the song cover", cfg.art_tint),
                sub("Animations", ANIMS[cfg.anim].split(' ').next().unwrap_or(""), pick(910, &ANIMS, cfg.anim)),
                sub("Frame rate", &format!("{} fps", FPS[cfg.fps.min(FPS.len() - 1)]), {
                    let mut v = vec![MI::Head("Higher is smoother but uses more CPU".into())];
                    v.extend(FPS.iter().enumerate().map(|(i, f)| act_r(920 + i, &format!("{f} fps"), ["lightest", "light", "balanced", "smoothest"][i], cfg.fps == i)));
                    v
                }),
                act(704, "Show clock", cfg.clock),
                MI::Sep,
                MI::Head("Layout".into()),
                act(800, if cfg.card { "Switch to pill" } else { "Switch to card (larger window)" }, false),
                sub("Metrics", "", LABELS.iter().enumerate().map(|(i, l)| act(200 + i, l, cfg.shown[i])).collect()),
                sub("Snap to", "", PLACES.iter().enumerate().map(|(i, p)| act(600 + i, p, false)).collect()),
                act(700, "Vertical when docked to a side", cfg.side_layout),
                act_r(709, "Auto-hide at the screen edge", "Ctrl+Alt+M by hand", cfg.auto_hide),
                act(701, "Click-through (unlock from tray)", cfg.click_through),
            ],
        );

        let gem = extras::has_key("gemini");
        let need = |ok: bool| if ok { "" } else { "needs a key" };
        let tx_ai = || vec![
            MI::Head("Transcription AI".into()),
            act(1210, "On this PC (private, free)", !cfg.tx_cloud),
            act_r(1211, "Gemini (cloud · transcript + summary)", need(gem), cfg.tx_cloud),
        ];
        let mut tx = vec![act_r(1205, "Setup guide", "change models", false), MI::Sep, MI::Head("Speech to text · predicted for this PC now".into())];
        for (i, (id, name)) in recorder::ASR.iter().enumerate() {
            tx.push(act_r(1000 + i, name, &recorder::asr_prediction(id, cpu), cfg.asr == i));
        }
        tx.push(MI::Sep);
        tx.push(MI::Head("Summary".into()));
        for (i, (dir, file, name)) in recorder::LLM.iter().enumerate() {
            tx.push(act_r(1100 + i, name, &recorder::llm_prediction(dir, file, self.vram_total, self.snap.vram_used, ram_free), cfg.llm == i));
        }
        tx.push(act(1100 + recorder::LLM.len(), "No summary (transcript only)", cfg.llm == recorder::LLM.len()));
        tx.push(MI::Sep);
        tx.push(act_r(1202, "Accurate pass after stopping", &recorder::final_prediction(cpu), cfg.final_pass));
        tx.push(act(1203, "Save the audio (listen back in Recordings)", cfg.keep_audio));
        tx.push(act(1200, "Open recordings folder", false));
        tx.push(act_r(1206, "Recordings folder…", &recorder::recordings_dir().display().to_string(), false));
        tx.push(MI::Sep);
        tx.extend(tx_ai());
        let asr_short = recorder::ASR.get(cfg.asr).map_or("", |a| a.1.split(" (").next().unwrap_or(a.1));

        let mut keys = vec![
            MI::Head("Highlight AI (explain)".into()),
            act(1310, "On this PC (private, free)", cfg.explain == 0),
            act_r(1311, "Claude (cloud)", need(extras::has_key("anthropic")), cfg.explain == 1),
            act_r(1312, "Gemini (cloud)", need(gem), cfg.explain == 2),
            act(712, "Read answers aloud (Windows voice)", cfg.voice),
            MI::Sep,
        ];
        keys.extend(tx_ai());
        keys.push(MI::Sep);
        for (i, (p, name)) in [("anthropic", "Anthropic"), ("gemini", "Gemini")].into_iter().enumerate() {
            match extras::load_key(p) {
                Some(k) => {
                    keys.push(MI::Head(format!("{name} key {} · encrypted for your account", extras::mask(&k))));
                    keys.push(act(1321 + i, &format!("Remove {name} key"), false));
                }
                None => keys.push(MI::Head(format!("No {name} key stored"))),
            }
        }
        keys.push(act_r(1320, "Paste API key from clipboard", "Claude or Gemini", false));

        let mut library = Vec::new();
        for (label, kind) in [("Voice", catalog::Kind::Voice), ("Text", catalog::Kind::Text), ("Vision", catalog::Kind::Vision), ("Engines", catalog::Kind::Engine)] {
            let mut items: Vec<MI> = catalog::MODELS
                .iter()
                .enumerate()
                .filter(|(_, m)| m.kind == kind)
                .map(|(i, m)| {
                    let installed = recorder::models_dir().join(m.id).join(m.marker).exists();
                    act_r(1400 + i, &format!("{} — {}", m.name, m.about), &if installed { "installed".into() } else { format!("{} MB", m.size_mb) }, installed)
                })
                .collect();
            if items.is_empty() {
                items.push(MI::Head("Nothing here yet — meetings and notes come first".into()));
            }
            let n = items.len();
            library.push(sub(label, &if kind == catalog::Kind::Vision && n == 1 { String::new() } else { format!("{n}") }, items));
        }
        library.push(MI::Sep);
        library.push(act(1499, "Open models folder", false));

        let sounds = vec![act(706, "Hourly chime", cfg.chime), act(707, "Half-hour ding (softer)", cfg.half_chime), act(711, "Soft sounds (recording, highlights)", cfg.cues), MI::Sep, act_r(713, "Volume mixer", "Ctrl+Alt+V", false), act_r(714, "Lower music when people talk", "Discord, calls", cfg.duck), act_r(715, "Smarter voice detection (AI)", "local", cfg.duck_ai), MI::Sep, act(705, "Play the hourly chime + animation", false), act(708, "Play the half-hour ding", false)];

        let start_with_windows = if cfg.admin_mode {
            MI::Act { id: 702, label: "Start with Windows".into(), right: "via the admin task".into(), checked: true, enabled: false }
        } else {
            act(702, "Start with Windows", autostart())
        };
        let system = vec![
            act_r(717, "Show keyboard connection", "Bluetooth · cable · off", cfg.kbd_show),
            sub("Refresh rate", INTERVALS[cfg.interval].0, pick(500, &INTERVALS.iter().map(|s| s.0).collect::<Vec<_>>(), cfg.interval)),
            sub("Smoothing", SMOOTH[cfg.smooth].split(' ').next().unwrap_or(""), pick(900, &SMOOTH, cfg.smooth)),
            start_with_windows,
            act(703, "Admin mode: CPU & SATA temperatures", cfg.admin_mode),
        ];

        let mut top = vec![];
        if let Some(r) = update::available() {
            top.extend([act_r(1500, &format!("New update available ({})", r.tag), "click to install", false), MI::Sep]);
        }
        top.extend([
            act_r(1201, if recording { "Stop recording" } else { "Record & transcribe" }, "Ctrl+Alt+R", false),
            act(1204, "Recordings", false),
            act_r(1300, "Explain highlighted text", "Ctrl+Alt+E", false),
            MI::Sep,
            appearance,
            sub("Transcription", asr_short, tx),
            sub("AI & API keys", &format!("{} · {}", ["Local", "Claude", "Gemini"][cfg.explain.min(2)], if cfg.tx_cloud { "Gemini" } else { "Local" }), keys),
            sub("Sounds", "", sounds),
            sub("Model library", "", library),
            sub("System", "", system),
            MI::Sep,
            act_r(710, if self.tucked { "Bring back from the edge" } else { "Tuck into the screen edge" }, "Ctrl+Alt+M", false),
            act(1, if self.want_visible { "Hide completely" } else { "Show" }, false),
            act(999, "Quit", false),
        ]);
        top
    }

    /// The items at the current depth, and the title for the panel header.
    fn settings_level(&self) -> (String, &Vec<MI>) {
        let mut items = &self.settings_cache;
        let mut title = "Settings".to_string();
        for &i in self.settings.as_deref().unwrap_or(&[]) {
            if let Some(MI::Sub { label, items: inner, .. }) = items.get(i) {
                title = label.clone();
                items = inner;
            }
        }
        (title, items)
    }

    pub fn settings_h(&self) -> f32 {
        let (_, items) = self.settings_level();
        (TOP + items.iter().map(item_h).sum::<f32>() + 8.0).min(MAX_H)
    }

    pub fn toggle_settings(&mut self) {
        if self.settings.is_some() {
            self.settings = None;
        } else {
            self.select(NONE);
            self.music_open = false;
            self.mixer = None;
            self.settings_cache = self.settings_items();
            self.settings = Some(Vec::new());
            self.settings_scroll = 0.0;
            self.nav_t = 0.0;
        }
        self.after_rec_change();
        self.kick();
    }

    /// Row index under (x, y), with the rows' top-left y; header areas give None.
    fn settings_row(&self, y: f32, panel: R) -> Option<usize> {
        let (_, items) = self.settings_level();
        let mut ry = panel.y + TOP - self.settings_scroll;
        for (i, it) in items.iter().enumerate() {
            let h = item_h(it);
            if y >= ry && y < ry + h && y >= panel.y + TOP {
                return matches!(it, MI::Act { .. } | MI::Sub { .. }).then_some(i);
            }
            ry += h;
        }
        None
    }

    pub fn settings_hover_at(&mut self, x: f32, y: f32, panel: R) -> bool {
        let h = if panel.contains(x, y) { self.settings_row(y, panel) } else { None };
        let changed = h != self.settings_hover;
        self.settings_hover = h;
        changed
    }

    pub fn settings_click(&mut self, x: f32, y: f32, panel: R) {
        let depth = self.settings.as_ref().map_or(0, |p| p.len());
        if y < panel.y + TOP {
            if x > panel.x + panel.w - 34.0 {
                self.toggle_settings(); // ×
            } else if depth > 0 {
                if let Some(p) = self.settings.as_mut() {
                    p.pop(); // ‹ back
                }
                self.settings_scroll = 0.0;
                self.nav_t = 0.0;
                self.after_rec_change();
                self.kick();
            }
            return;
        }
        let Some(i) = self.settings_row(y, panel) else { return };
        let (_, items) = self.settings_level();
        match &items[i] {
            MI::Sub { .. } => {
                if let Some(p) = self.settings.as_mut() {
                    p.push(i);
                }
                self.settings_scroll = 0.0;
                self.settings_hover = None;
                self.nav_t = 0.0;
                self.after_rec_change();
                self.kick();
            }
            MI::Act { id, enabled: true, .. } => {
                let id = *id;
                if closes_panel(id) {
                    self.settings = None;
                }
                self.command(id as u32);
                if self.settings.is_some() {
                    self.settings_cache = self.settings_items(); // ticks and values update in place
                    unsafe { self.render() };
                }
            }
            _ => {}
        }
    }

    pub fn settings_wheel(&mut self, delta: i16, panel: R) {
        let (_, items) = self.settings_level();
        let content: f32 = items.iter().map(item_h).sum::<f32>() + 8.0;
        let room = panel.h - TOP;
        self.settings_scroll = (self.settings_scroll - delta as f32 / 120.0 * ROW * 2.0).clamp(0.0, (content - room).max(0.0));
        unsafe { self.render() };
    }

    /// Draw the panel contents; `r` is the panel's (animated) surface, `full` its final rect.
    pub fn draw_settings(&self, r: R, full: R) {
        let th = self.theme();
        let (title, items) = self.settings_level();
        let depth = self.settings.as_ref().map_or(0, |p| p.len());
        // sliding in from the side the user navigated towards
        let slide = (1.0 - self.nav_t) * 14.0;
        let x = full.x + slide;
        let w = full.w;
        let a = self.nav_t.clamp(0.0, 1.0);
        let y0 = full.y;
        // header
        let tx = if depth > 0 { x + 34.0 } else { x + 18.0 };
        if depth > 0 {
            self.text("‹", &self.f.clock_big, R { x: x + 12.0, y: y0 + 7.0, w: 20.0, h: 28.0 }, th.fg, 0.75 * a);
        }
        self.text(&title, &self.f.title, R { x: tx, y: y0 + 12.0, w: w - 80.0, h: 20.0 }, th.fg, a);
        self.text("×", &self.f.title, R { x: full.x + w - 30.0, y: y0 + 10.0, w: 20.0, h: 22.0 }, th.fg, 0.55);
        unsafe {
            self.rt.FillRectangle(&R { x: full.x + 14.0, y: y0 + TOP - 4.0, w: w - 28.0, h: 1.0 }.d2d(), self.color(th.fg, 0.07));
            self.rt.PushAxisAlignedClip(&R { x: r.x, y: y0 + TOP - 2.0, w: r.w, h: (r.y + r.h - (y0 + TOP - 2.0) - 6.0).max(0.0) }.d2d(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }
        let mut ry = y0 + TOP - self.settings_scroll;
        for (i, it) in items.iter().enumerate() {
            let h = item_h(it);
            match it {
                MI::Sep => unsafe {
                    self.rt.FillRectangle(&R { x: x + 16.0, y: ry + h / 2.0, w: w - 32.0, h: 1.0 }.d2d(), self.color(th.fg, 0.06 * a));
                },
                MI::Head(t) => self.text(t, &self.f.label, R { x: x + 18.0, y: ry + 6.0, w: w - 36.0, h: 14.0 }, th.fg, 0.42 * a),
                MI::Act { .. } | MI::Sub { .. } => {
                    let (label, right, checked, enabled, is_sub) = match it {
                        MI::Act { label, right, checked, enabled, .. } => (label, right, *checked, *enabled, false),
                        MI::Sub { label, right, .. } => (label, right, false, true, true),
                        _ => unreachable!(),
                    };
                    if self.settings_hover == Some(i) && enabled {
                        unsafe { self.rt.FillRoundedRectangle(&self.rrect(R { x: x + 8.0, y: ry + 2.0, w: w - 16.0, h: h - 4.0 }, 9.0), self.color(th.fg, 0.08 * a)) };
                    }
                    let la = if enabled { 0.92 } else { 0.4 };
                    if checked {
                        self.text("✓", &self.f.row_val, R { x: x + 14.0, y: ry + 6.0, w: 16.0, h: 18.0 }, th.accent[0], a);
                    }
                    if long_note(right) {
                        // a longer note (e.g. a speed prediction): its own line under the label
                        self.text(label, &self.f.row, R { x: x + 34.0, y: ry + 4.0, w: w - 34.0 - 34.0, h: 18.0 }, th.fg, la * a);
                        self.text(right, &self.f.small, R { x: x + 34.0, y: ry + 22.0, w: w - 34.0 - 34.0, h: 15.0 }, th.fg, 0.5 * a);
                    } else {
                        let right_w = if right.is_empty() { 0.0 } else { (w * 0.46).min(190.0) };
                        self.text(label, &self.f.row, R { x: x + 34.0, y: ry + 6.0, w: w - 34.0 - right_w - 34.0, h: 18.0 }, th.fg, la * a);
                        if !right.is_empty() {
                            self.text(right, &self.f.detail, R { x: x + w - right_w - (if is_sub { 30.0 } else { 18.0 }), y: ry + 6.0, w: right_w, h: 18.0 }, th.fg, 0.5 * a);
                        }
                    }
                    if is_sub {
                        self.text("›", &self.f.title, R { x: x + w - 26.0, y: ry + 4.0, w: 14.0, h: 20.0 }, th.fg, 0.5 * a);
                    }
                }
            }
            ry += h;
        }
        // more below (or above): a soft fade at that edge says the list scrolls
        let view_top = y0 + TOP - 2.0;
        let view_bottom = r.y + r.h - 6.0;
        let fade = |from: f32, to: f32| {
            if let Some(b) = self.lin(x, from, x, to, &[(0.0, th.bg, 0.0), (1.0, th.bg, 0.95 * a)]) {
                unsafe { self.rt.FillRectangle(&R { x: r.x, y: from.min(to), w: r.w, h: (to - from).abs() }.d2d(), &b) };
            }
        };
        if ry > view_bottom + 1.0 {
            fade(view_bottom - 26.0, view_bottom);
        }
        if self.settings_scroll > 1.0 {
            fade(view_top + 20.0, view_top);
        }
        unsafe { self.rt.PopAxisAlignedClip() };
    }
}

/// Standard menu for the tray icon, built from the same items.
pub unsafe fn native_menu(hwnd: HWND) {
    unsafe {
        let Some(items) = with(|a| a.settings_items()) else { return };
        let Ok(m) = CreatePopupMenu() else { return };
        fn fill(menu: HMENU, items: &[MI]) {
            for it in items {
                unsafe {
                    match it {
                        MI::Act { id, label, right, checked, enabled } => {
                            let mut flags = MF_STRING;
                            if *checked {
                                flags |= MF_CHECKED;
                            }
                            if !*enabled {
                                flags |= MF_GRAYED;
                            }
                            let text = if right.is_empty() { label.replace('&', "&&") } else { format!("{}\t{}", label.replace('&', "&&"), right) };
                            let _ = AppendMenuW(menu, flags, *id, &HSTRING::from(text));
                        }
                        MI::Sub { label, items, .. } => {
                            if let Ok(s) = CreatePopupMenu() {
                                fill(s, items);
                                let _ = AppendMenuW(menu, MF_POPUP, s.0 as usize, &HSTRING::from(label.replace('&', "&&")));
                            }
                        }
                        MI::Sep => {
                            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                        }
                        MI::Head(t) => {
                            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &HSTRING::from(t.replace('&', "&&")));
                        }
                    }
                }
            }
        }
        fill(m, &items);
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(hwnd);
        with(|a| a.menu_open = true); // stop re-asserting topmost so the menu stays above the pill
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, p.x, p.y, None, hwnd, None).0;
        with(|a| a.menu_open = false);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(m);
        if cmd > 0 {
            with(|a| a.command(cmd as u32));
        }
    }
}
