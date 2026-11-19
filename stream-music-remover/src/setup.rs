//! One-time setup of the audio engine: everything the app needs besides its own .exe.
//!
//! The engine lives in one folder (by default %LOCALAPPDATA%\StreamMusicRemover\engine):
//!
//!   uv\uv.exe                  installer tool, downloaded first
//!   python\, venv\             a private Python with PyTorch and the other packages
//!   msst\                      the Bandit v2 model code
//!   model\                     the model weights
//!   ffmpeg\                    ffmpeg.exe and ffprobe.exe
//!   bandit_worker.py ...       the scripts built into this .exe, written out on every start
//!   engine.json                written last: its presence means setup finished
//!
//! Nothing outside that folder is touched (no PATH changes, no registry, no admin rights), so
//! deleting the folder undoes the setup. Steps that are already done are skipped, so a failed or
//! cancelled setup carries on where it stopped.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WORKER_PY: &str = include_str!("../worker/bandit_worker.py");
const SETUP_PY: &str = include_str!("../worker/setup_engine.py");
const MODEL_CONFIG: &str = include_str!("../worker/bandit_v2_dnr_multi.yaml");
const REQUIREMENTS: &str = include_str!("../worker/requirements.txt");

const UV_URL: &str = "https://github.com/astral-sh/uv/releases/download/0.13.9/uv-x86_64-pc-windows-msvc.zip";
const UV_SHA256: &str = "edd61f7f5728a56cc62455386661860bdfc7bab65872f39b0c25bdcc1d8e1beb";
const UV_SIZE: u64 = 15_567_875;
const PYTHON_VERSION: &str = "3.10";
/// Raised when an existing engine can't be used by a newer app, so setup runs again.
const ENGINE_VERSION: u64 = 1;
const USER_AGENT: &str = "StreamMusicRemover-setup";
pub const CANCELLED: &str = "Setup was cancelled.";

/// The steps shown to the user, in order: (id, label).
pub const STEPS: &[(&str, &str)] = &[
    ("tools", "Getting ready"),
    ("packages", "Downloading the audio engine"),
    ("ffmpeg", "Downloading the video tools"),
    ("model", "Downloading the music-removal model"),
    ("test", "Checking that everything works"),
];

// ---------- where things are ----------

#[derive(Clone)]
pub struct Engine {
    pub root: PathBuf,
}

impl Engine {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Engine { root: root.into() }
    }

    pub fn default_root() -> PathBuf {
        match std::env::var_os("LOCALAPPDATA") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("StreamMusicRemover").join("engine"),
            _ => std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default().join("engine"),
        }
    }

    pub fn python(&self) -> PathBuf {
        self.root.join("venv").join("Scripts").join("python.exe")
    }
    pub fn worker(&self) -> PathBuf {
        self.root.join("bandit_worker.py")
    }
    pub fn msst(&self) -> PathBuf {
        self.root.join("msst")
    }
    pub fn model(&self) -> PathBuf {
        self.root.join("model").join("bandit_v2_dnr_multi.ckpt")
    }
    pub fn config(&self) -> PathBuf {
        self.root.join("bandit_v2_dnr_multi.yaml")
    }
    pub fn ffmpeg_dir(&self) -> PathBuf {
        self.root.join("ffmpeg")
    }
    fn uv(&self) -> PathBuf {
        self.root.join("uv").join("uv.exe")
    }
    fn marker(&self) -> PathBuf {
        self.root.join("engine.json")
    }

    /// Setup has finished here, for this version of the app, and the main pieces are still in place.
    pub fn is_ready(&self) -> bool {
        let marker: Option<serde_json::Value> = fs::read(self.marker()).ok().and_then(|b| serde_json::from_slice(&b).ok());
        marker.is_some_and(|m| m["version"].as_u64() == Some(ENGINE_VERSION))
            && [self.python(), self.model(), self.msst().join("utils").join("utils.py"), self.ffmpeg_dir().join("ffmpeg.exe"), self.ffmpeg_dir().join("ffprobe.exe")]
                .iter()
                .all(|p| p.exists())
    }

    /// Write out the scripts and settings that are built into the app, so the engine always runs
    /// the worker that belongs to this .exe.
    pub fn write_files(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.root)?;
        for (path, text) in [
            (self.worker(), WORKER_PY),
            (self.root.join("setup_engine.py"), SETUP_PY),
            (self.config(), MODEL_CONFIG),
            (self.root.join("requirements.txt"), REQUIREMENTS),
        ] {
            if fs::read(&path).ok().as_deref() != Some(text.as_bytes()) {
                fs::write(&path, text)?;
            }
        }
        Ok(())
    }
}

