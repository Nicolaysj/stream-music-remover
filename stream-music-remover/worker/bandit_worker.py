"""Separation worker for Stream Music Remover.

Runs Bandit v2 (speech / music / sfx) on audio or video files.

Single-track mode (one mixed audio track):
  * "<name> (no music).wav"        speech + sfx: the recording without music
  * "<name> - speech|sfx|music.wav" individual stems (optional)

Multi-track mode (--mic-track / --game-track, e.g. an OBS recording with the mic on its own track):
  only the gameplay track is separated; the mic track is used untouched.
  * "<name> (no music).wav"        mic + speech + sfx from the gameplay track
  * "<name> - mic.wav", "<name> - game (no music).wav", "<name> - music.wav" (optional)

Video inputs can also be rewritten as "<name> (no music).<ext>" with the picture copied
(not re-encoded); in multi-track mode the mic and de-musiced gameplay can be added as extra
audio tracks for editing.

Long files are processed in segments so memory stays flat. Progress is reported to the GUI as
one JSON object per line on stdout.
"""

import argparse
import json
import os
import subprocess
import sys
import time
import traceback

os.environ.setdefault("TQDM_DISABLE", "1")
os.environ.setdefault("PYTHONWARNINGS", "ignore")

import warnings

warnings.filterwarnings("ignore")

SR = 48000
CHANNELS = 2
NO_WINDOW = 0x08000000 if os.name == "nt" else 0  # CREATE_NO_WINDOW for ffmpeg children
_json_out = sys.stdout


def emit(event, **data):
    data["event"] = event
    _json_out.write(json.dumps(data) + "\n")
    _json_out.flush()


def run(cmd):
    return subprocess.run(cmd, capture_output=True, creationflags=NO_WINDOW)


def remove_quietly(path):
    try:
        os.remove(path)
    except OSError:
        pass


def probe(path):
    """Return (duration_seconds, has_video, audio_tracks) where audio_tracks is a list of titles."""
    r = run(["ffprobe", "-v", "error", "-show_entries",
             "format=duration:stream=codec_type:stream_disposition=attached_pic:stream_tags=title,handler_name",
             "-of", "json", path])
    if r.returncode != 0:
        raise RuntimeError("ffprobe could not read the file: " + r.stderr.decode(errors="replace").strip()[-300:])
    info = json.loads(r.stdout or b"{}")
    streams = info.get("streams", [])
    has_video = any(s.get("codec_type") == "video" and not s.get("disposition", {}).get("attached_pic") for s in streams)
    tracks = [(s.get("tags") or {}).get("title") or (s.get("tags") or {}).get("handler_name") or ""
              for s in streams if s.get("codec_type") == "audio"]
    duration = float(info.get("format", {}).get("duration") or 0)
    return duration, has_video, tracks


def decode(path, track, start, length, np):
    """Decode [start, start+length) seconds of audio track `track` (0-based) as float32 (channels, samples)."""
    cmd = ["ffmpeg", "-v", "error", "-nostdin"]
    if start > 0:
        cmd += ["-ss", f"{start:.6f}"]
    cmd += ["-i", path, "-t", f"{length:.6f}", "-map", f"0:a:{track}", "-vn",
            "-ac", str(CHANNELS), "-ar", str(SR), "-f", "f32le", "-"]
    r = run(cmd)
    if r.returncode != 0:
        raise RuntimeError(f"ffmpeg could not decode audio track {track + 1}: " + r.stderr.decode(errors="replace").strip()[-300:])
    data = np.frombuffer(r.stdout, dtype=np.float32)
    usable = len(data) - len(data) % CHANNELS
    return data[:usable].reshape(-1, CHANNELS).T.copy()


class ProgressHook(dict):
    """demix() writes callback['progress'] after every batch; forward it to the GUI."""

    def __init__(self, on_progress):
        super().__init__()
        self.on_progress = on_progress

    def __setitem__(self, key, value):
        super().__setitem__(key, value)
        if key == "progress":
            self.on_progress(float(value))


