// Stream Music Remover: a native front end for Bandit v2 (speech / music / sfx separation).
// The window, queue and file handling are Rust (egui). Separation runs in a long-lived Python
// worker (worker/bandit_worker.py --serve) that receives one job per line on stdin and reports
// progress as JSON lines on stdout.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(test)]
mod screens;
mod theme;

use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use theme::*;

const VIDEO_EXT: &[&str] = &["mp4", "mkv", "mov", "m4v", "flv", "ts", "webm", "avi"];
const AUDIO_EXT: &[&str] = &["wav", "mp3", "flac", "m4a", "aac", "ogg", "opus", "wma"];

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Stream Music Remover")
            .with_inner_size([880.0, 780.0])
            .with_min_inner_size([680.0, 560.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Stream Music Remover", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

// ---------- settings ----------

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
struct Settings {
    msst_dir: String,
    python: String,
    worker: String,
    model: String,
    config: String,
    out_clean: bool,
    out_video: bool,
    out_stems: bool,
    use_custom_folder: bool,
    custom_folder: String,
    segment_minutes: u32,
    multitrack: bool,
    mic_track: u32,
    game_track: u32,
    video_tracks: bool,
    /// Seconds of media processed per second, remembered from earlier runs (0 = unknown).
    learned_speed: f64,
}

impl Default for Settings {
    fn default() -> Self {
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
        // Installed layout: <MSST-WebUI>\StreamMusicRemover\Stream Music Remover.exe
        let msst = exe_dir
            .parent()
            .filter(|p| p.join("webUI.py").exists())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(r"E:\MSST-WebUI"));
        let worker = [exe_dir.join("bandit_worker.py"), PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("worker").join("bandit_worker.py")]
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| exe_dir.join("bandit_worker.py"));
        let s = |p: PathBuf| p.to_string_lossy().into_owned();
        Settings {
            python: s(msst.join(".venv").join("Scripts").join("python.exe")),
            model: s(msst.join("pretrain").join("multi_stem_models").join("bandit_v2_dnr_multi.ckpt")),
            config: s(msst.join("configs").join("multi_stem_models").join("bandit_v2_dnr_multi.ckpt.yaml")),
            worker: s(worker),
            msst_dir: s(msst),
            out_clean: true,
            out_video: true,
            out_stems: false,
            use_custom_folder: false,
            custom_folder: String::new(),
            segment_minutes: 5,
            multitrack: false,
            mic_track: 2,
            game_track: 3,
            video_tracks: true,
            learned_speed: 0.0,
        }
    }
}

impl Settings {
    fn engine_problem(&self) -> Option<String> {
        for (label, path) in [
            ("engine folder", &self.msst_dir),
            ("Python", &self.python),
            ("worker script", &self.worker),
            ("model", &self.model),
            ("model config", &self.config),
        ] {
            if !Path::new(path).exists() {
                return Some(format!("The {label} was not found ({path})."));
            }
        }
        None
    }
}

/// The options a file was (or will be) processed with, frozen when it is sent to the worker.
#[derive(Clone)]
struct JobOptions {
    multitrack: bool,
    mic_track: u32,
    game_track: u32,
}

// ---------- jobs & worker ----------

#[derive(Clone)]
enum Status {
    Waiting,
    Running { progress: f32, stage: String, started: Instant },
    Done { outputs: Vec<String>, seconds: f64 },
    Failed(String),
    Stopped,
}

#[derive(Clone)]
struct Probe {
    tracks: Vec<String>,
    duration: f64,
}

struct Job {
    path: PathBuf,
    is_video: bool,
    status: Status,
    /// Audio tracks and duration from ffprobe (None while probing, Err if the file can't be read).
    probe: Option<Result<Probe, String>>,
    note: Option<String>,
    opts: Option<JobOptions>,
    sample: Sample,
}

/// A short before/after preview of one file, made on request before the full run.
#[derive(Clone, PartialEq)]
enum Sample {
    None,
    Pending,
    Working,
    Ready { before: String, after: String },
    Failed(String),
}

impl Job {
    fn track_count(&self) -> Option<usize> {
        match &self.probe {
            Some(Ok(p)) => Some(p.tracks.len()),
            _ => None,
        }
    }
    fn duration(&self) -> Option<f64> {
        match &self.probe {
            Some(Ok(p)) if p.duration > 0.0 => Some(p.duration),
            _ => None,
        }
    }
}

fn probe_file(path: &Path) -> Result<Probe, String> {
    let mut cmd = Command::new("ffprobe");
    cmd.args(["-v", "error", "-show_entries", "format=duration:stream=codec_type:stream_tags=title,handler_name", "-of", "json"])
        .arg(path)
        .stdin(Stdio::null());
    no_window(&mut cmd);
    let out = cmd.output().map_err(|e| format!("Couldn't inspect the file ({e})."))?;
    if !out.status.success() {
        return Err("This file couldn't be read. It may be damaged or still being recorded.".into());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let tracks: Vec<String> = v["streams"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|s| s["codec_type"] == "audio")
                .map(|s| s["tags"]["title"].as_str().or(s["tags"]["handler_name"].as_str()).unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    if tracks.is_empty() {
        return Err("This file has no sound.".into());
    }
    let duration = v["format"]["duration"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0);
    Ok(Probe { tracks, duration })
}

enum Msg {
    Json(serde_json::Value),
    Stderr(String),
    Exited(Option<i32>),
}

struct Worker {
    child: Arc<Mutex<Option<Child>>>,
    stdin: Option<ChildStdin>,
    rx: Receiver<Msg>,
    ready: bool,
    idle: bool,
}

impl Worker {
    fn kill(&self) {
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }
}

fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

fn spawn_worker(settings: &Settings, ctx: egui::Context) -> Result<Worker, String> {
    let mut cmd = Command::new(&settings.python);
    cmd.arg("-u")
        .arg(&settings.worker)
        .args(["--msst-dir", &settings.msst_dir, "--model", &settings.model, "--config", &settings.config, "--serve"])
        .current_dir(&settings.msst_dir)
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("The processing engine couldn't start ({e})."))?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let child = Arc::new(Mutex::new(Some(child)));
    let (tx, rx) = channel::<Msg>();

    let tx_err: Sender<Msg> = tx.clone();
    let ctx_err = ctx.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx_err.send(Msg::Stderr(line));
            ctx_err.request_repaint();
        }
    });

    let child_out = child.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let msg = match serde_json::from_str::<serde_json::Value>(&line) {
                Ok(v) if v.get("event").is_some() => Msg::Json(v),
                _ => Msg::Stderr(line),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        }
        let code = child_out.lock().unwrap().as_mut().and_then(|c| c.wait().ok()).and_then(|s| s.code());
        let _ = tx.send(Msg::Exited(code));
        ctx.request_repaint();
    });
    Ok(Worker { child, stdin, rx, ready: false, idle: false })
}

// ---------- app ----------

#[derive(Clone)]
struct Summary {
    done: usize,
    failed: usize,
    stopped: bool,
    seconds: f64,
}

#[derive(PartialEq)]
enum Dialog {
    None,
    Settings,
    ConfirmStop,
}