// ---------- graphics card ----------

/// Which build of PyTorch to install.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Backend {
    Cu128,
    Cu126,
    Cu118,
    Cpu,
}

impl Backend {
    fn name(self) -> &'static str {
        match self {
            Backend::Cu128 => "cu128",
            Backend::Cu126 => "cu126",
            Backend::Cu118 => "cu118",
            Backend::Cpu => "cpu",
        }
    }

    pub fn from_name(name: &str) -> Option<Backend> {
        [Backend::Cu128, Backend::Cu126, Backend::Cu118, Backend::Cpu].into_iter().find(|b| b.name() == name)
    }

    // The sizes below were measured on real installs of the cu128 and cpu builds. The cu126 and
    // cu118 figures are scaled from the size of their PyTorch download.

    /// Size of everything that gets downloaded, in GB, for telling the user up front.
    pub fn download_gb(self) -> f64 {
        match self {
            Backend::Cu128 => 4.1,
            Backend::Cu126 => 3.5,
            Backend::Cu118 => 3.6,
            Backend::Cpu => 1.0,
        }
    }

    /// Disk space the finished engine takes, in GB.
    pub fn installed_gb(self) -> f64 {
        match self {
            Backend::Cu128 => 6.3,
            Backend::Cu126 => 5.4,
            Backend::Cu118 => 5.6,
            Backend::Cpu => 2.0,
        }
    }

    /// Bytes the package cache holds once every package is downloaded and unpacked (for progress).
    fn packages_bytes(self) -> u64 {
        match self {
            Backend::Cu128 => 5_900_000_000,
            Backend::Cu126 => 5_000_000_000,
            Backend::Cu118 => 5_200_000_000,
            Backend::Cpu => 1_500_000_000,
        }
    }
}

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Gpu {
    /// Name of the NVIDIA graphics card, if there is one.
    pub name: Option<String>,
    /// Its driver version, e.g. 566.36.
    pub driver: Option<f64>,
}

impl Gpu {
    /// The newest PyTorch build this card's driver can run. PyTorch only uses NVIDIA cards on Windows.
    pub fn backend(&self) -> Backend {
        match (&self.name, self.driver) {
            (None, _) | (_, None) => Backend::Cpu,
            (_, Some(d)) if d >= 570.65 => Backend::Cu128, // GTX 10 series up to RTX 50 series
            (_, Some(d)) if d >= 560.76 => Backend::Cu126,
            (_, Some(d)) if d >= 522.06 => Backend::Cu118,
            _ => Backend::Cpu,
        }
    }

    /// There is an NVIDIA card, but its driver is too old for any PyTorch build we install.
    pub fn driver_too_old(&self) -> bool {
        self.name.is_some() && self.backend() == Backend::Cpu
    }
}

/// Parse the output of `nvidia-smi --query-gpu=name,driver_version --format=csv,noheader`.
pub fn parse_gpu(csv: &str) -> Gpu {
    let Some(line) = csv.lines().map(str::trim).find(|l| !l.is_empty()) else { return Gpu::default() };
    let Some((name, driver)) = line.rsplit_once(',') else { return Gpu::default() };
    let name = name.trim();
    if name.is_empty() {
        return Gpu::default();
    }
    Gpu { name: Some(name.to_string()), driver: driver.trim().parse().ok() }
}

pub fn detect_gpu() -> Gpu {
    let mut cmd = Command::new("nvidia-smi");
    cmd.args(["--query-gpu=name,driver_version", "--format=csv,noheader"]).stdin(Stdio::null()).stderr(Stdio::null());
    crate::no_window(&mut cmd);
    match cmd.output() {
        Ok(out) if out.status.success() => parse_gpu(&String::from_utf8_lossy(&out.stdout)),
        _ => Gpu::default(),
    }
}

// ---------- running the setup ----------

pub enum Msg {
    /// A step started (`done: false`) or finished.
    Step { id: String, done: bool },
    Progress { id: String, done: u64, total: u64 },
    Log(String),
    Failed(String),
    Finished { device: String, gpu: bool },
}