def load_model(msst_dir, model_path, config_path, device_name):
    sys.path.insert(0, msst_dir)
    import torch
    from utils.utils import get_model_from_config

    model, config = get_model_from_config("bandit_v2", config_path)
    # weights_only: a checkpoint is only ever read as data, never run as code.
    state = torch.load(model_path, map_location="cpu", weights_only=True)
    if isinstance(state, dict) and "state_dict" in state:  # raw Lightning checkpoint
        state = {k[len("model."):]: v for k, v in state["state_dict"].items() if k.startswith("model.")}
    model.load_state_dict(state)
    if device_name == "auto":
        device_name = "cuda" if torch.cuda.is_available() else "cpu"
    device = torch.device(device_name)
    model = model.to(device).eval()
    gpu = torch.cuda.get_device_name(0) if device.type == "cuda" else "CPU"
    return model, config, device, gpu


def fit(x, n, np):
    """Trim or zero-pad (channels, samples) to n samples."""
    if x.shape[1] >= n:
        return x[:, :n]
    return np.pad(x, ((0, 0), (0, n - x.shape[1])))


def process_file(index, path, args, model, config, device):
    import numpy as np
    import soundfile as sf
    from utils.utils import demix

    duration, has_video, tracks = probe(path)
    if not tracks:
        raise RuntimeError("The file has no audio track.")
    if duration <= 0:
        raise RuntimeError("Could not determine the file's duration.")

    multi = args.mic_track > 0
    game_t = 0
    if multi:
        mic_t, game_t = args.mic_track - 1, args.game_track - 1
        if mic_t == game_t:
            raise RuntimeError("The mic track and the game audio track must be different tracks.")
        if max(mic_t, game_t) >= len(tracks):
            # Not a multi-track recording: fall back to separating the single mixed track.
            n = len(tracks)
            emit("note", index=index, message=f"This file has {'only one audio track' if n == 1 else f'{n} audio tracks'}, "
                                              "so it was processed as one mixed track.")
            multi, game_t = False, 0

    out_dir = args.outdir or os.path.dirname(os.path.abspath(path))
    os.makedirs(out_dir, exist_ok=True)
    # The app passes "name" when two queued files would otherwise produce the same output names.
    base = getattr(args, "name", "") or os.path.splitext(os.path.basename(path))[0]
    ext = os.path.splitext(path)[1].lower()

    want_video = args.video and has_video
    want_tracks = want_video and multi and args.video_tracks
    want_clean = args.clean or (args.video and not has_video and not args.stems)  # audio-only input with only "video" ticked
    if not (want_clean or want_video or args.stems):
        raise RuntimeError("No outputs selected.")
    args = argparse.Namespace(**{**vars(args), "clean": want_clean})

    # key -> (final path, requested by the user?)
    plan = {}
    if args.clean or want_video:
        plan["clean"] = (os.path.join(out_dir, f"{base} (no music).wav"), args.clean)
    if multi:
        if args.stems or want_tracks:
            plan["mic"] = (os.path.join(out_dir, f"{base} - mic.wav"), args.stems)
            plan["game"] = (os.path.join(out_dir, f"{base} - game (no music).wav"), args.stems)
        if args.stems:
            plan["music"] = (os.path.join(out_dir, f"{base} - music.wav"), True)
    elif args.stems:
        for stem in ("speech", "sfx", "music"):
            plan[stem] = (os.path.join(out_dir, f"{base} - {stem}.wav"), True)

    # Everything is written under a ".partial" name first. Requested files are renamed when they are
    # complete; helper files that are only needed to build the video keep the temporary name and are
    # deleted afterwards. Whatever happens, no ".partial" file is left behind (see the finally below).
    partial = {k: final[:-4] + ".partial.wav" for k, (final, _) in plan.items()}
    video_part = None
    # A 24-bit stereo WAV passes the format's 4 GB limit after about 4 hours. RF64 is the same file
    # without the limit, and editors and ffmpeg read it as a normal .wav.
    wav_format = "RF64" if (duration + 60) * SR * CHANNELS * 3 >= 0xFFF00000 else "WAV"
    writers = {}
    clipped = 0
    seg = float(args.segment_seconds)
    pad = 5.0
    start = 0.0
    try:
        for k, p in partial.items():
            writers[k] = sf.SoundFile(p, "w", samplerate=SR, channels=CHANNELS, subtype="PCM_24", format=wav_format)
        while start < duration:
            pre = min(pad, start)
            game = decode(path, game_t, start - pre, pre + seg + pad, np)
            if game.shape[1] <= int(pre * SR):
                break

            def on_progress(p, start=start):
                done = min(start + p * seg, duration)
                emit("progress", index=index, value=min(done / duration, 0.99), stage="Separating")

            stems = demix(config, model, game, device, model_type="bandit_v2", callback=ProgressHook(on_progress))

            a = int(round(pre * SR))
            b = min(game.shape[1], a + int(round(seg * SR)))
            n = b - a
            cut = {k: v[:, a:b] for k, v in stems.items()}
            out = {}
            if multi:
                mic = fit(decode(path, mic_t, start, seg, np), n, np)
                game_clean = cut["speech"] + cut["sfx"]
                out["clean"] = mic + game_clean
                out["mic"] = mic
                out["game"] = game_clean
                out["music"] = cut["music"]
            else:
                out["clean"] = cut["speech"] + cut["sfx"]
                out.update(cut)
            for k, w in writers.items():
                x = out[k]
                clipped += int(np.count_nonzero(np.abs(x) > 1.0))
                w.write(np.clip(x, -1, 1).T)
            emit("progress", index=index, value=min((start + seg) / duration, 0.99), stage="Separating")
            if n < int(round(seg * SR)):  # reached end of audio
                break
            start += seg
        for w in writers.values():
            w.close()

        # Where each finished WAV is now: its final name if the user asked for it, else still temporary.
        ready = {}
        for k, (final, requested) in plan.items():
            if requested:
                os.replace(partial[k], final)
                ready[k] = final
            else:
                ready[k] = partial[k]
        if clipped:
            emit("log", message=f"{os.path.basename(path)}: {clipped} samples were above full scale and were clipped.")

        outputs = [final for final, requested in plan.values() if requested]

        if want_video:
            emit("progress", index=index, value=0.995, stage="Writing video")
            video_ext = ext if ext in (".mp4", ".mkv", ".mov", ".m4v", ".webm") else ".mkv"
            audio_codec = ["-c:a", "libopus", "-b:a", "256k"] if video_ext == ".webm" else ["-c:a", "aac", "-b:a", "320k"]
            final = os.path.join(out_dir, f"{base} (no music){video_ext}")
            video_part = os.path.join(out_dir, f"{base} (no music).partial{video_ext}")
            audio_inputs = [("clean", "Mix (no music)")]
            if want_tracks:
                audio_inputs += [("mic", "Mic"), ("game", "Game (no music)")]
            cmd = ["ffmpeg", "-v", "error", "-nostdin", "-y", "-i", path]
            for key, _ in audio_inputs:
                cmd += ["-i", ready[key]]
            cmd += ["-map", "0:v"]
            for i, (_, title) in enumerate(audio_inputs):
                cmd += ["-map", f"{i + 1}:a:0", f"-metadata:s:a:{i}", f"title={title}"]
            cmd += ["-c:v", "copy", *audio_codec, "-shortest"]
            if video_ext in (".mp4", ".mov", ".m4v"):
                cmd += ["-movflags", "+faststart"]
            r = run(cmd + [video_part])
            if r.returncode != 0:
                raise RuntimeError("ffmpeg could not write the video: " + r.stderr.decode(errors="replace").strip()[-300:])
            os.replace(video_part, final)
            outputs.append(final)
        return outputs
    finally:
        # Unfinished files after an error, and helper files that were only needed for the video.
        for w in writers.values():
            w.close()
        for p in partial.values():
            remove_quietly(p)
        if video_part:
            remove_quietly(video_part)