struct App {
    settings: Settings,
    jobs: Vec<Job>,
    worker: Option<Worker>,
    batch_active: bool,
    batch_started: Instant,
    summary: Option<Summary>,
    /// Seconds of media processed per second of wall-clock time, learned while running.
    speed: Option<f64>,
    log: VecDeque<String>,
    stderr_tail: VecDeque<String>,
    status_line: String,
    error: Option<String>,
    ffmpeg_ok: bool,
    dialog: Dialog,
    ctx: egui::Context,
    probe_tx: Sender<(PathBuf, Result<Probe, String>)>,
    probe_rx: Receiver<(PathBuf, Result<Probe, String>)>,
    /// The user picked the audio setup themselves, so don't switch it automatically.
    mode_touched: bool,
    /// The mic-track mode was switched on because a mic track was found.
    auto_detected: bool,
    scroll_to_output: bool,
    /// File whose sample the worker is making right now.
    sample_in_flight: Option<PathBuf>,
    /// When the worker last became idle outside a batch (it is closed after a while to free the GPU).
    worker_idle_since: Option<Instant>,
    adv_open: bool,
    log_open: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::with_args(cc, std::env::args().skip(1).collect())
    }

    fn with_args(cc: &eframe::CreationContext<'_>, args: Vec<String>) -> Self {
        theme::install(&cc.egui_ctx);
        let settings = cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default();
        let ffmpeg_ok = which("ffmpeg") && which("ffprobe");
        let (probe_tx, probe_rx) = channel();
        let mut app = App {
            settings,
            jobs: Vec::new(),
            worker: None,
            batch_active: false,
            batch_started: Instant::now(),
            summary: None,
            speed: None,
            log: VecDeque::new(),
            stderr_tail: VecDeque::new(),
            status_line: String::new(),
            error: None,
            ffmpeg_ok,
            dialog: Dialog::None,
            ctx: cc.egui_ctx.clone(),
            probe_tx,
            probe_rx,
            mode_touched: false,
            auto_detected: false,
            scroll_to_output: false,
            sample_in_flight: None,
            worker_idle_since: None,
            adv_open: false,
            log_open: false,
        };
        // Files given on the command line (dropped on the .exe / shortcut, or "Open with") join the queue.
        // --start begins processing immediately; --mic-track N --game-track N switch to mic-track mode.
        let mut files = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--mic-track" | "--game-track" => {
                    if let Some(n) = it.next().and_then(|v| v.parse::<u32>().ok()).filter(|n| *n >= 1) {
                        app.settings.multitrack = true;
                        app.mode_touched = true;
                        if a == "--mic-track" {
                            app.settings.mic_track = n;
                        } else {
                            app.settings.game_track = n;
                        }
                    }
                }
                s if s.starts_with("--") => {}
                s => files.push(PathBuf::from(s)),
            }
        }
        app.add_paths(files);
        // --sample makes a before/after sample of the first file (used for testing).
        if args.iter().any(|a| a == "--sample") && !app.jobs.is_empty() {
            app.request_sample(0);
        }
        if args.iter().any(|a| a == "--start") && !app.jobs.is_empty() {
            app.start();
        }
        app
    }

    fn running(&self) -> bool {
        self.batch_active
    }

    fn log(&mut self, line: impl Into<String>) {
        self.log.push_back(line.into());
        while self.log.len() > 500 {
            self.log.pop_front();
        }
    }

    fn add_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for p in paths {
            if p.is_dir() {
                if let Ok(rd) = std::fs::read_dir(&p) {
                    let mut inner: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_file()).collect();
                    inner.sort();
                    self.add_paths(inner);
                }
                continue;
            }
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            let is_video = VIDEO_EXT.contains(&ext.as_str());
            if !(is_video || AUDIO_EXT.contains(&ext.as_str())) {
                continue;
            }
            let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if name.ends_with("(no music)") || [" - speech", " - sfx", " - music", " - mic", " - game (no music)"].iter().any(|s| name.ends_with(s)) {
                continue; // our own outputs
            }
            if !self.jobs.iter().any(|j| j.path == p) {
                let (tx, ctx, probe_path) = (self.probe_tx.clone(), self.ctx.clone(), p.clone());
                std::thread::spawn(move || {
                    let result = probe_file(&probe_path);
                    let _ = tx.send((probe_path, result));
                    ctx.request_repaint();
                });
                self.jobs.push(Job { path: p, is_video, status: Status::Waiting, probe: None, note: None, opts: None, sample: Sample::None });
                self.summary = None;
            }
        }
    }

    fn waiting_count(&self) -> usize {
        self.jobs.iter().filter(|j| matches!(j.status, Status::Waiting)).count()
    }

    fn retryable_count(&self) -> usize {
        self.jobs.iter().filter(|j| matches!(j.status, Status::Failed(_) | Status::Stopped)).count()
    }

    /// Files that will fall back to one mixed track because they have too few audio tracks.
    fn needs_fallback(&self, job: &Job) -> bool {
        self.settings.multitrack && job.track_count().is_some_and(|n| n < self.settings.mic_track.max(self.settings.game_track) as usize)
    }

    fn validate(&self) -> Result<(), String> {
        let s = &self.settings;
        if let Some(p) = s.engine_problem() {
            return Err(format!("{p} Open Settings → File locations to fix it."));
        }
        if !self.ffmpeg_ok {
            return Err("ffmpeg isn't installed. Install it (winget install Gyan.FFmpeg) and restart the app.".into());
        }
        if !(s.out_clean || s.out_video || s.out_stems) {
            return Err("Choose at least one thing to create under Output.".into());
        }
        if s.multitrack && s.mic_track == s.game_track {
            return Err("The microphone and the game audio can't be the same track.".into());
        }
        if s.use_custom_folder && s.custom_folder.trim().is_empty() {
            return Err("Choose a folder to save to, or save next to the originals.".into());
        }
        Ok(())
    }

    fn start(&mut self) {
        self.error = None;
        if let Err(e) = self.validate() {
            self.error = Some(e);
            return;
        }
        if self.waiting_count() == 0 {
            return;
        }
        self.summary = None;
        self.batch_active = true;
        self.batch_started = Instant::now();
        keep_awake(true);
        for j in &mut self.jobs {
            if j.sample == Sample::Pending {
                j.sample = Sample::None; // samples aren't made during a run
            }
        }
        if self.worker.is_none() {
            self.stderr_tail.clear();
            match spawn_worker(&self.settings, self.ctx.clone()) {
                Ok(w) => {
                    self.status_line = "Starting the engine...".into();
                    self.log("Starting the processing engine");
                    self.worker = Some(w);
                }
                Err(e) => {
                    self.error = Some(e);
                    self.batch_active = false;
                    keep_awake(false);
                }
            }
        }
    }

    fn retry_failed(&mut self) {
        for j in &mut self.jobs {
            if matches!(j.status, Status::Failed(_) | Status::Stopped) {
                j.status = Status::Waiting;
                j.note = None;
            }
        }
        self.start();
    }

    fn stop(&mut self) {
        if let Some(w) = self.worker.take() {
            w.kill();
        }
        for j in &mut self.jobs {
            if matches!(j.status, Status::Running { .. }) {
                j.status = Status::Stopped;
            }
        }
        self.finish_batch(true);
        self.log("Stopped by user");
    }

    fn finish_batch(&mut self, stopped: bool) {
        if !self.batch_active {
            return;
        }
        self.batch_active = false;
        keep_awake(false);
        self.ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        let done = self.jobs.iter().filter(|j| matches!(j.status, Status::Done { .. })).count();
        let failed = self.jobs.iter().filter(|j| matches!(j.status, Status::Failed(_))).count();
        self.summary = Some(Summary { done, failed, stopped, seconds: self.batch_started.elapsed().as_secs_f64() });
    }

    fn ensure_worker(&mut self) -> bool {
        if self.worker.is_some() {
            return true;
        }
        if let Err(e) = self.validate() {
            self.error = Some(e);
            return false;
        }
        self.stderr_tail.clear();
        match spawn_worker(&self.settings, self.ctx.clone()) {
            Ok(w) => {
                self.log("Starting the processing engine");
                self.worker = Some(w);
                true
            }
            Err(e) => {
                self.error = Some(e);
                false
            }
        }
    }

    fn request_sample(&mut self, i: usize) {
        self.error = None;
        self.jobs[i].sample = Sample::Pending;
        if !self.ensure_worker() {
            self.jobs[i].sample = Sample::None;
        }
    }

    /// Send the next sample request (outside a batch) or the next waiting file to an idle worker,
    /// or wind the batch down when nothing is left.
    fn dispatch(&mut self) {
        let Some(w) = self.worker.as_ref() else { return };
        if !(w.ready && w.idle) {
            return;
        }
        if !self.batch_active {
            if let Some(i) = self.jobs.iter().position(|j| j.sample == Sample::Pending) {
                let s = &self.settings;
                let dir = std::env::temp_dir().join("Stream Music Remover samples");
                let job = serde_json::json!({
                    "index": i,
                    "path": self.jobs[i].path.to_string_lossy(),
                    "sample": true,
                    "sample_seconds": 30,
                    "outdir": dir.to_string_lossy(),
                    "mic_track": if s.multitrack { s.mic_track } else { 0 },
                    "game_track": if s.multitrack { s.game_track } else { 0 },
                });
                let w = self.worker.as_mut().unwrap();
                if w.stdin.as_mut().map(|stdin| writeln!(stdin, "{job}").and_then(|_| stdin.flush()).is_ok()).unwrap_or(false) {
                    w.idle = false;
                    self.worker_idle_since = None;
                    self.jobs[i].sample = Sample::Working;
                    self.sample_in_flight = Some(self.jobs[i].path.clone());
                } else {
                    self.jobs[i].sample = Sample::Failed("Lost contact with the processing engine.".into());
                }
            } else if self.worker_idle_since.is_some_and(|t| t.elapsed() > Duration::from_secs(120)) {
                // Nothing to do for a while: close the engine so the graphics card memory is freed.
                if let Some(mut w) = self.worker.take() {
                    drop(w.stdin.take());
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(3));
                        w.kill();
                    });
                }
                self.worker_idle_since = None;
            }
            return;
        }
        let Some(i) = self.jobs.iter().position(|j| matches!(j.status, Status::Waiting)) else {
            // Batch complete: close the worker so the GPU memory is released.
            if let Some(mut w) = self.worker.take() {
                drop(w.stdin.take());
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(3));
                    w.kill();
                });
            }
            self.finish_batch(false);
            return;
        };
        let s = &self.settings;
        let opts = JobOptions { multitrack: s.multitrack, mic_track: s.mic_track, game_track: s.game_track };
        let job = serde_json::json!({
            "index": i,
            "path": self.jobs[i].path.to_string_lossy(),
            "outdir": if s.use_custom_folder { s.custom_folder.trim().to_string() } else { String::new() },
            "clean": s.out_clean,
            "video": s.out_video,
            "stems": s.out_stems,
            "mic_track": if s.multitrack { s.mic_track } else { 0 },
            "game_track": if s.multitrack { s.game_track } else { 0 },
            "video_tracks": s.multitrack && s.video_tracks,
            "segment_seconds": s.segment_minutes.max(1) * 60,
        });
        let w = self.worker.as_mut().unwrap();
        let sent = w.stdin.as_mut().map(|stdin| writeln!(stdin, "{job}").and_then(|_| stdin.flush()).is_ok()).unwrap_or(false);
        if sent {
            w.idle = false;
            self.jobs[i].opts = Some(opts);
            self.jobs[i].note = None;
            self.jobs[i].status = Status::Running { progress: 0.0, stage: "Starting".into(), started: Instant::now() };
        } else {
            self.error = Some("Lost contact with the processing engine.".into());
            self.stop();
        }
    }

    fn poll_probes(&mut self) {
        for (path, result) in self.probe_rx.try_iter() {
            if let Some(job) = self.jobs.iter_mut().find(|j| j.path == path) {
                job.probe = Some(result);
            }
        }
    }

    fn poll_worker(&mut self) {
        let Some(worker) = self.worker.as_ref() else { return };
        let msgs: Vec<Msg> = worker.rx.try_iter().collect();
        let mut exited: Option<Option<i32>> = None;
        for msg in msgs {
            match msg {
                Msg::Stderr(line) => {
                    if !line.trim().is_empty() {
                        self.stderr_tail.push_back(line);
                        while self.stderr_tail.len() > 40 {
                            self.stderr_tail.pop_front();
                        }
                    }
                }
                Msg::Json(v) => self.handle_event(&v),
                Msg::Exited(code) => exited = Some(code),
            }
        }
        if let Some(code) = exited {
            let unexpected = self.batch_active;
            self.worker = None;
            self.sample_in_flight = None;
            for j in &mut self.jobs {
                if matches!(j.sample, Sample::Pending | Sample::Working) {
                    j.sample = Sample::Failed("The processing engine stopped.".into());
                }
            }
            for j in &mut self.jobs {
                if matches!(j.status, Status::Running { .. }) {
                    j.status = Status::Failed("The processing engine stopped while working on this file.".into());
                }
            }
            if unexpected {
                let tail: Vec<String> = self.stderr_tail.iter().rev().take(8).rev().cloned().collect();
                for line in &tail {
                    self.log(line.clone());
                }
                if self.error.is_none() {
                    self.error = Some(format!("The processing engine stopped unexpectedly (code {}). Details are in Settings → Activity log.", code.map(|c| c.to_string()).unwrap_or("?".into())));
                }
                self.finish_batch(false);
            }
        }
    }

    fn handle_event(&mut self, v: &serde_json::Value) {
        let event = v["event"].as_str().unwrap_or("");
        let job = v["index"].as_u64().map(|i| i as usize).filter(|i| *i < self.jobs.len());
        let message = v["message"].as_str().unwrap_or("").to_string();
        match (event, job) {
            ("log", _) => self.log(message),
            ("status", _) => {
                self.log(message.clone());
                self.status_line = message;
            }
            ("ready", _) => {
                self.log(message);
                if let Some(w) = self.worker.as_mut() {
                    w.ready = true;
                }
            }
            ("idle", _) => {
                if let Some(w) = self.worker.as_mut() {
                    w.ready = true;
                    w.idle = true;
                }
                self.worker_idle_since = Some(Instant::now());
            }
            ("sample_done", _) | ("sample_error", _) => {
                let path = self.sample_in_flight.take();
                let result = if event == "sample_done" {
                    Sample::Ready { before: v["before"].as_str().unwrap_or("").to_string(), after: v["after"].as_str().unwrap_or("").to_string() }
                } else {
                    self.log(format!("Sample failed: {message}"));
                    Sample::Failed(message.clone())
                };
                if let Some(j) = self.jobs.iter_mut().find(|j| Some(&j.path) == path.as_ref()) {
                    if let Sample::Ready { after, .. } = &result {
                        open_file(after);
                    }
                    j.sample = result;
                }
            }
            ("fatal", _) => {
                self.log(message.clone());
                if let Some(d) = v["detail"].as_str() {
                    self.log(d.to_string());
                }
                self.error = Some(format!("The processing engine couldn't start: {message}"));
            }
            ("file_start", Some(j)) => {
                self.log(format!("Processing {}", file_name(&self.jobs[j].path)));
            }
            ("note", Some(j)) => {
                self.log(format!("{}: {message}", file_name(&self.jobs[j].path)));
                self.jobs[j].note = Some(message);
            }
            ("progress", Some(j)) => {
                let duration = self.jobs[j].duration();
                if let Status::Running { progress, stage, started } = &mut self.jobs[j].status {
                    *progress = v["value"].as_f64().unwrap_or(0.0) as f32;
                    *stage = v["stage"].as_str().unwrap_or("").to_string();
                    let elapsed = started.elapsed().as_secs_f64();
                    if let (Some(d), true) = (duration, *progress > 0.05 && elapsed > 2.0) {
                        let sp = d * *progress as f64 / elapsed;
                        self.set_speed(sp);
                    }
                }
            }
            ("file_done", Some(j)) => {
                let outputs: Vec<String> = v["outputs"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                let seconds = v["seconds"].as_f64().unwrap_or(0.0);
                if let (Some(d), true) = (self.jobs[j].duration(), seconds > 1.0) {
                    self.set_speed(d / seconds);
                }
                self.log(format!("Done: {} ({})", file_name(&self.jobs[j].path), fmt_secs(seconds)));
                for o in &outputs {
                    self.log(format!("   -> {o}"));
                }
                self.jobs[j].status = Status::Done { outputs, seconds };
            }
            ("file_error", Some(j)) => {
                self.log(format!("Failed: {}: {message}", file_name(&self.jobs[j].path)));
                if let Some(d) = v["detail"].as_str() {
                    self.log(d.to_string());
                }
                self.jobs[j].status = Status::Failed(message);
            }
            _ => {}
        }
    }

    /// Media seconds processed per wall-clock second: measured this session, else remembered.
    fn speed(&self) -> Option<f64> {
        self.speed.or((self.settings.learned_speed > 0.0).then_some(self.settings.learned_speed))
    }

    fn set_speed(&mut self, s: f64) {
        self.speed = Some(s);
        self.settings.learned_speed = s;
    }

    /// Seconds left for one file. Uses the same speed as the batch total, so the two always agree.
    fn job_left(&self, job: &Job) -> Option<f64> {
        let speed = self.speed();
        match &job.status {
            Status::Running { progress, started, .. } => {
                let p = *progress as f64;
                match (job.duration(), speed) {
                    (Some(d), Some(s)) => Some(d * (1.0 - p) / s.max(0.01)),
                    _ if p > 0.03 => Some(started.elapsed().as_secs_f64() / p * (1.0 - p)),
                    _ => None,
                }
            }
            Status::Waiting => Some(job.duration()? / speed?.max(0.01)),
            _ => Some(0.0),
        }
    }

    /// (mic track, game track) when a queued file looks like an OBS multi-track recording.
    fn detect_mic_layout(&self) -> Option<(u32, u32)> {
        self.jobs.iter().find_map(|j| {
            let Some(Ok(p)) = &j.probe else { return None };
            if p.tracks.len() < 2 {
                return None;
            }
            let lower: Vec<String> = p.tracks.iter().map(|t| t.to_lowercase()).collect();
            let m = lower.iter().position(|t| t.contains("mic") || t.contains("voice"))?;
            let g = lower.iter().position(|t| t.contains("game") || t.contains("desktop")).filter(|g| *g != m).or_else(|| (1..lower.len()).rev().find(|i| *i != m))?;
            Some((m as u32 + 1, g as u32 + 1))
        })
    }

    fn auto_detect(&mut self) {
        if self.mode_touched || self.running() {
            return;
        }
        match self.detect_mic_layout() {
            Some((m, g)) if !self.auto_detected => {
                self.settings.multitrack = true;
                self.settings.mic_track = m;
                self.settings.game_track = g;
                self.auto_detected = true;
            }
            None if self.auto_detected && self.jobs.iter().all(|j| j.probe.is_some()) => {
                self.settings.multitrack = false;
                self.auto_detected = false;
            }
            _ => {}
        }
    }

    /// Estimated bytes the queued files will need, per output drive: (drive, needed, free).
    fn space_check(&self) -> Vec<(String, u64, Option<u64>)> {
        let s = &self.settings;
        let mut need: Vec<(String, u64)> = Vec::new();
        for j in self.jobs.iter().filter(|j| matches!(j.status, Status::Waiting)) {
            let Some(d) = j.duration() else { continue };
            let wav = d * 48_000.0 * 2.0 * 3.0;
            let mut bytes = 0.0;
            if s.out_video && j.is_video {
                bytes += std::fs::metadata(&j.path).map(|m| m.len() as f64).unwrap_or(d * 750_000.0);
            }
            if s.out_clean || (s.out_video && !j.is_video && !s.out_stems) {
                bytes += wav;
            }
            if s.out_stems {
                bytes += 3.0 * wav;
            }
            let dir = if s.use_custom_folder && !s.custom_folder.trim().is_empty() { PathBuf::from(s.custom_folder.trim()) } else { j.path.clone() };
            let drive = drive_of(&dir);
            match need.iter_mut().find(|(k, _)| *k == drive) {
                Some(e) => e.1 += bytes as u64,
                None => need.push((drive, bytes as u64)),
            }
        }
        need.into_iter().map(|(drive, n)| {
            let free = free_space(&format!("{drive}\\"));
            (drive, n, free)
        }).collect()
    }

    /// Short description of what will be created, for the action bar.
    fn output_summary(&self) -> String {
        let s = &self.settings;
        let any_video = self.jobs.iter().any(|j| j.is_video);
        let any_audio = self.jobs.iter().any(|j| !j.is_video);
        let mut parts = Vec::new();
        if s.out_video && any_video {
            parts.push(if any_audio && !s.out_clean && !s.out_stems { "Video (a WAV for audio files)" } else { "Video" });
        }
        if s.out_clean {
            parts.push("WAV");
        }
        if s.out_stems {
            parts.push("separate parts");
        }
        if parts.is_empty() {
            return "Nothing selected to create".into();
        }
        let place = if s.use_custom_folder { "saved to your chosen folder" } else { "saved next to each recording" };
        format!("{}, {place}", parts.join(" + "))
    }

    /// (files finished in this batch, total files in this batch, seconds left if known)
    fn batch_progress(&self) -> (usize, usize, f32, Option<f64>) {
        let in_batch: Vec<&Job> = self.jobs.iter().filter(|j| !matches!(j.status, Status::Done { .. } | Status::Failed(_) | Status::Stopped) || j.opts.is_some()).collect();
        let total = in_batch.len();
        let finished = in_batch.iter().filter(|j| matches!(j.status, Status::Done { .. } | Status::Failed(_) | Status::Stopped)).count();
        // Weight by duration when known.
        let mut all = 0.0;
        let mut done = 0.0;
        let mut eta = Some(0.0);
        for j in &in_batch {
            let d = j.duration().unwrap_or(600.0);
            all += d;
            match &j.status {
                Status::Done { .. } | Status::Failed(_) | Status::Stopped => done += d,
                Status::Running { progress, .. } => done += d * *progress as f64,
                Status::Waiting => {}
            }
            eta = match (eta, self.job_left(j)) {
                (Some(a), Some(b)) => Some(a + b),
                _ => None,
            };
        }
        let frac = if all > 0.0 { (done / all) as f32 } else { 0.0 };
        (finished, total, frac, eta)
    }

    fn output_folder(&self) -> Option<String> {
        if self.settings.use_custom_folder && !self.settings.custom_folder.trim().is_empty() {
            return Some(self.settings.custom_folder.trim().to_string());
        }
        self.jobs.iter().rev().find_map(|j| match &j.status {
            Status::Done { outputs, .. } => outputs.first().cloned(),
            _ => None,
        })
    }
}

