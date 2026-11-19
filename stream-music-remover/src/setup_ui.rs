//! The setup screen: shown instead of the main window until the audio engine is installed.
//! One button starts it; the rest is progress, a clear error if something goes wrong, and "Start".

use super::*;
use setup::{Engine, Gpu};

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum StepState {
    Pending,
    Active,
    Done,
}

pub(crate) enum SetupPhase {
    Intro,
    Running,
    Failed(String),
    Done { device: String, gpu: bool },
}

pub(crate) struct SetupUi {
    /// Folder the engine is installed to.
    pub root: PathBuf,
    pub gpu: Gpu,
    pub phase: SetupPhase,
    /// State and byte progress (done, total) of each entry of `setup::STEPS`.
    pub steps: Vec<(StepState, Option<(u64, u64)>)>,
    pub handle: Option<setup::Handle>,
    pub log: VecDeque<String>,
    pub show_log: bool,
    /// The app already works (setup was opened from Settings), so there is a way back.
    pub can_go_back: bool,
}

impl SetupUi {
    pub(crate) fn new(root: PathBuf, gpu: Gpu, can_go_back: bool) -> Self {
        SetupUi { root, gpu, phase: SetupPhase::Intro, steps: vec![(StepState::Pending, None); setup::STEPS.len()], handle: None, log: VecDeque::new(), show_log: false, can_go_back }
    }

    pub(crate) fn apply(&mut self, msg: setup::Msg) {
        let index = |id: &str| setup::STEPS.iter().position(|(step, _)| *step == id);
        match msg {
            setup::Msg::Step { id, done } => {
                if let Some(i) = index(&id) {
                    for earlier in &mut self.steps[..i] {
                        earlier.0 = StepState::Done;
                    }
                    self.steps[i].0 = if done { StepState::Done } else { StepState::Active };
                }
            }
            setup::Msg::Progress { id, done, total } => {
                if let Some(i) = index(&id) {
                    self.steps[i].1 = Some((done, total));
                }
            }
            setup::Msg::Log(line) => {
                self.log.push_back(line);
                while self.log.len() > 400 {
                    self.log.pop_front();
                }
            }
            setup::Msg::Failed(e) => {
                self.handle = None;
                // Cancelling is the user's own choice, not an error to report.
                self.phase = if e == setup::CANCELLED { SetupPhase::Intro } else { SetupPhase::Failed(e) };
            }
            setup::Msg::Finished { device, gpu } => {
                self.handle = None;
                for step in &mut self.steps {
                    step.0 = StepState::Done;
                }
                self.phase = SetupPhase::Done { device, gpu };
            }
        }
    }
}

enum Action {
    None,
    Start,
    Cancel,
    Finish,
    Back,
    ChooseFolder,
}

impl App {
    /// Folder of the engine that setup made, or would make.
    fn engine_root(&self) -> PathBuf {
        let saved = self.settings.engine_root.trim();
        if saved.is_empty() { Engine::default_root() } else { PathBuf::from(saved) }
    }

    /// Decide what the app runs on: the engine from setup if it is there, else an installation
    /// made by hand if its saved locations still work, else show the setup screen.
    pub(crate) fn choose_engine(&mut self, force_setup: bool) {
        let engine = Engine::new(self.engine_root());
        let by_hand = self.settings.engine_root.trim().is_empty() && self.settings.engine_problem().is_none();
        if force_setup {
            self.open_setup(by_hand || engine.is_ready());
        } else if by_hand {
            self.ffmpeg_ok = which("ffmpeg") && which("ffprobe");
        } else if engine.is_ready() {
            self.use_engine(&engine);
        } else {
            self.open_setup(false);
        }
    }

    pub(crate) fn open_setup(&mut self, can_go_back: bool) {
        self.setup = Some(SetupUi::new(self.engine_root(), setup::detect_gpu(), can_go_back));
        self.dialog = Dialog::None;
    }