def make_sample(path, args, model, config, device):
    """Separate a short stretch of a file and write "before" and "after" WAVs, for listening first."""
    import numpy as np
    import soundfile as sf
    from utils.utils import demix

    duration, _, tracks = probe(path)
    if not tracks:
        raise RuntimeError("The file has no audio track.")
    length = min(float(args.sample_seconds), max(duration, 1.0))
    start = args.sample_start if args.sample_start >= 0 else max(0.0, duration * 0.4)
    start = max(0.0, min(start, duration - length))
    multi = args.mic_track > 0 and max(args.mic_track, args.game_track) <= len(tracks) and args.mic_track != args.game_track
    game_t = args.game_track - 1 if multi else 0
    game = decode(path, game_t, start, length, np)
    stems = demix(config, model, game, device, model_type="bandit_v2", callback=ProgressHook(lambda p: None))
    clean = stems["speech"] + stems["sfx"]
    original = game
    if multi:
        mic = fit(decode(path, args.mic_track - 1, start, length, np), game.shape[1], np)
        clean = clean + mic
        original = game + mic
    os.makedirs(args.outdir, exist_ok=True)
    base = os.path.splitext(os.path.basename(path))[0]
    before = os.path.join(args.outdir, f"{base} - sample before.wav")
    after = os.path.join(args.outdir, f"{base} - sample after (no music).wav")
    sf.write(before, np.clip(original, -1, 1).T, SR, subtype="PCM_16")
    sf.write(after, np.clip(clean, -1, 1).T, SR, subtype="PCM_16")
    return before, after, start