// ---------- drawing ----------

impl App {
    fn draw_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Stream Music Remover").font(semibold(22.0)).color(TEXT));
                ui.label(RichText::new("Removes background music from stream recordings. The voice and game sounds stay.").size(14.0).color(MUTED));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if secondary_button(ui, Some(ICON_SETTINGS), "Settings", true).clicked() {
                    self.dialog = Dialog::Settings;
                }
            });
        });
    }

    fn draw_recordings(&mut self, ui: &mut egui::Ui) {
        let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        card(ui, |ui| {
            ui.horizontal(|ui| {
                section_title(ui, "Recordings");
                if !self.jobs.is_empty() {
                    ui.label(RichText::new(format!("{}", self.jobs.len())).size(13.0).color(MUTED));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !self.jobs.is_empty() {
                        let finished = self.jobs.iter().any(|j| matches!(j.status, Status::Done { .. }));
                        if secondary_button(ui, Some(ICON_ADD), "Add files", true).clicked() {
                            self.browse_files();
                        }
                        if !self.running() && ghost_button(ui, None, "Clear list").on_hover_text("Empties this list. Your files are not deleted.").clicked() {
                            self.jobs.clear();
                            self.summary = None;
                            self.auto_detected = false;
                        }
                        if finished && ghost_button(ui, None, "Remove finished").on_hover_text("Takes finished files off this list. Nothing is deleted.").clicked() {
                            self.jobs.retain(|j| !matches!(j.status, Status::Done { .. }));
                        }
                    }
                });
            });
            ui.add_space(6.0);

            if self.jobs.is_empty() {
                self.drop_zone(ui, hovering, 200.0);
                return;
            }

            let mut remove: Option<usize> = None;
            let mut retry: Option<usize> = None;
            for i in 0..self.jobs.len() {
                let action = self.file_row(ui, i);
                match action {
                    RowAction::Remove => remove = Some(i),
                    RowAction::Retry => retry = Some(i),
                    RowAction::Sample => self.request_sample(i),
                    RowAction::Details => {
                        self.log_open = true;
                        self.dialog = Dialog::Settings;
                    }
                    RowAction::None => {}
                }
                ui.add_space(6.0);
            }
            if let Some(i) = remove {
                self.jobs.remove(i);
            }
            if let Some(i) = retry {
                self.jobs[i].status = Status::Waiting;
                self.jobs[i].note = None;
                self.start();
            }
            self.drop_strip(ui, hovering);
        });
    }

    fn browse_files(&mut self) {
        let mut exts: Vec<&str> = VIDEO_EXT.to_vec();
        exts.extend_from_slice(AUDIO_EXT);
        if let Some(files) = rfd::FileDialog::new().set_title("Choose stream recordings").add_filter("Video and audio", &exts).pick_files() {
            self.add_paths(files);
        }
    }

    fn drop_zone(&mut self, ui: &mut egui::Ui, hovering: bool, height: f32) {
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        let stroke_color = if hovering { ACCENT } else { BORDER_STRONG };
        ui.painter().rect_filled(rect, 10.0, if hovering { ACCENT_TINT } else { SURFACE_2 });
        dashed_rect(ui.painter(), rect.shrink(1.0), egui::Stroke::new(1.5, stroke_color), 10.0);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(16.0)).layout(egui::Layout::top_down(egui::Align::Center)));
        child.add_space(((height - 32.0 - 140.0) / 2.0).max(0.0));
        child.label(icon(ICON_DROP, 40.0, if hovering { ACCENT } else { MUTED }));
        child.add_space(6.0);
        child.label(RichText::new(if hovering { "Drop to add" } else { "Drag your stream recordings here" }).font(semibold(18.0)).color(TEXT));
        child.add_space(2.0);
        child.label(RichText::new("Videos (MP4, MKV, MOV...) or audio (WAV, MP3...). Folders work too.").size(13.5).color(MUTED));
        child.add_space(10.0);
        if primary_button(&mut child, Some(ICON_FOLDER), "Browse files", true).clicked() {
            self.browse_files();
        }
    }

    fn drop_strip(&mut self, ui: &mut egui::Ui, hovering: bool) {
        let width = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 40.0), egui::Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Add more recordings"));
        let stroke_color = if hovering || resp.hovered() { ACCENT } else { BORDER_STRONG };
        if hovering {
            ui.painter().rect_filled(rect, 8.0, ACCENT_TINT);
        }
        dashed_rect(ui.painter(), rect.shrink(1.0), egui::Stroke::new(1.2, stroke_color), 8.0);
        let text = if hovering { "Drop to add" } else { "Drop more recordings here, or click to browse" };
        let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(13.5), if hovering { ACCENT } else { MUTED });
        let icon_g = ui.painter().layout_no_wrap(ICON_ADD.to_string(), icon_font(13.0), if hovering { ACCENT } else { MUTED });
        let total = icon_g.size().x + 8.0 + galley.size().x;
        let x = rect.center().x - total / 2.0;
        ui.painter().galley(egui::pos2(x, rect.center().y - icon_g.size().y / 2.0), icon_g.clone(), MUTED);
        ui.painter().galley(egui::pos2(x + icon_g.size().x + 8.0, rect.center().y - galley.size().y / 2.0), galley, MUTED);
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            self.browse_files();
        }
    }

    fn file_row(&mut self, ui: &mut egui::Ui, i: usize) -> RowAction {
        let mut action = RowAction::None;
        let running = self.running();
        let fallback = self.needs_fallback(&self.jobs[i]);
        let job = &self.jobs[i];
        let failed = matches!(job.status, Status::Failed(_));
        let active = matches!(job.status, Status::Running { .. });
        let fill = SURFACE_2;
        let row_left = self.job_left(job);
        let frame = egui::Frame::new().fill(fill).corner_radius(8.0).inner_margin(egui::Margin { left: 12, right: 12, top: 10, bottom: 10 });
        let resp = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // Type badge
                let (badge_color, glyph) = if job.is_video { (ACCENT, ICON_VIDEO) } else { (GOOD, ICON_AUDIO) };
                let (r, _) = ui.allocate_exact_size(egui::vec2(38.0, 38.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 8.0, badge_color.gamma_multiply(0.16));
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, glyph, icon_font(18.0), badge_color);
                ui.add_space(4.0);

                // Status area on the right is laid out first so the name can truncate.
                let status_width = 320.0;
                let name_width = (ui.available_width() - status_width - 8.0).max(120.0);
                ui.allocate_ui_with_layout(egui::vec2(name_width, 40.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(name_width);
                    ui.add(egui::Label::new(RichText::new(file_name(&job.path)).font(semibold(15.0)).color(TEXT)).truncate()).on_hover_text(job.path.to_string_lossy());
                    ui.add(egui::Label::new(RichText::new(meta_line(job)).size(13.0).color(MUTED)).truncate());
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match &job.status {
                        Status::Waiting => {
                            if icon_button(ui, ICON_CLOSE, "Remove from list").clicked() {
                                action = RowAction::Remove;
                            }
                            if running {
                                pill(ui, "Waiting", MUTED, SURFACE_3);
                            } else {
                                match &job.sample {
                                    Sample::Ready { before, after } => {
                                        if ghost_button(ui, Some(ICON_PLAY), "After").on_hover_text("Play the sample with the music removed").clicked() {
                                            open_file(after);
                                        }
                                        if ghost_button(ui, Some(ICON_PLAY), "Before").on_hover_text("Play the same 30 seconds from the original").clicked() {
                                            open_file(before);
                                        }
                                    }
                                    Sample::Pending | Sample::Working => {
                                        ui.add(egui::Spinner::new().size(14.0).color(MUTED));
                                        ui.label(RichText::new("Making a sample...").size(13.5).color(MUTED));
                                    }
                                    _ => {
                                        let busy = self.jobs.iter().any(|j| matches!(j.sample, Sample::Pending | Sample::Working));
                                        if ui.add_enabled_ui(!busy && job.probe.as_ref().is_some_and(|p| p.is_ok()), |ui| ghost_button(ui, Some(ICON_HEADPHONES), "Listen to a sample")).inner.on_hover_text("Removes the music from 30 seconds of this file so you can hear the result first").clicked() {
                                            action = RowAction::Sample;
                                        }
                                    }
                                }
                            }
                        }
                        Status::Running { progress, stage, started } => {
                            ui.vertical(|ui| {
                                ui.set_width(300.0);
                                let _ = started;
                                let pct = (*progress * 100.0).round() as i32;
                                let left = match row_left {
                                    Some(l) if *progress > 0.0 => format!(" · {} left", fmt_eta(l)),
                                    _ => String::new(),
                                };
                                let stage = if stage == "Writing video" { "Saving video".to_string() } else if *progress <= 0.0 { "Starting".to_string() } else { "Removing music".to_string() };
                                ui.label(RichText::new(format!("{stage} · {pct}%{left}")).size(13.0).color(ACCENT));
                                progress_bar(ui, *progress, 300.0);
                            });
                        }
                        Status::Done { outputs, seconds } => {
                            if secondary_button(ui, Some(ICON_FOLDER), "Show", true).on_hover_text(outputs.join("\n")).clicked() {
                                if let Some(first) = outputs.first() {
                                    reveal(first);
                                }
                            }
                            ui.label(RichText::new(format!("Done in {}", fmt_secs(*seconds))).size(13.5).color(GOOD));
                            ui.label(icon(ICON_CHECK, 15.0, GOOD));
                        }
                        Status::Failed(_) => {
                            if !running && icon_button(ui, ICON_CLOSE, "Remove from list").clicked() {
                                action = RowAction::Remove;
                            }
                            if secondary_button(ui, Some(ICON_RETRY), "Try again", true).clicked() {
                                action = RowAction::Retry;
                            }
                            if ghost_button(ui, None, "Details").on_hover_text("Show the activity log").clicked() {
                                action = RowAction::Details;
                            }
                            ui.label(RichText::new("Failed").size(13.5).color(BAD));
                            ui.label(icon(ICON_ERROR, 15.0, BAD));
                        }
                        Status::Stopped => {
                            if !running && icon_button(ui, ICON_CLOSE, "Remove from list").clicked() {
                                action = RowAction::Remove;
                            }
                            if secondary_button(ui, Some(ICON_RETRY), "Start again", true).clicked() {
                                action = RowAction::Retry;
                            }
                            ui.label(RichText::new("Stopped").size(13.5).color(MUTED));
                        }
                    }
                });
            });

            // Extra lines: warnings, notes and errors in readable text.
            let indent = 50.0;
            if let Status::Failed(msg) = &job.status {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(indent);
                    ui.label(RichText::new(friendly_error(msg, job.opts.as_ref(), &drive_of(&job.path))).size(13.5).color(BAD_TEXT));
                });
            } else if let Some(note) = &job.note {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(indent);
                    ui.label(icon(ICON_INFO, 13.0, MUTED));
                    ui.label(RichText::new(note).size(13.5).color(MUTED));
                });
            } else if let (Sample::Failed(e), Status::Waiting) = (&job.sample, &job.status) {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(indent);
                    ui.label(RichText::new(format!("The sample couldn't be made: {e}")).size(13.5).color(BAD_TEXT));
                });
            } else if fallback && matches!(job.status, Status::Waiting) {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(indent);
                    ui.label(icon(ICON_INFO, 13.0, MUTED));
                    ui.label(RichText::new("One audio track, so it's processed as a mixed track. That's fine.").size(13.5).color(MUTED));
                });
            } else if let Some(Err(e)) = &job.probe {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(indent);
                    ui.label(RichText::new(e).size(13.5).color(BAD_TEXT));
                });
            }
        });
        if failed || active {
            let r = resp.response.rect;
            ui.painter().rect_filled(egui::Rect::from_min_max(r.left_top(), egui::pos2(r.left() + 3.0, r.bottom())), egui::CornerRadius { nw: 8, sw: 8, ne: 0, se: 0 }, if failed { BAD } else { ACCENT });
        }
        action
    }

    fn draw_audio_setup(&mut self, ui: &mut egui::Ui) {
        let running = self.running();
        let titles = self.sample_track_titles();
        let recommended = self.detect_mic_layout().is_some();
        card(ui, |ui| {
            section_header(ui, "Audio setup", running);
            ui.add_space(6.0);
            ui.add_enabled_ui(!running, |ui| {
                let half = (ui.available_width() - 10.0) / 2.0;
                ui.horizontal_top(|ui| {
                    if choice_card(ui, half, !self.settings.multitrack, ICON_AUDIO, "Everything on one track", "Voice, game and music are mixed together. This is most recordings.", None).clicked() {
                        self.settings.multitrack = false;
                        self.mode_touched = true;
                    }
                    ui.add_space(2.0);
                    if choice_card(ui, half, self.settings.multitrack, ICON_MIC, "Mic on its own track", "The streamer's mic was recorded on a separate track (OBS multi-track). Cleaner results.", recommended.then_some("Recommended")).clicked() {
                        self.settings.multitrack = true;
                        self.mode_touched = true;
                    }
                });
                if self.settings.multitrack && self.auto_detected && !self.mode_touched {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(icon(ICON_CHECK, 13.0, GOOD));
                        ui.label(RichText::new("We found a separate mic track in your recordings and picked the tracks for you.").size(13.5).color(MUTED));
                    });
                }
                if self.settings.multitrack {
                    ui.add_space(10.0);
                    egui::Grid::new("track_pickers").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        track_picker(ui, "Streamer's microphone", "mic_track", &mut self.settings.mic_track, &titles);
                        ui.end_row();
                        track_picker(ui, "Game audio (with music)", "game_track", &mut self.settings.game_track, &titles);
                        ui.end_row();
                    });
                    ui.add_space(4.0);
                    ui.scope(|ui| {
                        ui.set_max_width(520.0);
                        ui.add(egui::Label::new(RichText::new("For game audio, pick the track with the game and the music but not your mic. Guests and voice chat on that track are kept.").size(13.5).color(MUTED)).wrap());
                    });
                    if self.settings.mic_track == self.settings.game_track {
                        ui.label(RichText::new("The microphone and the game audio need to be different tracks.").size(13.5).color(BAD_TEXT));
                    }
                }
            });
        });
    }

    fn draw_output(&mut self, ui: &mut egui::Ui) {
        let running = self.running();
        let multi = self.settings.multitrack;
        let scroll = std::mem::take(&mut self.scroll_to_output);
        let r = ui.scope(|ui| card(ui, |ui| {
            section_header(ui, "Output", running);
            ui.add_space(6.0);
            ui.add_enabled_ui(!running, |ui| {
                option_row(ui, &mut self.settings.out_video, "Video without music", "A copy of each video with the music removed. The picture is copied as-is, with no quality loss.");
                if multi && self.settings.out_video {
                    ui.horizontal(|ui| {
                        ui.add_space(28.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            checkbox(ui, &mut self.settings.video_tracks, RichText::new("Also put the mic and the game audio on their own tracks").size(14.0).color(TEXT));
                            ui.add_space(1.0);
                            ui.horizontal(|ui| {
                                ui.add_space(28.0);
                                ui.label(RichText::new("Handy for adjusting levels later in Premiere.").size(13.0).color(MUTED));
                            });
                        });
                    });
                    ui.add_space(10.0);
                }
                option_row(ui, &mut self.settings.out_clean, "Audio without music (WAV)", if multi { "The mic and the game sounds together in one WAV file." } else { "The voice and the game sounds together in one WAV file." });
                option_row(
                    ui,
                    &mut self.settings.out_stems,
                    "Separate parts for your own mix",
                    if multi { "The mic, the game audio and the music as three separate WAV files." } else { "The voice, the game sounds and the music as three separate WAV files." },
                );

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_sized([56.0, 38.0], egui::Label::new(RichText::new("Save to").font(semibold(14.5)).color(TEXT)));
                    ui.add_space(4.0);
                    let mut idx = self.settings.use_custom_folder as usize;
                    if segmented(ui, "save_to", &mut idx, &["Next to each recording", "Another folder"]) {
                        self.settings.use_custom_folder = idx == 1;
                    }
                });
                if self.settings.use_custom_folder {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(64.0);
                        let shown = if self.settings.custom_folder.trim().is_empty() { "No folder chosen yet".to_string() } else { middle_truncate(&self.settings.custom_folder, 60) };
                        ui.label(icon(ICON_FOLDER, 14.0, MUTED));
                        ui.label(RichText::new(shown).size(13.5).color(if self.settings.custom_folder.trim().is_empty() { WARN } else { TEXT })).on_hover_text(&self.settings.custom_folder);
                        if secondary_button(ui, None, "Choose...", true).clicked() {
                            if let Some(dir) = rfd::FileDialog::new().set_title("Save files to").pick_folder() {
                                self.settings.custom_folder = dir.to_string_lossy().into_owned();
                            }
                        }
                    });
                }

                // Preview of what gets created.
                ui.add_space(8.0);
                let preview = self.preview_names();
                let more = self.jobs.len().saturating_sub(1);
                egui::Frame::new().fill(SURFACE_2).corner_radius(8.0).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let line = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
                        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| add(ui));
                    };
                    let first = if self.jobs.is_empty() { "For each recording, these new files are made:".to_string() } else { format!("For {}, these new files are made:", middle_truncate(&file_name(&self.jobs[0].path), 60)) };
                    line(ui, &mut |ui| {
                        ui.label(RichText::new(&first).size(13.5).color(MUTED));
                    });
                    if preview.is_empty() {
                        line(ui, &mut |ui| {
                            ui.label(RichText::new("Nothing yet. Choose at least one option above.").size(13.5).color(WARN));
                        });
                    }
                    let extra_tracks = self.settings.multitrack && self.settings.video_tracks;
                    for name in &preview {
                        line(ui, &mut |ui| {
                            ui.add_space(6.0);
                            ui.label(icon(ICON_FILE, 13.0, MUTED));
                            ui.label(RichText::new(name).size(13.5).color(TEXT));
                            if extra_tracks && !name.ends_with(".wav") {
                                ui.label(RichText::new("(+ separate mic and game tracks)").size(13.0).color(MUTED));
                            }
                        });
                    }
                    if more > 0 {
                        line(ui, &mut |ui| {
                            ui.label(RichText::new(format!("...and the same for the other {more} file{}.", plural(more))).size(13.5).color(MUTED));
                        });
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("Your original recordings are never changed.").size(13.5).color(MUTED));
                });
            });
        })).response;
        if scroll {
            ui.scroll_to_rect(r.rect, Some(egui::Align::Center));
        }
    }

    fn preview_names(&self) -> Vec<String> {
        let sample = self.jobs.first();
        let (base, ext, is_video) = match sample {
            Some(j) => (
                j.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                j.path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default(),
                j.is_video,
            ),
            None => ("Recording".into(), "mp4".into(), true),
        };
        let base = middle_truncate(&base, 54);
        let s = &self.settings;
        let mut v = Vec::new();
        if s.out_video && is_video {
            let ext = if ["mp4", "mkv", "mov", "m4v", "webm"].contains(&ext.as_str()) { ext } else { "mkv".into() };
            v.push(format!("{base} (no music).{ext}"));
        }
        if s.out_clean || (s.out_video && !is_video && !s.out_stems) {
            v.push(format!("{base} (no music).wav"));
        }
        if s.out_stems {
            if s.multitrack {
                v.push(format!("{base} - mic / game / music.wav"));
            } else {
                v.push(format!("{base} - speech / sfx / music.wav"));
            }
        }
        v
    }

    /// Track titles of the first queued file that has more than one audio track, for labelling the pickers.
    fn sample_track_titles(&self) -> Vec<String> {
        self.jobs
            .iter()
            .filter_map(|j| match &j.probe {
                Some(Ok(p)) if p.tracks.len() > 1 => Some(p.tracks.clone()),
                _ => None,
            })
            .next()
            .unwrap_or_default()
    }

    fn draw_action_bar(&mut self, ui: &mut egui::Ui) {
        let waiting = self.waiting_count();
        let retryable = self.retryable_count();
        ui.horizontal(|ui| {
            ui.set_min_height(48.0);
            let right_width = if self.running() { 130.0 } else { 260.0 };
            let left_width = (ui.available_width() - right_width).max(200.0);
            ui.allocate_ui_with_layout(egui::vec2(left_width, 48.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(left_width - 16.0);
                if let Some(err) = &self.error {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(icon(ICON_ERROR, 15.0, BAD));
                        ui.label(RichText::new(err).size(13.5).color(BAD_TEXT));
                    });
                } else if self.running() {
                    let (finished, total, frac, eta) = self.batch_progress();
                    let current = (finished + 1).min(total.max(1));
                    let starting = self.worker.as_ref().is_some_and(|w| !w.ready);
                    let text = if starting {
                        "Starting the engine...".to_string()
                    } else {
                        match eta {
                            Some(s) => format!("File {current} of {total} · about {} left in total", fmt_eta(s)),
                            None => format!("File {current} of {total} · working out the time left..."),
                        }
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(text).size(14.0).color(TEXT));
                        ui.label(RichText::new("· Your PC won't go to sleep until it's done").size(13.0).color(MUTED));
                    });
                    ui.horizontal(|ui| {
                        let w = ui.available_width() - 48.0;
                        progress_bar(ui, frac, w);
                        ui.label(RichText::new(format!("{}%", (frac * 100.0).round() as i32)).size(13.0).color(MUTED));
                    });
                } else if let Some(sum) = &self.summary {
                    let (glyph, color, text) = if sum.stopped {
                        (ICON_INFO, MUTED, format!("Stopped. {} finished file{} kept.", sum.done, plural(sum.done)))
                    } else if sum.failed == 0 {
                        (ICON_CHECK, GOOD, format!("All done! {} file{} finished in {}.", sum.done, plural(sum.done), fmt_secs(sum.seconds)))
                    } else {
                        (ICON_ERROR, BAD, format!("{} done · {} failed", sum.done, sum.failed))
                    };
                    ui.horizontal(|ui| {
                        ui.label(icon(glyph, 16.0, color));
                        ui.label(RichText::new(text).font(semibold(14.5)).color(color));
                    });
                    if waiting > 0 {
                        ui.label(RichText::new(format!("{waiting} more file{} ready to go.", plural(waiting))).size(13.5).color(MUTED));
                    }
                } else if self.jobs.is_empty() {
                    ui.label(RichText::new("Add recordings to get started.").size(14.0).color(MUTED));
                } else {
                    let total_secs: Option<f64> = self.jobs.iter().filter(|j| matches!(j.status, Status::Waiting)).map(|j| self.job_left(j)).sum();
                    let time = match total_secs {
                        Some(t) if t > 0.0 => format!(" · takes about {}", fmt_eta(t)),
                        _ => String::new(),
                    };
                    ui.label(RichText::new(format!("{waiting} file{} ready{time}", plural(waiting))).size(14.0).color(TEXT));
                    let short = self.space_check().into_iter().find(|(_, need, free)| free.is_some_and(|f| *need > f));
                    ui.horizontal(|ui| {
                        match short {
                            Some((drive, need, free)) => {
                                ui.label(icon(ICON_ERROR, 13.0, WARN));
                                ui.label(RichText::new(format!("Not enough space on {drive}: needs about {}, {} free", fmt_bytes(need), fmt_bytes(free.unwrap_or(0)))).size(13.5).color(WARN));
                            }
                            None => {
                                ui.label(RichText::new(self.output_summary()).size(13.5).color(MUTED));
                            }
                        }
                        if link_button(ui, "Change").on_hover_text("Go to the Output options").clicked() {
                            self.scroll_to_output = true;
                        }
                    });
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.running() {
                    if secondary_button(ui, Some(ICON_STOP), "Stop", true).clicked() {
                        self.dialog = Dialog::ConfirmStop;
                    }
                } else if waiting > 0 {
                    let label = format!("Remove music from {waiting} file{}", plural(waiting));
                    if big_primary_button(ui, &label, true).clicked() {
                        self.start();
                    }
                } else if retryable > 0 {
                    let label = if retryable == 1 { "Retry failed file".to_string() } else { format!("Retry {retryable} failed files") };
                    if big_primary_button(ui, &label, true).clicked() {
                        self.retry_failed();
                    }
                    if let Some(folder) = self.output_folder() {
                        if secondary_button(ui, Some(ICON_FOLDER), "Open output folder", true).clicked() {
                            reveal(&folder);
                        }
                    }
                } else if self.summary.is_some() && self.output_folder().is_some() {
                    if big_primary_button(ui, "Open output folder", true).clicked() {
                        reveal(&self.output_folder().unwrap());
                    }
                } else {
                    big_primary_button(ui, "Remove music", false).on_disabled_hover_text("Add some recordings first.");
                }
            });
        });
    }

    fn draw_dialogs(&mut self, ctx: &egui::Context) {
        match self.dialog {
            Dialog::None => {}
            Dialog::ConfirmStop => {
                let mut close = false;
                let modal = egui::Modal::new(egui::Id::unique("confirm_stop")).frame(dialog_frame()).show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.label(RichText::new("Stop processing?").font(semibold(18.0)).color(TEXT));
                    ui.add_space(6.0);
                    let current = self.jobs.iter().find_map(|j| match &j.status {
                        Status::Running { progress, .. } => Some(format!("{} is {}% done and will have to start over.", file_name(&j.path), (progress * 100.0).round() as i32)),
                        _ => None,
                    });
                    ui.label(RichText::new(current.unwrap_or_else(|| "The file being worked on now will have to start over.".into())).size(14.0).color(TEXT));
                    ui.add_space(2.0);
                    ui.label(RichText::new("Files that are already done are kept.").size(14.0).color(MUTED));
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if danger_button(ui, "Stop").clicked() {
                                self.stop();
                                close = true;
                            }
                            if secondary_button(ui, None, "Keep going", true).clicked() {
                                close = true;
                            }
                        });
                    });
                });
                if close || modal.should_close() {
                    self.dialog = Dialog::None;
                }
            }
            Dialog::Settings => {
                let mut close = false;
                let modal = egui::Modal::new(egui::Id::unique("settings")).frame(dialog_frame()).show(ctx, |ui| {
                    ui.set_width(600.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Settings").font(semibold(20.0)).color(TEXT));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon_button(ui, ICON_CLOSE, "Close").clicked() {
                                close = true;
                            }
                        });
                    });
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| self.settings_body(ui));
                    ui.add_space(6.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let done = egui::Button::new(RichText::new("Done").font(semibold(15.0)).color(egui::Color32::WHITE)).fill(ACCENT).corner_radius(8.0).min_size(egui::vec2(120.0, 40.0));
                        if ui.add(done).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            close = true;
                        }
                    });
                });
                if close || modal.should_close() {
                    self.dialog = Dialog::None;
                }
            }
        }
    }

    fn settings_body(&mut self, ui: &mut egui::Ui) {
        let running = self.running();
        // Engine status
        let problem = self.settings.engine_problem().or_else(|| (!self.ffmpeg_ok).then(|| "ffmpeg isn't installed (winget install Gyan.FFmpeg).".to_string()));
        egui::Frame::new().fill(SURFACE_2).corner_radius(8.0).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| match &problem {
                None => {
                    ui.label(icon(ICON_CHECK, 16.0, GOOD));
                    ui.label(RichText::new("Ready to go").font(semibold(14.5)).color(GOOD));
                    ui.label(RichText::new("Everything the app needs is installed.").size(13.5).color(MUTED));
                }
                Some(p) => {
                    ui.label(icon(ICON_ERROR, 16.0, BAD));
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Something the app needs is missing").font(semibold(14.5)).color(BAD));
                        ui.label(RichText::new(p).size(13.5).color(BAD_TEXT));
                    });
                }
            });
        });
        ui.add_space(12.0);

        ui.add_enabled_ui(!running, |ui| {
            ui.label(RichText::new("Memory use").font(semibold(14.5)).color(TEXT));
            ui.label(RichText::new("Leave this on Balanced. Pick Low memory only if processing fails with a memory error.").size(13.5).color(MUTED));
            ui.add_space(4.0);
            const STEPS: [u32; 3] = [2, 5, 10];
            let mut idx = match self.settings.segment_minutes {
                0..=3 => 0,
                4..=7 => 1,
                _ => 2,
            };
            if segmented(ui, "memory", &mut idx, &["Low memory", "Balanced", "Faster"]) {
                self.settings.segment_minutes = STEPS[idx];
            }
            ui.add_space(14.0);

            if problem.is_some() {
                self.adv_open = true;
            }
            if expander_row(ui, "File locations (advanced)", "Only needed if you moved the app's files", &mut self.adv_open) {
                ui.label(RichText::new("Only change these if you moved the app's files. Red means the file wasn't found.").size(13.5).color(MUTED));
                ui.add_space(4.0);
                path_row(ui, "Main folder", &mut self.settings.msst_dir, true);
                path_row(ui, "Python", &mut self.settings.python, false);
                path_row(ui, "Worker script", &mut self.settings.worker, false);
                path_row(ui, "Model file", &mut self.settings.model, false);
                path_row(ui, "Model settings", &mut self.settings.config, false);
                if link_button(ui, "Reset to defaults").clicked() {
                    let keep = self.settings.clone();
                    self.settings = Settings { multitrack: keep.multitrack, mic_track: keep.mic_track, game_track: keep.game_track, learned_speed: keep.learned_speed, ..Settings::default() };
                }
                ui.add_space(4.0);
            }
        });
        ui.add_space(6.0);
        if expander_row(ui, "Activity log", "What the app has done, useful if something goes wrong", &mut self.log_open) {
            egui::Frame::new().fill(SURFACE_0).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(180.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
                    if self.log.is_empty() {
                        ui.label(RichText::new("Nothing yet.").size(13.0).color(MUTED));
                    }
                    for line in &self.log {
                        ui.label(RichText::new(line).monospace().size(12.5).color(MUTED));
                    }
                });
            });
        }
    }
}