    /// Point the app at a finished engine.
    fn use_engine(&mut self, engine: &Engine) {
        if let Err(e) = engine.write_files() {
            self.log(format!("Couldn't refresh the engine's scripts: {e}"));
        }
        let s = |p: PathBuf| p.to_string_lossy().into_owned();
        self.settings.engine_root = s(engine.root.clone());
        self.settings.msst_dir = s(engine.msst());
        self.settings.python = s(engine.python());
        self.settings.worker = s(engine.worker());
        self.settings.model = s(engine.model());
        self.settings.config = s(engine.config());
        *FFMPEG_DIR.write().unwrap() = Some(engine.ffmpeg_dir());
        self.ffmpeg_ok = which("ffmpeg") && which("ffprobe");
        self.error = None;
    }

    pub(crate) fn poll_setup(&mut self) {
        let Some(s) = self.setup.as_mut() else { return };
        let msgs: Vec<setup::Msg> = s.handle.as_ref().map(|h| h.rx.try_iter().collect()).unwrap_or_default();
        for msg in msgs {
            s.apply(msg);
        }
        if s.handle.is_none() {
            keep_awake(false);
        }
    }

    pub(crate) fn cancel_setup(&mut self) {
        if let Some(handle) = self.setup.as_ref().and_then(|s| s.handle.as_ref()) {
            handle.cancel();
        }
    }

    pub(crate) fn draw_setup(&mut self, ui: &mut egui::Ui) {
        let Some(s) = self.setup.as_mut() else { return };
        let mut action = Action::None;
        egui::CentralPanel::default_margins().frame(egui::Frame::new().fill(BG)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let width = (ui.available_width() - 48.0).min(620.0);
                let side = ((ui.available_width() - width) / 2.0).max(0.0);
                ui.add_space(28.0);
                ui.horizontal(|ui| {
                    ui.add_space(side);
                    ui.vertical(|ui| {
                        ui.set_width(width);
                        ui.label(RichText::new("Stream Music Remover").font(semibold(22.0)).color(TEXT));
                        ui.label(RichText::new("Removes background music from stream recordings. The voice and game sounds stay.").size(14.0).color(MUTED));
                        ui.add_space(16.0);
                        // Taken out while drawing so the drawing code can change the rest of the state.
                        let phase = std::mem::replace(&mut s.phase, SetupPhase::Intro);
                        card(ui, |ui| match &phase {
                            SetupPhase::Intro => action = draw_intro(ui, s),
                            SetupPhase::Running => action = draw_running(ui, s),
                            SetupPhase::Failed(error) => action = draw_failed(ui, s, error),
                            SetupPhase::Done { device, gpu } => action = draw_done(ui, s, device, *gpu),
                        });
                        s.phase = phase;
                        if s.show_log && !matches!(s.phase, SetupPhase::Intro | SetupPhase::Done { .. }) {
                            ui.add_space(12.0);
                            draw_log(ui, s);
                        }
                        ui.add_space(20.0);
                    });
                });
            });
        });

        match action {
            Action::None => {}
            Action::Start => {
                s.steps = vec![(StepState::Pending, None); setup::STEPS.len()];
                s.phase = SetupPhase::Running;
                let ctx = self.ctx.clone();
                s.handle = Some(setup::start(Engine::new(s.root.clone()), s.gpu.backend(), move || ctx.request_repaint()));
                keep_awake(true);
            }
            Action::Cancel => self.cancel_setup(),
            Action::ChooseFolder => {
                if let Some(dir) = rfd::FileDialog::new().set_title("Install the audio engine in").pick_folder() {
                    // A folder of its own, so that removing the engine later is just deleting that folder.
                    s.root = if dir.file_name().is_some_and(|n| n.eq_ignore_ascii_case("engine")) { dir } else { dir.join("StreamMusicRemover").join("engine") };
                }
            }
            Action::Back => self.setup = None,
            Action::Finish => {
                let engine = Engine::new(s.root.clone());
                self.setup = None;
                self.use_engine(&engine);
            }
        }
    }
}

/// One line of the "what to expect" list: an icon and wrapped text.
fn fact(ui: &mut egui::Ui, glyph: &str, color: egui::Color32, text: &str, text_color: egui::Color32) {
    ui.horizontal_top(|ui| {
        ui.add_sized([20.0, 20.0], egui::Label::new(icon(glyph, 14.0, color)));
        ui.add(egui::Label::new(RichText::new(text).size(14.0).color(text_color)).wrap());
    });
    ui.add_space(4.0);
}