pub struct Handle {
    pub rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Handle {
    /// Stop the setup. What is already downloaded is kept for the next attempt.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            crate::kill_tree(child);
        }
    }
}

struct Run {
    engine: Engine,
    backend: Backend,
    tx: Mutex<Sender<Msg>>,
    notify: Box<dyn Fn() + Send + Sync>,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    log_file: Mutex<Option<fs::File>>,
}

/// Start the setup on its own thread. `notify` is called after every message (to repaint the window).
pub fn start(engine: Engine, backend: Backend, notify: impl Fn() + Send + Sync + 'static) -> Handle {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let child = Arc::new(Mutex::new(None));
    let run = Arc::new(Run { engine, backend, tx: Mutex::new(tx), notify: Box::new(notify), cancel: cancel.clone(), child: child.clone(), log_file: Mutex::new(None) });
    std::thread::spawn(move || {
        let result = run.clone().run();
        match result {
            Ok((device, gpu)) => {
                run.log(&format!("Setup finished. Running on {device}."));
                run.send(Msg::Finished { device, gpu });
            }
            Err(e) => {
                run.log(&format!("Setup stopped: {e}"));
                run.send(Msg::Failed(e));
            }
        }
    });
    Handle { rx, cancel, child }
}

impl Run {
    fn send(&self, msg: Msg) {
        let _ = self.tx.lock().unwrap().send(msg);
        (self.notify)();
    }

    /// Goes to the window's details view and to setup.log in the engine folder.
    fn log(&self, line: &str) {
        let line = line.trim_end();
        if line.is_empty() {
            return;
        }
        if let Some(f) = self.log_file.lock().unwrap().as_mut() {
            let _ = writeln!(f, "{line}");
        }
        self.send(Msg::Log(line.to_string()));
    }

    fn step(&self, id: &str, done: bool) {
        self.log(&format!("[{id}] {}", if done { "done" } else { "started" }));
        self.send(Msg::Step { id: id.to_string(), done });
    }

