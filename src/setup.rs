//! Getting transcription ready, out of the box:
//! - on first start the models recording needs (voice detection, the speech model, voiceprints,
//!   speaker turns: ~570 MB with Parakeet) download one after another, with progress in the strip;
//! - the record button shows that progress as a ring; pressing it meanwhile means "record as soon
//!   as it's ready" (one press is enough);
//! - a short guide (welcome → speech model → summaries → getting ready) opens the first time,
//!   and again from Settings → Transcription → Setup guide to change models.

use crate::*;

pub const GUIDE_H: f32 = 372.0;
const STEPS: usize = 5;
/// The "where your recordings go" step (the gear on the pill pulses while it shows).
pub const RECORDINGS_STEP: u8 = 3;

/// Where each step's buttons and choices are (panel-relative layout shared by drawing and clicks).
struct Parts {
    back: R,
    next: R,
    skip: R,
    choices: Vec<R>,
}

fn parts(p: R, step: u8) -> Parts {
    let (x, w) = (p.x + 18.0, p.w - 36.0);
    let foot = p.y + p.h - 50.0;
    let n = match step {
        1 => 2,
        2 => 3,
        _ => 0,
    };
    let mut choices: Vec<R> = (0..n).map(|i| R { x, y: p.y + 92.0 + i as f32 * 74.0, w, h: 64.0 }).collect();
    if step == RECORDINGS_STEP {
        // "Change folder…" under the path, "Open Recordings now" under the how-to
        choices = vec![R { x, y: p.y + 138.0, w: 150.0, h: 32.0 }, R { x, y: p.y + 272.0, w: 190.0, h: 32.0 }];
    }
    Parts {
        back: R { x, y: foot, w: 84.0, h: 34.0 },
        next: R { x: p.x + p.w - 18.0 - 120.0, y: foot, w: 120.0, h: 34.0 },
        skip: R { x: p.x + p.w - 18.0 - 120.0 - 92.0, y: foot, w: 84.0, h: 34.0 },
        choices,
    }
}

pub fn installed(id: &str) -> bool {
    catalog::find(id).is_some_and(|m| recorder::models_dir().join(m.id).join(m.marker).exists())
}

/// MB still to download for the GPU speech engine (0: installed).
pub fn gpu_download_mb() -> u32 {
    catalog::GPU_RUNTIME.iter().filter(|id| !installed(id)).filter_map(|id| catalog::find(id)).map(|m| m.size_mb).sum()
}