enum RowAction {
    None,
    Remove,
    Retry,
    Sample,
    Details,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_probes();
        self.auto_detect();
        self.poll_worker();
        self.dispatch();

        let dropped: Vec<PathBuf> = ui.ctx().input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if !dropped.is_empty() {
            self.add_paths(dropped);
        }

        egui::Panel::bottom("action_bar")
            .frame(egui::Frame::new().fill(SURFACE_1).stroke(egui::Stroke::new(1.0, BORDER)).inner_margin(egui::Margin::symmetric(24, 14)))
            .show(ui, |ui| self.draw_action_bar(ui));

        egui::CentralPanel::default_margins().frame(egui::Frame::new().fill(BG)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                egui::Frame::new().inner_margin(egui::Margin { left: 24, right: 24, top: 20, bottom: 20 }).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.draw_header(ui);
                    ui.add_space(16.0);
                    self.draw_recordings(ui);
                    ui.add_space(14.0);
                    self.draw_audio_setup(ui);
                    ui.add_space(14.0);
                    self.draw_output(ui);
                });
            });
        });

        let ctx = ui.ctx().clone();
        self.draw_dialogs(&ctx);

        if self.running() {
            ui.ctx().request_repaint_after(Duration::from_millis(500)); // keep times ticking
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
    }

    fn on_exit(&mut self) {
        if let Some(w) = self.worker.take() {
            w.kill();
        }
    }
}