    fn check_cancel(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::SeqCst) { Err(CANCELLED.into()) } else { Ok(()) }
    }

    fn run(self: Arc<Self>) -> Result<(String, bool), String> {
        let e = &self.engine;
        fs::create_dir_all(&e.root).map_err(|err| format!("The folder {} couldn't be created ({err}).", e.root.display()))?;
        *self.log_file.lock().unwrap() = fs::OpenOptions::new().create(true).append(true).open(e.root.join("setup.log")).ok();
        self.log(&format!("--- Setting up in {} (PyTorch build: {})", e.root.display(), self.backend.name()));
        let _ = fs::remove_file(e.marker()); // not ready again until this run finishes
        e.write_files().map_err(|err| format!("Couldn't write to {} ({err}).", e.root.display()))?;

        // 1. The installer tool and a private Python.
        self.step("tools", false);
        if !e.uv().exists() {
            self.install_uv()?;
        }
        if !e.python().exists() {
            let mut cmd = self.uv_command();
            cmd.arg("venv").arg(e.root.join("venv")).args(["--python", PYTHON_VERSION, "--managed-python", "--no-project", "--clear"]);
            self.run_command(cmd, |_| {}).map_err(|err| explain("Python couldn't be installed", &err))?;
        }
        self.step("tools", true);

        // 2. PyTorch and the other packages: by far the largest download.
        self.step("packages", false);
        let mut cmd = self.uv_command();
        cmd.args(["pip", "install", "--python"]).arg(e.python()).args(["--torch-backend", self.backend.name(), "-r"]).arg(e.root.join("requirements.txt"));
        let watcher = self.clone().watch_packages();
        let installed = self.run_command(cmd, |_| {});
        watcher.store(true, Ordering::SeqCst);
        installed.map_err(|err| explain("The audio engine couldn't be downloaded", &err))?;
        self.step("packages", true);

        // 3. Model code, ffmpeg, the model itself and a test run: done by the engine's own Python.
        let mut cmd = Command::new(e.python());
        cmd.arg("-u").arg(e.root.join("setup_engine.py")).arg("--root").arg(&e.root).current_dir(&e.root).env("PYTHONUTF8", "1").env("PYTHONIOENCODING", "utf-8");
        let mut outcome: Option<Result<(String, bool), String>> = None;
        let run = self.clone();
        let finished = self.run_command(cmd, |line| {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                run.log(line);
                return;
            };
            // The model code is a handful of small files; it is shown as part of the "video tools" step.
            let id = match v["id"].as_str().unwrap_or("") {
                "code" => "ffmpeg",
                other => other,
            };
            match v["event"].as_str().unwrap_or("") {
                "step" => match (v["id"].as_str(), v["state"].as_str()) {
                    (Some("code"), Some("done")) | (Some("ffmpeg"), Some("start")) => {}
                    (_, state) => run.step(id, state == Some("done")),
                },
                "progress" if v["id"] != "code" => run.send(Msg::Progress { id: id.to_string(), done: v["done"].as_u64().unwrap_or(0), total: v["total"].as_u64().unwrap_or(0) }),
                "fatal" => {
                    run.log(v["detail"].as_str().unwrap_or(""));
                    outcome = Some(Err(v["message"].as_str().unwrap_or("Setup failed.").to_string()));
                }
                "done" => {
                    run.log(&format!("PyTorch {}", v["torch"].as_str().unwrap_or("?")));
                    outcome = Some(Ok((v["device"].as_str().unwrap_or("CPU").to_string(), v["gpu"].as_bool().unwrap_or(false))));
                }
                _ => {}
            }
        });
        let (device, gpu) = match (outcome, finished) {
            (Some(result), _) => result?,
            (None, Err(err)) => return Err(explain("Setup couldn't finish", &err)),
            (None, Ok(())) => return Err("Setup ended without a result.".into()),
        };

        // 4. Done: tidy up and leave the marker that says so.
        let _ = fs::remove_dir_all(e.root.join("cache"));
        let marker = serde_json::json!({ "version": ENGINE_VERSION, "backend": self.backend.name(), "device": device, "gpu": gpu });
        fs::write(e.marker(), marker.to_string()).map_err(|err| format!("Couldn't finish writing to {} ({err}).", e.root.display()))?;
        Ok((device, gpu))
    }

    fn uv_command(&self) -> Command {
        let root = &self.engine.root;
        let mut cmd = Command::new(self.engine.uv());
        // Everything stays inside the engine folder, and the user's own uv settings are ignored.
        cmd.env("UV_CACHE_DIR", root.join("cache"))
            .env("UV_PYTHON_INSTALL_DIR", root.join("python"))
            .env("UV_NO_CONFIG", "1")
            .env("UV_NO_PROGRESS", "1")
            .env("UV_HTTP_TIMEOUT", "300")
            .env_remove("UV_PYTHON")
            .env_remove("UV_INDEX_URL")
            .env_remove("UV_DEFAULT_INDEX")
            .env_remove("UV_EXTRA_INDEX_URL")
            .env_remove("VIRTUAL_ENV")
            .current_dir(root);
        cmd
    }

    fn install_uv(&self) -> Result<(), String> {
        let dir = self.engine.root.join("uv");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let archive = dir.join("uv.zip");
        self.download(UV_URL, &archive, UV_SHA256, UV_SIZE)?;
        let unpack = || -> Result<(), Box<dyn std::error::Error>> {
            let mut zip = zip::ZipArchive::new(fs::File::open(&archive)?)?;
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i)?;
                if entry.name()?.rsplit(['/', '\\']).next() == Some("uv.exe") {
                    let part = dir.join("uv.exe.part");
                    std::io::copy(&mut entry, &mut fs::File::create(&part)?)?;
                    fs::rename(&part, dir.join("uv.exe"))?;
                    return Ok(());
                }
            }
            Err("uv.exe is missing from the download".into())
        };
        unpack().map_err(|e| format!("The installer tool couldn't be unpacked ({e})."))?;
        let _ = fs::remove_file(&archive);
        Ok(())
    }

    /// Download a file with progress and check it against its known SHA-256.
    fn download(&self, url: &str, dest: &Path, sha256: &str, size: u64) -> Result<(), String> {
        let offline = |e: &dyn std::fmt::Display| format!("Couldn't download from {} ({e}). Check your internet connection.", host_of(url));
        // Windows' own TLS and certificate store, like a browser on this PC would use.
        let tls = ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
        let agent: ureq::Agent = ureq::Agent::config_builder().tls_config(tls).build().into();
        let mut res = agent.get(url).header("User-Agent", USER_AGENT).call().map_err(|e| offline(&e))?;
        let total = res.body().content_length().unwrap_or(size);
        let mut reader = res.body_mut().as_reader();
        let mut file = fs::File::create(dest).map_err(|e| format!("Couldn't write {} ({e}).", dest.display()))?;
        let (mut hasher, mut done, mut shown) = (Sha256::new(), 0u64, Instant::now());
        let mut buf = vec![0u8; 1 << 16];
        loop {
            self.check_cancel()?;
            let n = reader.read(&mut buf).map_err(|e| offline(&e))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| format!("Couldn't write {} ({e}).", dest.display()))?;
            hasher.update(&buf[..n]);
            done += n as u64;
            if shown.elapsed() > Duration::from_millis(200) {
                shown = Instant::now();
                self.send(Msg::Progress { id: "tools".into(), done, total });
            }
        }
        let digest: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if digest != sha256 {
            let _ = fs::remove_file(dest);
            return Err("A downloaded file was damaged on the way. Try again.".into());
        }
        Ok(())
    }

    /// Report progress of the package download by watching the cache folder grow. The installer
    /// tool doesn't report byte counts itself. Returns the flag that stops the watching.
    fn watch_packages(self: Arc<Self>) -> Arc<AtomicBool> {
        let stop = Arc::new(AtomicBool::new(false));
        let (flag, cache, total) = (stop.clone(), self.engine.root.join("cache"), self.backend.packages_bytes());
        std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                // Never quite full: the last stretch is unpacking and linking, which adds no bytes here.
                let done = dir_size(&cache).min(total * 97 / 100);
                if !flag.load(Ordering::SeqCst) {
                    self.send(Msg::Progress { id: "packages".into(), done, total });
                }
                std::thread::sleep(Duration::from_millis(1500));
            }
        });
        stop
    }

    /// Run a command to the end. stdout lines go to `on_line`, stderr lines to the log; on failure
    /// the last stderr lines are the error.
    fn run_command(self: &Arc<Self>, mut cmd: Command, mut on_line: impl FnMut(&str)) -> Result<(), String> {
        self.check_cancel()?;
        self.log(&format!("> {cmd:?}"));
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        crate::no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("{:?} couldn't be started ({e}).", cmd.get_program()))?;
        let (stdout, stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        *self.child.lock().unwrap() = Some(child);

        let run = self.clone();
        let errors = std::thread::spawn(move || {
            let mut tail: Vec<String> = Vec::new();
            for_each_line(stderr, |line| {
                run.log(line);
                if !line.trim().is_empty() {
                    tail.push(line.trim_end().to_string());
                    if tail.len() > 6 {
                        tail.remove(0);
                    }
                }
            });
            tail
        });
        for_each_line(stdout, |line| on_line(line));
        let tail = errors.join().unwrap_or_default();

        let child = self.child.lock().unwrap().take();
        let ok = child.and_then(|mut c| c.wait().ok()).is_some_and(|s| s.success());
        self.check_cancel()?;
        if ok { Ok(()) } else { Err(tail.join("\n")) }
    }
}

