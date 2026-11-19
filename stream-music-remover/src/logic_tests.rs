//! Tests for the queue logic: which file a worker message belongs to, what a run counts,
//! and how result files are named. No worker is started and no files are written.

use super::*;

fn app() -> App {
    let mut app = App::from_parts(egui::Context::default(), Settings::default(), true);
    // Start every test from the same choices, whatever this machine's defaults are.
    app.settings.use_custom_folder = false;
    app.settings.multitrack = false;
    app.settings.out_video = true;
    app.settings.out_clean = true;
    app.settings.out_stems = false;
    app
}

fn add(app: &mut App, path: &str) -> u64 {
    let path = PathBuf::from(path);
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    app.push_job(path, VIDEO_EXT.contains(&ext.as_str()));
    app.jobs.last().unwrap().id
}

fn job(app: &App, id: u64) -> &Job {
    app.jobs.iter().find(|j| j.id == id).expect("file is still in the list")
}

fn job_mut(app: &mut App, id: u64) -> &mut Job {
    app.jobs.iter_mut().find(|j| j.id == id).expect("file is still in the list")
}

/// Mark a file as sent to the worker in the current run, the way dispatch() does.
fn send(app: &mut App, id: u64) {
    let batch = app.batch_id;
    let j = job_mut(app, id);
    j.batch = batch;
    j.status = Status::Running { progress: 0.0, stage: "Starting".into(), started: Instant::now() };
}

fn finish(app: &mut App, id: u64) {
    app.handle_event(&serde_json::json!({"event": "file_done", "index": id, "outputs": [format!("out {id}.wav")], "seconds": 5.0}));
}

#[test]
fn results_stay_with_their_file_when_the_list_changes_mid_run() {
    let mut app = app();
    let a = add(&mut app, r"E:\Streams\a.mkv");
    let b = add(&mut app, r"E:\Streams\b.mkv");
    let c = add(&mut app, r"E:\Streams\c.mkv");
    app.batch_active = true;
    app.batch_id = 1;
    send(&mut app, a);
    finish(&mut app, a);
    send(&mut app, b);

    // "Remove finished" while b is still running: b and c both move up a row.
    app.jobs.retain(|j| !matches!(j.status, Status::Done { .. }));

    app.handle_event(&serde_json::json!({"event": "progress", "index": b, "value": 0.5, "stage": "Separating"}));
    assert!(matches!(job(&app, b).status, Status::Running { progress, .. } if progress == 0.5));
    assert!(matches!(job(&app, c).status, Status::Waiting));

    finish(&mut app, b);
    assert!(matches!(&job(&app, b).status, Status::Done { outputs, .. } if outputs == &[format!("out {b}.wav")]));
    assert!(matches!(job(&app, c).status, Status::Waiting), "the waiting file must not be credited with another file's result");
}

#[test]
fn messages_about_a_removed_file_are_ignored() {
    let mut app = app();
    let a = add(&mut app, r"E:\Streams\a.mkv");
    let b = add(&mut app, r"E:\Streams\b.mkv");
    app.jobs.retain(|j| j.id != a);
    app.handle_event(&serde_json::json!({"event": "file_error", "index": a, "message": "boom"}));
    assert!(matches!(job(&app, b).status, Status::Waiting));
}

#[test]
fn a_run_counts_only_its_own_files() {
    let mut app = app();
    let a = add(&mut app, r"E:\Streams\a.mkv");
    app.batch_active = true;
    app.batch_id = 1;
    send(&mut app, a);
    finish(&mut app, a);
    app.finish_batch(false);
    assert_eq!(app.summary.as_ref().map(|s| s.done), Some(1));

    // A second run with one new file: the file from the first run is not part of it.
    let b = add(&mut app, r"E:\Streams\b.mkv");
    app.batch_active = true;
    app.batch_id = 2;
    let (finished, total, frac, _) = app.batch_progress();
    assert_eq!((finished, total), (0, 1));
    assert_eq!(frac, 0.0);
    send(&mut app, b);
    finish(&mut app, b);
    app.finish_batch(false);
    assert_eq!(app.summary.as_ref().map(|s| (s.done, s.failed)), Some((1, 0)));
}

#[test]
fn setting_a_file_to_waiting_mid_run_does_not_restart_the_run() {
    let mut app = app();
    add(&mut app, r"E:\Streams\a.mkv");
    app.batch_active = true;
    app.batch_id = 3;
    let started = app.batch_started;
    app.start();
    assert_eq!(app.batch_id, 3);
    assert_eq!(app.batch_started, started);
}

#[test]
fn files_with_the_same_name_get_separate_results() {
    let mut app = app();
    let video = add(&mut app, r"E:\Streams\stream.mp4");
    let audio = add(&mut app, r"E:\Streams\STREAM.wav");
    let other = add(&mut app, r"E:\Other\stream.mp4");

    // Saved next to each recording: only the two in the same folder clash.
    assert_eq!(app.out_base(job(&app, video)), "stream");
    assert_eq!(app.out_base(job(&app, audio)), "STREAM (2)");
    assert_eq!(app.out_base(job(&app, other)), "stream");

    // Saved to one folder: all three clash.
    app.settings.use_custom_folder = true;
    app.settings.custom_folder = r"E:\Out".into();
    assert_eq!(app.out_base(job(&app, other)), "stream (3)");

    let mut all: Vec<PathBuf> = app.jobs.iter().flat_map(|j| app.planned_outputs(j)).collect();
    let count = all.len();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), count, "no two files may produce the same result file");
    assert!(all.contains(&PathBuf::from(r"E:\Out\stream (3) (no music).mp4")));
}