// ---------- helpers ----------

fn meta_line(job: &Job) -> String {
    let kind = if job.is_video { "Video" } else { "Audio" };
    match &job.probe {
        None => format!("{kind} · checking..."),
        Some(Err(_)) => format!("{kind} · couldn't be read"),
        Some(Ok(p)) => {
            let mut s = format!("{kind} · {}", fmt_duration(p.duration));
            if p.tracks.len() > 1 {
                let named: Vec<String> = p
                    .tracks
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let generic = t.is_empty() || t.eq_ignore_ascii_case(&format!("Track {}", i + 1));
                        match (i, generic) {
                            (0, true) => "Track 1 (full mix)".to_string(),
                            (_, true) => format!("Track {}", i + 1),
                            _ => t.clone(),
                        }
                    })
                    .collect();
                s.push_str(&format!(" · {} audio tracks: {}", p.tracks.len(), named.join(", ")));
            } else {
                s.push_str(" · 1 audio track");
            }
            s
        }
    }
}

fn friendly_error(msg: &str, opts: Option<&JobOptions>, drive: &str) -> String {
    let mode = match opts {
        Some(o) if o.multitrack => format!(" (processed with mic on track {} and game audio on track {})", o.mic_track, o.game_track),
        _ => String::new(),
    };
    let lower = msg.to_lowercase();
    let main = if lower.contains("no space") || lower.contains("disk full") {
        format!("Drive {drive} is full. Free up some space on it and try again.")
    } else if lower.contains("permission") || lower.contains("access is denied") {
        "The file or folder couldn't be written to. Check that it isn't open in another program.".to_string()
    } else if lower.contains("out of memory") || lower.contains("cuda") && lower.contains("memory") {
        "The graphics card ran out of memory. Choose Low memory in Settings → Memory use and try again.".to_string()
    } else {
        msg.to_string()
    };
    format!("{main}{mode}")
}