/// Call `f` for every line of a child's output; bytes that aren't valid UTF-8 are replaced.
fn for_each_line(stream: impl Read, mut f: impl FnMut(&str)) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    while reader.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
        f(String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']));
        buf.clear();
    }
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn host_of(url: &str) -> &str {
    url.split("//").nth(1).and_then(|rest| rest.split('/').next()).unwrap_or(url)
}

/// Turn the tail of a tool's error output into something a person can act on.
pub fn explain(what: &str, detail: &str) -> String {
    if detail == CANCELLED {
        return detail.to_string();
    }
    let lower = detail.to_lowercase();
    let why = if lower.contains("no space") || lower.contains("not enough space") || lower.contains("os error 112") {
        "The drive is full. Free up some space, or choose another folder, and try again."
    } else if ["dns", "connect", "timed out", "timeout", "network", "failed to fetch", "failed to download", "connection", "os error 11001"].iter().any(|k| lower.contains(k)) {
        "The download was interrupted. Check your internet connection and try again; it carries on where it stopped."
    } else if lower.contains("access is denied") || lower.contains("permission denied") || lower.contains("os error 5") {
        "Windows blocked a file from being written. Check that your antivirus isn't blocking the app, then try again."
    } else {
        "Try again. If it keeps happening, the details below say what went wrong."
    };
    format!("{what}. {why}")
}