#[test]
fn planned_results_match_the_chosen_outputs() {
    let mut app = app();
    let video = add(&mut app, r"E:\Streams\vod.flv");
    let audio = add(&mut app, r"E:\Streams\voice.mp3");
    app.settings.out_clean = false;
    let names = |app: &App, id| app.planned_outputs(job(app, id)).iter().map(|p| file_name(p)).collect::<Vec<_>>();
    assert_eq!(names(&app, video), ["vod (no music).mkv"]); // FLV can't hold the result, so it becomes an MKV
    assert_eq!(names(&app, audio), ["voice (no music).wav"]); // audio in, WAV out even with only "video" ticked
    app.settings.out_stems = true;
    assert_eq!(names(&app, audio), ["voice - speech.wav", "voice - sfx.wav", "voice - music.wav"]);
}

#[test]
fn unfinished_results_are_never_queued_as_recordings() {
    let mut app = app();
    app.add_paths([
        PathBuf::from(r"E:\Streams\vod (no music).partial.wav"),
        PathBuf::from(r"E:\Streams\vod - mic.partial.wav"),
        PathBuf::from(r"E:\Streams\vod (no music).partial.mkv"),
        PathBuf::from(r"E:\Streams\vod (no music).mkv"),
        PathBuf::from(r"E:\Streams\vod.mkv"),
    ]);
    let queued: Vec<String> = app.jobs.iter().map(|j| file_name(&j.path)).collect();
    assert_eq!(queued, ["vod.mkv"]);
}

#[test]
fn leftovers_cover_every_temporary_file_the_worker_writes() {
    let opts = JobOptions { multitrack: true, mic_track: 2, game_track: 3, out_dir: PathBuf::from(r"E:\Out"), base: "vod (2)".into() };
    let files = partial_files(&opts);
    for name in ["vod (2) (no music).partial.wav", "vod (2) - mic.partial.wav", "vod (2) - game (no music).partial.wav", "vod (2) - music.partial.wav", "vod (2) - speech.partial.wav", "vod (2) - sfx.partial.wav", "vod (2) (no music).partial.mkv", "vod (2) (no music).partial.mp4"] {
        assert!(files.contains(&Path::new(r"E:\Out").join(name)), "{name}");
    }
    assert!(files.iter().all(|f| file_name(f).contains(".partial.")), "only temporary files may ever be deleted");
}

/// Stopping must end ffmpeg (a child of the worker) too, or it keeps the unfinished file open and
/// carries on writing. Stand-in family: cmd (the "worker") running ping (the "ffmpeg"), both
/// holding the file open so that it can't be deleted while either of them is alive.
#[cfg(windows)]
#[test]
fn stopping_ends_the_whole_process_family_and_removes_its_leftovers() {
    use std::os::windows::fs::OpenOptionsExt;
    let file = std::env::temp_dir().join(format!("smr-test-{}.partial.wav", std::process::id()));
    let out = std::fs::OpenOptions::new().write(true).create(true).truncate(true).share_mode(0).open(&file).unwrap();
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", "ping -n 60 127.0.0.1"]).stdin(Stdio::null()).stdout(out).stderr(Stdio::null());
    no_window(&mut cmd);
    let child = cmd.spawn().unwrap();
    drop(cmd); // closes this process's own handle to the file
    std::thread::sleep(Duration::from_millis(700)); // give cmd time to start ping
    assert!(std::fs::remove_file(&file).is_err(), "the file should be locked while the family runs");

    let (_tx, rx) = channel();
    let worker = Worker { child: Arc::new(Mutex::new(Some(child))), stdin: None, rx, ready: true, idle: false };
    worker.kill();
    remove_partials(vec![file.clone()], &worker.child);
    assert!(!file.exists(), "the unfinished file should be gone once the family has been stopped");
}

#[test]
fn mic_track_layout_is_detected_from_track_names() {
    let mut app = app();
    let id = add(&mut app, r"E:\Streams\obs.mkv");
    let tracks = |names: &[&str]| Some(Ok(Probe { tracks: names.iter().map(|s| s.to_string()).collect(), duration: 60.0 }));
    job_mut(&mut app, id).probe = tracks(&["Track 1", "Mic", "Game + music"]);
    assert_eq!(app.detect_mic_layout(), Some((2, 3)));
    job_mut(&mut app, id).probe = tracks(&["Full mix", "Microphone", "Discord", "Other"]);
    assert_eq!(app.detect_mic_layout(), Some((2, 4)));
    job_mut(&mut app, id).probe = tracks(&[""]);
    assert_eq!(app.detect_mic_layout(), None);
}

#[test]
fn text_helpers() {
    assert_eq!(fmt_eta(30.0), "under a minute");
    assert_eq!(fmt_eta(90.0 * 60.0), "1h 30m");
    assert_eq!(fmt_secs(3725.0), "1h 02m");
    assert_eq!(middle_truncate("short", 10), "short");
    assert_eq!(middle_truncate("abcdefghijklmnopqrstuvwxyz", 11).chars().count(), 11);
    assert_eq!(drive_of(Path::new(r"D:\Out")), "D:");
    let opts = JobOptions { multitrack: false, mic_track: 2, game_track: 3, out_dir: PathBuf::from(r"D:\Out"), base: "x".into() };
    assert!(friendly_error("[Errno 28] No space left on device", Some(&opts), "D:").starts_with("Drive D: is full"));
}
