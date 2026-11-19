//! Renders the app's screen states to PNG files for design review.
//!
//! Run with:  SMR_SHOT_DIR=<folder> cargo test --release render_screens -- --ignored --nocapture
//! Uses sample data only: no worker is started and no files are touched.

use super::*;
use egui_kittest::Harness;

const WINDOW: [f32; 2] = [880.0, 780.0];
const FULL_PAGE: [f32; 2] = [880.0, 1440.0];

fn job(path: &str, tracks: &[&str], duration: f64, status: Status) -> Job {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let path = PathBuf::from(path);
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    Job {
        id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        batch: 0,
        is_video: VIDEO_EXT.contains(&ext.as_str()),
        path,
        status,
        probe: Some(Ok(Probe { tracks: tracks.iter().map(|s| s.to_string()).collect(), duration })),
        note: None,
        opts: None,
        sample: Sample::None,
    }
}

fn running(progress: f32, secs_ago: u64) -> Status {
    Status::Running { progress, stage: "Separating".into(), started: Instant::now() - Duration::from_secs(secs_ago) }
}

fn done(outputs: &[&str], seconds: f64) -> Status {
    Status::Done { outputs: outputs.iter().map(|s| s.to_string()).collect(), seconds }
}

const OBS: &[&str] = &["Track 1", "Mic", "Game + music"];
const ONE: &[&str] = &[""];

fn queue() -> Vec<Job> {
    vec![
        job(r"E:\Streams\2026-11-14 Elden Ring part 12.mkv", OBS, 4.0 * 3600.0 + 12.0 * 60.0, Status::Waiting),
        job(r"E:\Streams\2026-11-15 Baldur's Gate co-op.mp4", OBS, 3.0 * 3600.0 + 41.0 * 60.0, Status::Waiting),
        job(r"E:\Streams\Intro voiceover.wav", ONE, 95.0, Status::Waiting),
    ]
}

fn fake_worker() -> Worker {
    let (_tx, rx) = channel();
    Worker { child: Arc::new(Mutex::new(None)), stdin: None, rx, ready: true, idle: false }
}

fn processing(app: &mut App) {
    app.jobs = queue();
    app.jobs[0].status = done(&[r"E:\Streams\2026-11-14 Elden Ring part 12 (no music).mkv"], 1412.0);
    app.settings.multitrack = true;
    app.jobs[1].status = running(0.42, 560);
    app.batch_id = 1;
    for i in 0..2 {
        let opts = JobOptions { multitrack: true, mic_track: 2, game_track: 3, out_dir: app.out_dir_for(&app.jobs[i]), base: app.out_base(&app.jobs[i]) };
        app.jobs[i].opts = Some(opts);
        app.jobs[i].batch = 1;
    }
    app.batch_active = true;
    app.batch_started = Instant::now() - Duration::from_secs(1980);
    app.speed = Some(10.5);
    app.worker = Some(fake_worker());
}

fn render(name: &str, size: [f32; 2], setup: impl FnOnce(&mut App)) {
    let dir = std::env::var("SMR_SHOT_DIR").expect("set SMR_SHOT_DIR");
    std::fs::create_dir_all(&dir).unwrap();
    let mut harness = Harness::builder().with_size(size).with_pixels_per_point(1.25).build_eframe(|cc| {
        let mut app = App::with_args(cc, vec![]);
        app.settings = Settings { learned_speed: 10.5, ..Settings::default() };
        app.ffmpeg_ok = true;
        setup(&mut app);
        app
    });
    harness.run_steps(6);
    let img = harness.render().expect("render failed");
    let path = Path::new(&dir).join(format!("{name}.png"));
    img.save(&path).unwrap();
    println!("wrote {}", path.display());
}

#[test]
#[ignore]
fn render_screens() {
    render("01_first_launch", WINDOW, |_| {});
    render("02_files_added", WINDOW, |app| app.jobs = queue());
    render("02b_files_added_full_page", FULL_PAGE, |app| app.jobs = queue());
    render("03_processing", WINDOW, processing);
    render("04_stop_confirmation", WINDOW, |app| {
        processing(app);
        app.dialog = Dialog::ConfirmStop;
    });
    render("05_finished_all_ok", WINDOW, |app| {
        app.jobs = queue();
        for (j, s) in app.jobs.iter_mut().zip([1412.0, 1290.0, 22.0]) {
            j.status = done(&[r"E:\Streams\x (no music).mkv"], s);
        }
        app.summary = Some(Summary { done: 3, failed: 0, stopped: false, seconds: 2724.0 });
    });
    render("06_finished_with_error", WINDOW, |app| {
        app.jobs = queue();
        app.jobs[0].status = done(&[r"E:\Streams\a (no music).mkv"], 1412.0);
        app.jobs[1].status = done(&[r"E:\Streams\b (no music).mp4"], 1290.0);
        app.jobs[2].status = Status::Failed("The disk is full (no space left on device).".into());
        app.summary = Some(Summary { done: 2, failed: 1, stopped: false, seconds: 2702.0 });
    });
    render("07_mic_track_mode", WINDOW, |app| {
        app.jobs = queue();
        app.settings.multitrack = true;
    });
    render("07b_mic_track_mode_full_page", FULL_PAGE, |app| {
        app.jobs = queue();
        app.settings.multitrack = true;
    });
    render("08_settings_window", WINDOW, |app| {
        app.jobs = queue();
        app.log("Starting the processing engine".to_string());
        app.dialog = Dialog::Settings;
    });
    render("10_close_confirmation", WINDOW, |app| {
        processing(app);
        app.dialog = Dialog::ConfirmClose;
    });
    render("11_same_name_files", WINDOW, |app| {
        app.jobs = vec![
            job(r"E:\Streams\2026-11-14 Elden Ring part 12.mkv", ONE, 4.0 * 3600.0 + 12.0 * 60.0, Status::Waiting),
            job(r"E:\Streams\2026-11-14 Elden Ring part 12.wav", ONE, 4.0 * 3600.0 + 12.0 * 60.0, Status::Waiting),
            job(r"E:\Streams\Two tracks only.mkv", &["Track 1", "Mic"], 1800.0, Status::Waiting),
        ];
        app.settings.multitrack = true;
        app.settings.mic_track = 2;
        app.settings.game_track = 3;
        app.mode_touched = true;
    });
    render("09_listening_to_samples", WINDOW, |app| {
        app.jobs = queue();
        app.jobs[0].sample = Sample::Ready { before: "a.wav".into(), after: "b.wav".into() };
        app.jobs[1].sample = Sample::Working;
    });
}