impl App {
    /// What recording needs with the current choices (plus the local summary model if chosen).
    pub fn needed(&self) -> Vec<&'static str> {
        let asr = recorder::ASR[self.cfg.asr.min(recorder::ASR.len() - 1)].0;
        let mut v = vec!["vad", asr, "voiceprint", "segmentation"];
        if !self.cfg.tx_cloud {
            if let Some((dir, _, _)) = recorder::LLM.get(self.cfg.llm) {
                v.extend(["llama.cpp", "cudart"]);
                if let Some(m) = catalog::MODELS.iter().find(|m| m.id == *dir) {
                    v.push(m.id);
                }
            }
        }
        if self.use_gpu() {
            for id in catalog::GPU_RUNTIME {
                if !v.contains(&id) {
                    v.push(id);
                }
            }
        }
        v
    }

    /// The required pieces still missing (summaries are optional: recording works without them).
    pub fn missing_for_recording(&self) -> Vec<&'static str> {
        self.needed().into_iter().take(4).filter(|id| !installed(id)).collect()
    }

    pub fn recording_ready(&self) -> bool {
        self.missing_for_recording().is_empty()
    }

    /// Queue everything chosen that isn't on this PC yet, and start downloading.
    pub fn start_setup(&mut self) {
        for id in self.needed() {
            if !installed(id) && !self.setup_queue.contains(&id) && self.downloading() != Some(id) {
                self.setup_queue.push(id);
                self.setup_total += 1;
            }
        }
        self.setup_next();
    }

    /// The model being downloaded right now, if any.
    fn downloading(&self) -> Option<&'static str> {
        self.job.as_ref().filter(|j| j.kind == recorder::JobKind::Download && j.state == recorder::JobState::Running).and(self.setup_current)
    }

    pub(crate) fn setup_next(&mut self) {
        if self.job.as_ref().is_some_and(|j| j.state == recorder::JobState::Running) {
            return; // one download at a time
        }
        while let Some(id) = (!self.setup_queue.is_empty()).then(|| self.setup_queue.remove(0)) {
            if installed(id) {
                continue;
            }
            let Some(m) = catalog::find(id) else { continue };
            let done = self.setup_total - self.setup_queue.len();
            let title = format!("Getting ready · {done} of {} · {} · {} MB", self.setup_total, m.name, m.size_mb);
            self.setup_current = Some(id);
            let notify = self.job_notify();
            let args = vec!["--download".to_string(), id.to_string()];
            self.job = recorder::Job::start(recorder::JobKind::Download, title, String::new(), &args, None, notify).ok();
            self.after_rec_change();
            return;
        }
        // all done
        self.setup_current = None;
        self.setup_total = 0;
        if std::mem::take(&mut self.record_when_ready) && self.recording_ready() {
            self.job = None;
            self.toggle_record(); // the press they made while it was getting ready
        } else if self.job.as_ref().is_some_and(|j| j.kind == recorder::JobKind::Download && j.state == recorder::JobState::Done) {
            self.job = Some(recorder::Job::note(recorder::JobKind::Download, "Ready", "Transcription is ready · press ● or Ctrl+Alt+R to record"));
        }
        self.after_rec_change();
    }

    /// A download finished (or failed): on to the next one.
    pub fn setup_job_done(&mut self) {
        if self.setup_current.is_none() || self.job.as_ref().is_some_and(|j| j.state == recorder::JobState::Running) {
            return;
        }
        if self.job.as_ref().is_some_and(|j| j.state == recorder::JobState::Failed) {
            // keep the error on screen; pressing record tries again
            self.setup_queue.clear();
            self.setup_total = 0;
            self.setup_current = None;
            self.record_when_ready = false;
            return;
        }
        self.setup_next();
    }

    /// Overall progress of getting ready, 0..1 (by size), while anything is downloading.
    pub fn setup_progress(&self) -> Option<f32> {
        let cur = self.setup_current?;
        let size = |id: &str| catalog::find(id).map_or(1.0, |m| m.size_mb as f32);
        let left: f32 = self.setup_queue.iter().map(|id| size(id)).sum::<f32>() + size(cur);
        let part = self.job.as_ref().map_or(0.0, |j| if j.progress.1 > 0 { j.progress.0 as f32 / j.progress.1 as f32 } else { 0.0 });
        let total_done: f32 = self.needed().iter().filter(|id| installed(id)).map(|id| size(id)).sum();
        let all = total_done + left;
        Some(((total_done + size(cur) * part) / all.max(1.0)).clamp(0.0, 1.0))
    }

    /// Record pressed while not ready: download what's missing and record the moment it's ready.
    pub fn record_when_ready(&mut self) {
        self.record_when_ready = true;
        if self.setup_current.is_none() {
            self.start_setup();
        }
        self.after_rec_change();
    }

    pub fn open_guide(&mut self, step: u8) {
        self.select(NONE);
        self.settings = None;
        self.music_open = false;
        self.mixer = None;
        self.guide = Some(step);
        self.nav_t = 0.0;
        self.sync_pulse();
        if let Some((x, y)) = self.cfg.pos {
            self.place_at(x, y, None);
        }
        self.kick();
    }

    fn close_guide(&mut self) {
        self.guide = None;
        self.cfg.guide_done = true;
        self.cfg.save();
        self.after_rec_change();
        self.kick();
    }

    pub fn guide_click(&mut self, x: f32, y: f32, p: R) {
        let Some(step) = self.guide else { return };
        let ps = parts(p, step);
        if ps.next.contains(x, y) {
            if step as usize + 1 >= STEPS {
                self.close_guide();
            } else {
                self.guide = Some(step + 1);
                self.nav_t = 0.0;
                if step + 1 == STEPS as u8 - 1 {
                    self.start_setup(); // "Getting ready": fetch everything chosen (incl. a summary model)
                }
                self.sync_pulse(); // the gear pulses on the recordings step
                self.kick();
            }
            return;
        }
        if step > 0 && ps.back.contains(x, y) {
            self.guide = Some(step - 1);
            self.nav_t = 0.0;
            self.sync_pulse();
            self.kick();
            return;
        }
        if step == 0 && ps.skip.contains(x, y) {
            self.close_guide(); // the downloads carry on anyway
            return;
        }
        if let Some(i) = ps.choices.iter().position(|c| c.contains(x, y)) {
            match step {
                1 => self.cfg.asr = i.min(recorder::ASR.len() - 1),
                RECORDINGS_STEP => {
                    if i == 0 {
                        self.pick_recordings_folder();
                    } else {
                        recorder::open_viewer(None);
                    }
                    unsafe { self.render() };
                    return;
                }
                2 => match i {
                    0 => {
                        self.cfg.tx_cloud = false;
                        self.cfg.llm = 0; // Qwen 3.5 4B
                    }
                    1 => {
                        self.cfg.tx_cloud = true;
                        if !extras::has_key("gemini") {
                            self.command(1320); // paste a key from the clipboard (says how if there is none)
                        }
                    }
                    _ => {
                        self.cfg.tx_cloud = false;
                        self.cfg.llm = recorder::LLM.len(); // no summary
                    }
                },
                _ => {}
            }
            self.cfg.save();
            self.start_setup(); // fetch whatever the new choice needs
            unsafe { self.render() };
        }
    }

    pub fn draw_guide(&self, p: R) {
        let Some(step) = self.guide else { return };
        let th = self.theme();
        let a = self.nav_t.clamp(0.0, 1.0);
        let (x, w) = (p.x + 18.0, p.w - 36.0);
        // progress dots and title
        for i in 0..STEPS {
            let on = i as u8 == step;
            unsafe {
                self.rt.FillRoundedRectangle(&self.rrect(R { x: x + i as f32 * 14.0, y: p.y + 18.0, w: if on { 18.0 } else { 7.0 }, h: 7.0 }, 3.5), self.color(if on { th.accent[0] } else { th.fg }, if on { 1.0 } else { 0.2 }));
            }
        }
        let (title, sub) = match step {
            0 => ("Welcome to Peter", "Your PC at a glance, meetings written down, anything explained"),
            1 => ("Speech to text", "Runs on this PC: private and free. Change it any time."),
            2 => ("Meeting summaries", "Optional: key points, decisions and action items"),
            RECORDINGS_STEP => ("Your recordings", "Where they're saved, and where to find them"),
            _ => ("Getting transcription ready", "Downloading once; after this, one press records"),
        };
        self.text(title, &self.f.big, R { x, y: p.y + 34.0, w, h: 28.0 }, th.fg, a);
        self.text(sub, &self.f.small, R { x, y: p.y + 62.0, w, h: 16.0 }, th.fg, 0.55 * a);
        let ps = parts(p, step);
        let cpu = self.snap.frac[stats::CPU];
        match step {
            0 => {
                let lines = [
                    ("◔", "Live CPU, RAM, GPU, disk and network, with the top apps"),
                    ("●", "Record any meeting: who said what, plus a summary"),
                    ("✦", "Highlight any text: Peter explains or searches it"),
                    ("♫", "Music, a volume mixer, and music that dips when people talk"),
                ];
                for (i, (icon, text)) in lines.into_iter().enumerate() {
                    let ry = p.y + 98.0 + i as f32 * 40.0;
                    self.text(icon, &self.f.title, R { x, y: ry, w: 24.0, h: 24.0 }, th.accent[i % 2], a);
                    self.text(text, &self.f.row, R { x: x + 30.0, y: ry, w: w - 30.0, h: 24.0 }, th.fg, 0.9 * a);
                }
                let note = if self.recording_ready() { "Transcription is ready: press ● any time to record." } else { "Transcription is already downloading in the background." };
                self.text(note, &self.f.small, R { x, y: p.y + 266.0, w, h: 16.0 }, th.accent[0], a);
            }
            1 => {
                for (i, c) in ps.choices.iter().enumerate() {
                    let (id, name) = recorder::ASR[i];
                    let note = match i {
                        0 => format!("Recommended · fast · {} MB", catalog::find(id).map_or(0, |m| m.size_mb)),
                        _ => format!("Most accurate · heavier on the CPU · {} MB", catalog::find(id).map_or(0, |m| m.size_mb)),
                    };
                    self.choice(*c, name.split(" (").next().unwrap_or(name), &note, &recorder::asr_prediction(id, cpu), self.cfg.asr == i, a);
                }
            }
            2 => {
                let gem = extras::has_key("gemini");
                let opts = [
                    ("On this PC · Qwen 3.5 4B", "Private · ~3.2 GB · needs an NVIDIA graphics card", !self.cfg.tx_cloud && self.cfg.llm == 0),
                    ("Gemini (cloud)", if gem { "Uses your stored Google key · nothing to download" } else { "Copy your Google AI key, then choose this" }, self.cfg.tx_cloud),
                    ("No summaries", "Just the transcript", !self.cfg.tx_cloud && self.cfg.llm >= recorder::LLM.len()),
                ];
                for (c, (name, note, on)) in ps.choices.iter().zip(opts) {
                    self.choice(*c, name, note, "", on, a);
                }
            }
            RECORDINGS_STEP => {
                // where they go
                self.text("Saved to", &self.f.label, R { x, y: p.y + 90.0, w, h: 14.0 }, th.fg, 0.5 * a);
                let path = recorder::recordings_dir().display().to_string();
                let box_r = R { x, y: p.y + 106.0, w, h: 26.0 };
                unsafe {
                    self.rt.FillRoundedRectangle(&self.rrect(box_r, 8.0), self.color(th.fg, 0.06));
                }
                self.text(&path, &self.f.small, R { x: x + 10.0, y: box_r.y + 5.0, w: w - 20.0, h: 16.0 }, th.fg, 0.9 * a);
                self.small_button(ps.choices[0], "Change folder…");
                // where to find them
                self.text("Finding your transcripts", &self.f.label, R { x, y: p.y + 186.0, w, h: 14.0 }, th.fg, 0.5 * a);
                self.text("⚙  Gear on the pill (glowing) → Recordings", &self.f.small, R { x, y: p.y + 204.0, w, h: 30.0 }, th.fg, 0.9 * a);
                self.text("●  Or after a meeting, click the strip under the pill", &self.f.small, R { x, y: p.y + 240.0, w, h: 16.0 }, th.fg, 0.9 * a);
                self.small_button(ps.choices[1], "Open Recordings now");
            }
            _ => {
                // the checklist: each piece installed, downloading (with %) or waiting
                let mut ry = p.y + 96.0;
                for id in self.needed() {
                    let Some(m) = catalog::find(id) else { continue };
                    let (mark, rgb, state) = if installed(id) {
                        ("✓", th.accent[0], "ready".to_string())
                    } else if self.setup_current == Some(id) {
                        let pct = self.job.as_ref().map_or(0, |j| if j.progress.1 > 0 { j.progress.0 * 100 / j.progress.1 } else { 0 });
                        ("↓", th.accent[1], format!("{pct}%"))
                    } else {
                        ("·", th.fg, "waiting".to_string())
                    };
                    self.text(mark, &self.f.row_val, R { x, y: ry, w: 18.0, h: 22.0 }, rgb, a);
                    self.text(m.name, &self.f.row, R { x: x + 22.0, y: ry, w: w - 120.0, h: 22.0 }, th.fg, 0.9 * a);
                    self.text(&format!("{state} · {} MB", m.size_mb), &self.f.detail, R { x: x + w - 140.0, y: ry, w: 140.0, h: 22.0 }, th.fg, 0.55 * a);
                    ry += 26.0;
                }
                if let Some(f) = self.setup_progress() {
                    let bar = R { x, y: ry + 8.0, w, h: 5.0 };
                    unsafe {
                        self.rt.FillRoundedRectangle(&self.rrect(bar, 2.5), self.color(th.fg, 0.12));
                        self.rt.FillRoundedRectangle(&self.rrect(R { w: (bar.w * f).max(5.0), ..bar }, 2.5), self.color(th.accent[0], 1.0));
                    }
                }
                let msg = if self.recording_ready() { "Ready: press ● or Ctrl+Alt+R to record a meeting" } else { "Press ● any time: recording starts the moment it's ready" };
                self.text(msg, &self.f.small, R { x, y: p.y + p.h - 82.0, w, h: 16.0 }, th.accent[0], a);
            }
        }
        // buttons
        let button = |r: R, label: &str, primary: bool| {
            unsafe {
                if primary {
                    self.rt.FillRoundedRectangle(&self.rrect(r, r.h / 2.0), self.color(th.accent[0], 1.0));
                } else {
                    self.rt.DrawRoundedRectangle(&self.rrect(r, r.h / 2.0), self.color(th.fg, 0.25), 1.0, None);
                }
            }
            self.text(label, &self.f.c_value, r, if primary { 0xFFFFFF } else { th.fg }, if primary { 1.0 } else { 0.8 });
        };
        if step > 0 {
            button(ps.back, "Back", false);
        } else {
            button(ps.skip, "Skip", false);
        }
        button(ps.next, if step as usize + 1 >= STEPS { "Done" } else { "Next" }, true);
    }

    /// A small outlined button.
    fn small_button(&self, r: R, label: &str) {
        let th = self.theme();
        unsafe {
            self.rt.FillRoundedRectangle(&self.rrect(r, r.h / 2.0), self.color(th.accent[0], 0.14));
            self.rt.DrawRoundedRectangle(&self.rrect(r, r.h / 2.0), self.color(th.accent[0], 0.7), 1.0, None);
        }
        self.text(label, &self.f.c_value, r, th.fg, 0.95);
    }

    /// Windows' folder picker, starting at the current recordings folder; saves the choice.
    pub fn pick_recordings_folder(&mut self) {
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
        use windows::Win32::UI::Shell::*;
        let start = recorder::recordings_dir();
        let chosen = unsafe {
            (|| -> Option<std::path::PathBuf> {
                let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
                dlg.SetOptions(FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST).ok()?;
                let _ = dlg.SetTitle(w!("Where should Peter save recordings and transcripts?"));
                let _ = std::fs::create_dir_all(&start);
                if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start.as_os_str()), None) {
                    let _ = dlg.SetFolder(&item);
                }
                dlg.Show(Some(self.hwnd)).ok()?; // cancelled: nothing changes
                let path = dlg.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
                let s = path.to_string().ok();
                windows::Win32::System::Com::CoTaskMemFree(Some(path.0 as *const _));
                s.map(std::path::PathBuf::from)
            })()
        };
        if let Some(dir) = chosen {
            let custom = dir != recorder::default_recordings_dir();
            self.cfg.rec_dir = if custom { dir.display().to_string() } else { String::new() };
            recorder::set_recordings_dir(custom.then_some(dir));
            self.cfg.save();
        }
    }

    /// A choice card: name, note, an optional prediction for this PC, selected or not.
    fn choice(&self, c: R, name: &str, note: &str, prediction: &str, on: bool, a: f32) {
        let th = self.theme();
        unsafe {
            self.rt.FillRoundedRectangle(&self.rrect(c, 12.0), self.color(if on { th.accent[0] } else { th.fg }, if on { 0.14 } else { 0.05 }));
            if on {
                self.rt.DrawRoundedRectangle(&self.rrect(c, 12.0), self.color(th.accent[0], 0.9), 1.4, None);
            }
        }
        self.text(if on { "●" } else { "○" }, &self.f.row_val, R { x: c.x + 12.0, y: c.y + 8.0, w: 18.0, h: 20.0 }, if on { th.accent[0] } else { th.fg }, a);
        self.text(name, &self.f.title, R { x: c.x + 34.0, y: c.y + 7.0, w: c.w - 44.0, h: 20.0 }, th.fg, a);
        self.text(note, &self.f.small, R { x: c.x + 34.0, y: c.y + 28.0, w: c.w - 44.0, h: 15.0 }, th.fg, 0.6 * a);
        if !prediction.is_empty() {
            self.text(prediction, &self.f.small, R { x: c.x + 34.0, y: c.y + 44.0, w: c.w - 44.0, h: 15.0 }, th.accent[1], 0.85 * a);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn guide_parts_fit_inside() {
        let p = crate::R { x: 0.0, y: 0.0, w: 390.0, h: super::GUIDE_H };
        for step in 0..super::STEPS as u8 {
            let ps = super::parts(p, step);
            for c in &ps.choices {
                assert!(c.y + c.h < ps.next.y - 4.0, "choices sit above the buttons");
            }
            assert!(ps.skip.x + ps.skip.w < ps.next.x, "buttons don't overlap");
        }
    }
}