fn draw_intro(ui: &mut egui::Ui, s: &SetupUi) -> Action {
    let mut action = Action::None;
    let backend = s.gpu.backend();
    ui.label(RichText::new("One-time setup").font(semibold(19.0)).color(TEXT));
    ui.add_space(4.0);
    ui.add(egui::Label::new(RichText::new("Before its first use, the app downloads and installs its audio engine. Click the button and it does the rest. You only do this once.").size(14.5).color(TEXT)).wrap());
    ui.add_space(12.0);

    fact(ui, ICON_DROP, MUTED, &format!("Downloads about {:.1} GB. On a fast connection that takes 5 to 15 minutes.", backend.download_gb()), TEXT);
    match &s.gpu.name {
        Some(name) if !s.gpu.driver_too_old() => fact(ui, ICON_CHECK, GOOD, &format!("Your graphics card ({name}) will be used, so removing music is fast."), TEXT),
        Some(name) => fact(ui, ICON_ERROR, WARN, &format!("The driver of your graphics card ({name}) is too old to be used, so a one-hour recording can take four hours or more. Update the driver from nvidia.com first to make it fast."), WARN),
        None => fact(ui, ICON_INFO, WARN, "No NVIDIA graphics card was found. The app will work, but slowly: a one-hour recording can take four hours or more.", WARN),
    }

    // While it runs, setup also holds the downloads it unpacks (the model and ffmpeg archives).
    let needed = backend.installed_gb() + 1.0;
    let drive = drive_of(&s.root);
    let free = free_space(&format!("{drive}\\")).map(|b| b as f64 / 1e9);
    ui.horizontal_top(|ui| {
        ui.add_sized([20.0, 20.0], egui::Label::new(icon(ICON_FOLDER, 14.0, MUTED)));
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add(egui::Label::new(RichText::new(format!("Takes about {:.1} GB of disk space. Installed in:", backend.installed_gb())).size(14.0).color(TEXT)).wrap());
            ui.horizontal(|ui| {
                ui.label(RichText::new(middle_truncate(&s.root.to_string_lossy(), 58)).size(13.5).color(MUTED)).on_hover_text(s.root.to_string_lossy());
                if link_button(ui, "Change").on_hover_text("Install on another drive or in another folder").clicked() {
                    action = Action::ChooseFolder;
                }
            });
        });
    });
    let short = free.filter(|f| *f < needed);
    if let Some(free) = short {
        ui.add_space(4.0);
        fact(ui, ICON_ERROR, WARN, &format!("Drive {drive} has {free:.0} GB free, and setup needs about {needed:.0} GB while it runs. Free up space or choose another folder."), WARN);
    }

    ui.add_space(14.0);
    ui.horizontal(|ui| {
        if big_primary_button(ui, "Set up now", short.is_none()).on_disabled_hover_text("There isn't enough free space on that drive.").clicked() {
            action = Action::Start;
        }
        if s.can_go_back && ghost_button(ui, None, "Back").clicked() {
            action = Action::Back;
        }
    });
    action
}

/// The list of steps. `stopped` is true after a failure: the step that was running is then the one that failed.
fn draw_steps(ui: &mut egui::Ui, s: &SetupUi, stopped: bool) {
    for ((id, label), (state, progress)) in setup::STEPS.iter().zip(&s.steps) {
        ui.horizontal(|ui| {
            ui.set_min_height(26.0);
            match state {
                StepState::Done => {
                    ui.add_sized([20.0, 20.0], egui::Label::new(icon(ICON_CHECK, 14.0, GOOD)));
                }
                StepState::Active if stopped => {
                    ui.add_sized([20.0, 20.0], egui::Label::new(icon(ICON_ERROR, 14.0, BAD)));
                }
                StepState::Active => {
                    ui.add_sized([20.0, 20.0], egui::Spinner::new().size(14.0).color(ACCENT));
                }
                StepState::Pending => {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
                    ui.painter().circle_stroke(r.center(), 5.0, egui::Stroke::new(1.5, BORDER_STRONG));
                }
            }
            let color = match state {
                StepState::Pending => MUTED,
                _ => TEXT,
            };
            ui.label(RichText::new(*label).size(14.5).color(color));
            if let (StepState::Active, Some((done, total))) = (state, progress)
                && *total > 0
            {
                let about = if *id == "packages" { "about " } else { "" };
                ui.label(RichText::new(format!("· {} of {about}{}", fmt_bytes(*done), fmt_bytes(*total))).size(13.0).color(MUTED));
            }
        });
        if let (StepState::Active, Some((done, total))) = (state, progress)
            && *total > 0
            && !stopped
        {
            ui.horizontal(|ui| {
                ui.add_space(28.0);
                progress_bar(ui, *done as f32 / *total as f32, (ui.available_width() - 8.0).max(60.0));
            });
            ui.add_space(4.0);
        }
    }
}