fn track_picker(ui: &mut egui::Ui, label: &str, id: &str, value: &mut u32, titles: &[String]) {
    let name = |n: u32| -> String {
        let base = match titles.get(n as usize - 1).filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case(&format!("Track {n}"))) {
            Some(t) => format!("Track {n}: {t}"),
            None => format!("Track {n}"),
        };
        if n == 1 && titles.len() > 1 { format!("{base} (usually the full stream mix)") } else { base }
    };
    let label_id = ui.label(RichText::new(label).size(14.0).color(TEXT)).id;
    egui::ComboBox::from_id_salt(id)
        .selected_text(RichText::new(name(*value)).size(14.0))
        .width(340.0)
        .icon(chevron_icon)
        .show_ui(ui, |ui| {
            let count = titles.len().max(6) as u32;
            for n in 1..=count {
                ui.selectable_value(value, n, name(n));
            }
        })
        .response
        .labelled_by(label_id);
}

fn path_row(ui: &mut egui::Ui, label: &str, value: &mut String, folder: bool) {
    let exists = Path::new(value.as_str()).exists();
    ui.horizontal(|ui| {
        let label_id = ui.add_sized([120.0, 24.0], egui::Label::new(RichText::new(label).size(13.5).color(MUTED))).id;
        let field_width = (ui.available_width() - 44.0).max(120.0);
        ui.add(egui::TextEdit::singleline(value).desired_width(field_width).text_color(if exists { TEXT } else { BAD_TEXT })).labelled_by(label_id);
        if icon_button(ui, ICON_FOLDER, "Browse").clicked() {
            let picked = if folder { rfd::FileDialog::new().pick_folder() } else { rfd::FileDialog::new().pick_file() };
            if let Some(p) = picked {
                *value = p.to_string_lossy().into_owned();
            }
        }
    });
}