def main():
    global _json_out
    ap = argparse.ArgumentParser()
    ap.add_argument("--msst-dir", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--config", required=True)
    ap.add_argument("--outdir", default="")
    ap.add_argument("--clean", action="store_true", help="write the no-music WAV")
    ap.add_argument("--video", action="store_true", help="write video with music removed")
    ap.add_argument("--stems", action="store_true", help="write individual stems")
    ap.add_argument("--mic-track", type=int, default=0, help="1-based audio track with the mic only (enables multi-track mode)")
    ap.add_argument("--game-track", type=int, default=0, help="1-based audio track with gameplay + music (multi-track mode)")
    ap.add_argument("--video-tracks", action="store_true", help="multi-track mode: also put mic and gameplay as extra tracks in the video")
    ap.add_argument("--device", default="auto")
    ap.add_argument("--segment-seconds", type=float, default=300)
    ap.add_argument("--serve", action="store_true", help="read jobs as JSON lines from stdin (used by the app)")
    ap.add_argument("files", nargs="*")
    args = ap.parse_args()
    if args.mic_track > 0 and args.game_track <= 0:
        ap.error("--game-track is required with --mic-track")
    if not args.serve and not args.files:
        ap.error("give files to process, or use --serve")

    # Keep stray prints from libraries off the JSON channel.
    _json_out = sys.stdout
    sys.stdout = sys.stderr

    emit("status", message="Loading model...")
    try:
        t = time.time()
        model, config, device, gpu = load_model(args.msst_dir, args.model, args.config, args.device)
        emit("ready", message=f"Model loaded on {gpu} in {time.time() - t:.1f}s")
    except Exception as e:
        emit("fatal", message=f"Could not load the model: {e}", detail=traceback.format_exc())
        sys.exit(1)

    def run_job(index, path, job_args):
        emit("file_start", index=index, path=path)
        t = time.time()
        try:
            outputs = process_file(index, path, job_args, model, config, device)
            emit("file_done", index=index, outputs=outputs, seconds=round(time.time() - t, 1))
            return True
        except Exception as e:
            emit("file_error", index=index, message=str(e), detail=traceback.format_exc())
            return False
        finally:
            if device.type == "cuda":
                import torch

                torch.cuda.empty_cache()

    if args.serve:
        # One job per line: {"index": n, "path": "...", plus any option overrides
        # (outdir, clean, video, stems, mic_track, game_track, video_tracks, segment_seconds)}.
        emit("idle")
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            try:
                job = json.loads(line)
            except json.JSONDecodeError:
                emit("log", message=f"Ignoring malformed job: {line[:200]}")
                continue
            if job.get("quit"):
                break
            opts = {k: v for k, v in job.items() if k not in ("index", "path", "sample")}
            job_args = argparse.Namespace(**{**vars(args), **opts})
            if job.get("sample"):
                index = int(job["index"])
                try:
                    job_args.sample_seconds = float(job.get("sample_seconds", 30))
                    job_args.sample_start = float(job.get("sample_start", -1))
                    before, after, start = make_sample(job["path"], job_args, model, config, device)
                    emit("sample_done", index=index, before=before, after=after, start=start)
                except Exception as e:
                    emit("sample_error", index=index, message=str(e), detail=traceback.format_exc())
                emit("idle")
                continue
            run_job(int(job["index"]), job["path"], job_args)
            emit("idle")
        return

    failures = 0
    for i, path in enumerate(args.files):
        failures += not run_job(i, path, args)
    emit("all_done", failures=failures)


if __name__ == "__main__":
    main()