fn draw_running(ui: &mut egui::Ui, s: &mut SetupUi) -> Action {
    let mut action = Action::None;
    ui.label(RichText::new("Setting up...").font(semibold(19.0)).color(TEXT));
    ui.add_space(2.0);
    ui.add(egui::Label::new(RichText::new("You can keep using your PC. Leave this window open until it's done.").size(14.0).color(MUTED)).wrap());
    ui.add_space(12.0);
    draw_steps(ui, s, false);
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if secondary_button(ui, None, "Cancel", true).on_hover_text("Stops the setup. What is already downloaded is kept, so it carries on from there next time.").clicked() {
            action = Action::Cancel;
        }
        if link_button(ui, if s.show_log { "Hide details" } else { "Show details" }).clicked() {
            s.show_log = !s.show_log;
        }
    });
    action
}

fn draw_failed(ui: &mut egui::Ui, s: &mut SetupUi, error: &str) -> Action {
    let mut action = Action::None;
    ui.horizontal(|ui| {
        ui.label(icon(ICON_ERROR, 17.0, BAD));
        ui.label(RichText::new("Setup didn't finish").font(semibold(19.0)).color(TEXT));
    });
    ui.add_space(4.0);
    ui.add(egui::Label::new(RichText::new(error).size(14.5).color(BAD_TEXT)).wrap());
    ui.add_space(2.0);
    ui.add(egui::Label::new(RichText::new("Nothing is lost: trying again carries on where it stopped.").size(14.0).color(MUTED)).wrap());
    ui.add_space(12.0);
    draw_steps(ui, s, true);
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if big_primary_button(ui, "Try again", true).clicked() {
            action = Action::Start;
        }
        if link_button(ui, if s.show_log { "Hide details" } else { "Show details" }).clicked() {
            s.show_log = !s.show_log;
        }
        if s.can_go_back && ghost_button(ui, None, "Back").clicked() {
            action = Action::Back;
        }
    });
    action
}

fn draw_done(ui: &mut egui::Ui, _s: &SetupUi, device: &str, gpu: bool) -> Action {
    let mut action = Action::None;
    ui.horizontal(|ui| {
        ui.label(icon(ICON_CHECK, 18.0, GOOD));
        ui.label(RichText::new("All set").font(semibold(19.0)).color(TEXT));
    });
    ui.add_space(4.0);
    if gpu {
        ui.add(egui::Label::new(RichText::new(format!("The audio engine is installed and tested. It runs on your {device}.")).size(14.5).color(TEXT)).wrap());
    } else {
        ui.add(egui::Label::new(RichText::new("The audio engine is installed and tested.").size(14.5).color(TEXT)).wrap());
        ui.add_space(4.0);
        fact(ui, ICON_INFO, WARN, "It runs on the processor, not on a graphics card, so it is slow: a one-hour recording can take four hours or more.", WARN);
    }
    ui.add_space(14.0);
    if big_primary_button(ui, "Start", true).clicked() {
        action = Action::Finish;
    }
    action
}

fn draw_log(ui: &mut egui::Ui, s: &SetupUi) {
    egui::Frame::new().fill(SURFACE_0).corner_radius(8.0).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().id_salt("setup_log").max_height(200.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
            if s.log.is_empty() {
                ui.label(RichText::new("Nothing yet.").size(13.0).color(MUTED));
            }
            for line in &s.log {
                ui.add(egui::Label::new(RichText::new(line).monospace().size(12.0).color(MUTED)).wrap());
            }
        });
        ui.add_space(4.0);
        ui.label(RichText::new(format!("Also saved as setup.log in {}", middle_truncate(&s.root.to_string_lossy(), 60))).size(12.5).color(MUTED));
    });
}