/// "E:" for a path on drive E (or the path's first component).
fn drive_of(p: &Path) -> String {
    match p.components().next() {
        Some(std::path::Component::Prefix(pre)) => pre.as_os_str().to_string_lossy().into_owned(),
        _ => "the output drive".into(),
    }
}

/// Stop Windows from going to sleep while a batch runs.
fn keep_awake(on: bool) {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn SetThreadExecutionState(flags: u32) -> u32;
        }
        const ES_CONTINUOUS: u32 = 0x8000_0000;
        const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
        unsafe { SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS }) };
    }
    #[cfg(not(windows))]
    let _ = on;
}

/// Open a file with its default program (e.g. play a WAV).
fn open_file(path: &str) {
    #[cfg(windows)]
    {
        let _ = Command::new("explorer").arg(path.replace('/', "\\")).spawn();
    }
    #[cfg(not(windows))]
    let _ = path;
}

fn free_space(dir: &str) -> Option<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDiskFreeSpaceExW(dir: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
        }
        let w: Vec<u16> = std::ffi::OsStr::new(dir).encode_wide().chain(Some(0)).collect();
        let mut avail = 0u64;
        let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, std::ptr::null_mut(), std::ptr::null_mut()) };
        (ok != 0).then_some(avail)
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        None
    }
}

fn fmt_bytes(b: u64) -> String {
    let gb = b as f64 / 1e9;
    if gb >= 10.0 { format!("{:.0} GB", gb) } else if gb >= 1.0 { format!("{:.1} GB", gb) } else { format!("{:.0} MB", (b as f64 / 1e6).max(1.0)) }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn fmt_secs(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// Rounded, friendlier time-left text.
fn fmt_eta(s: f64) -> String {
    let s = s.max(0.0);
    if s < 60.0 {
        "under a minute".into()
    } else if s < 3600.0 {
        format!("{} min", (s / 60.0).round() as u64)
    } else {
        let m = (s / 60.0).round() as u64;
        format!("{}h {:02}m", m / 60, m % 60)
    }
}

fn fmt_duration(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{} min", (s + 30) / 60)
    } else {
        format!("{s} sec")
    }
}

fn middle_truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let head = max / 2 - 1;
    let tail = max - head - 3;
    format!("{}...{}", chars[..head].iter().collect::<String>(), chars[chars.len() - tail..].iter().collect::<String>())
}

fn reveal(path: &str) {
    #[cfg(windows)]
    {
        let p = path.replace('/', "\\");
        if Path::new(&p).is_dir() {
            let _ = Command::new("explorer").arg(&p).spawn();
        } else {
            let _ = Command::new("explorer").arg(format!("/select,{p}")).spawn();
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

fn which(program: &str) -> bool {
    let mut cmd = Command::new(program);
    cmd.arg("-version").stdout(Stdio::null()).stderr(Stdio::null());
    no_window(&mut cmd);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}
