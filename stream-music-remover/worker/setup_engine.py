"""One-time engine setup for Stream Music Remover.

The app runs this with the engine's own Python once the Python packages are installed. It fetches
the remaining pieces into the engine folder and checks that the whole thing works:

  code    the Bandit v2 model code (a few files from MSST-WebUI, pinned to one commit)
  ffmpeg  ffmpeg.exe and ffprobe.exe
  model   the Bandit v2 weights from Zenodo, reduced to the part needed for separating
  test    load the model and separate three seconds of noise

Every download is checked against a known SHA-256 before it is used, and steps that are already
done are skipped, so running it again after a failure carries on where it stopped.
Progress goes to the app as one JSON object per line on stdout.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
import traceback
import urllib.error
import urllib.request
import zipfile

HEADERS = {"User-Agent": "StreamMusicRemover-setup"}
NO_WINDOW = 0x08000000 if os.name == "nt" else 0

MSST_COMMIT = "39287d46072d2d1d91d9f1518abb1dd9a930e3d7"
MSST_RAW = f"https://raw.githubusercontent.com/SUC-DriverOld/MSST-WebUI/{MSST_COMMIT}/"
# utils/constant.py is left out on purpose: it reads WebUI data files that don't exist here, and
# utils/logger.py falls back to its own defaults without it.
ENGINE_FILES = {
    "modules/bandit_v2/bandit.py": "274d86121ecc5211bc94e65c6adc6735cb15578b5ec78d1919e3c7d7a5352229",
    "modules/bandit_v2/bandsplit.py": "69440dba123eb6a84129aaffba891d06405ae98c9897b1925f8f8dce55a86bdd",
    "modules/bandit_v2/maskestim.py": "d7d4aec162d28c0c483717a946c9c65f14867e4a97c9becc84242d272da594aa",
    "modules/bandit_v2/tfmodel.py": "12552bb263a25804f17ee4f72ca66b1ae0bac7d4f713b51b561289bd8dfca325",
    "modules/bandit_v2/utils.py": "b8536989fda1495dca2684806ff1f8335cb3a09145f43c2c212919210b01e33c",
    "utils/logger.py": "ed90f9f4b47d811a2122c1edd8d890b16d847d7dbfcbd4454a9d43ac8aa5be9d",
    "utils/utils.py": "0fa1f37975a23fb01b2a3cf03afae8d67534ef72abc713e25eb3b252f9a58533",
}
EMPTY_FILES = ["modules/__init__.py", "utils/__init__.py"]

FFMPEG_URL = "https://github.com/GyanD/codexffmpeg/releases/download/8.0.1/ffmpeg-8.0.1-essentials_build.zip"
FFMPEG_SHA256 = "e2aaeaa0fdbc397d4794828086424d4aaa2102cef1fb6874f6ffd29c0b88b673"
FFMPEG_SIZE = 106259850

# "Remastering Divide and Remaster" model weights by Watcharasupat, Wu and Orife (CC BY-SA 4.0).
MODEL_URL = "https://zenodo.org/api/records/12701995/files/checkpoint-multi.ckpt/content"
MODEL_SHA256 = "abcfccf65446752a057f4a302c941479a54b7560ebf8d7bca039d2ea98e64cfc"
MODEL_SIZE = 446680129

_out = sys.stdout


def emit(event, **data):
    data["event"] = event
    _out.write(json.dumps(data) + "\n")
    _out.flush()


def sha256_of(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def fetch_small(url):
    last = None
    for attempt in range(4):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=HEADERS), timeout=60) as r:
                return r.read()
        except (urllib.error.URLError, OSError) as e:
            last = e
            time.sleep(2 * (attempt + 1))
    raise RuntimeError(f"Couldn't download {url} ({last}). Check your internet connection.")


def download(step, url, dest, sha256, size):
    """Download url to dest with progress, resuming a partial file, and check its SHA-256."""
    part = dest + ".part"
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    last = None
    for attempt in range(6):
        have = os.path.getsize(part) if os.path.exists(part) else 0
        if have > size:
            os.remove(part)
            have = 0
        if have == size:
            break
        try:
            headers = dict(HEADERS)
            if have:
                headers["Range"] = f"bytes={have}-"
            with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=60) as r:
                if have and r.status != 206:  # the server sent the whole file again
                    have = 0
                shown = 0.0
                with open(part, "ab" if have else "wb") as f:
                    while True:
                        chunk = r.read(1 << 18)
                        if not chunk:
                            break
                        f.write(chunk)
                        have += len(chunk)
                        if time.time() - shown > 0.25:
                            shown = time.time()
                            emit("progress", id=step, done=have, total=size)
        except (urllib.error.URLError, OSError) as e:
            last = e
            time.sleep(min(3 * (attempt + 1), 15))
    if not os.path.exists(part) or os.path.getsize(part) != size:
        raise RuntimeError(f"The download kept getting interrupted ({last}). Check your internet connection and try again.")
    emit("progress", id=step, done=size, total=size)
    if sha256_of(part) != sha256:
        os.remove(part)
        raise RuntimeError("A downloaded file was damaged on the way. Try again.")
    os.replace(part, dest)


def step_code(root):
    msst = os.path.join(root, "msst")
    for rel in EMPTY_FILES:
        path = os.path.join(msst, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        open(path, "a").close()
    for i, (rel, sha) in enumerate(ENGINE_FILES.items()):
        path = os.path.join(msst, rel)
        if not (os.path.exists(path) and sha256_of(path) == sha):
            data = fetch_small(MSST_RAW + rel)
            if hashlib.sha256(data).hexdigest() != sha:
                raise RuntimeError(f"{rel} is not the expected version. Try again later.")
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "wb") as f:
                f.write(data)
        emit("progress", id="code", done=i + 1, total=len(ENGINE_FILES))


def tool_works(path):
    try:
        return subprocess.run([path, "-version"], capture_output=True, creationflags=NO_WINDOW, timeout=30).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def step_ffmpeg(root):
    out = os.path.join(root, "ffmpeg")
    tools = [os.path.join(out, "ffmpeg.exe"), os.path.join(out, "ffprobe.exe")]
    if all(tool_works(t) for t in tools):
        return
    archive = os.path.join(root, "downloads", "ffmpeg.zip")
    download("ffmpeg", FFMPEG_URL, archive, FFMPEG_SHA256, FFMPEG_SIZE)
    os.makedirs(out, exist_ok=True)
    with zipfile.ZipFile(archive) as z:
        for name in z.namelist():
            base = name.rsplit("/", 1)[-1]
            if base in ("ffmpeg.exe", "ffprobe.exe") and "/bin/" in "/" + name:
                with z.open(name) as src, open(os.path.join(out, base), "wb") as dst:
                    while True:
                        chunk = src.read(1 << 20)
                        if not chunk:
                            break
                        dst.write(chunk)
    os.remove(archive)
    if not all(tool_works(t) for t in tools):
        raise RuntimeError("ffmpeg was downloaded but doesn't run on this PC.")


def step_model(root):
    dst = os.path.join(root, "model", "bandit_v2_dnr_multi.ckpt")
    if os.path.exists(dst) and os.path.getsize(dst) > 100_000_000:
        return
    src = os.path.join(root, "downloads", "checkpoint-multi.ckpt")
    download("model", MODEL_URL, src, MODEL_SHA256, MODEL_SIZE)
    import torch

    # The published file is a full training checkpoint. Only the model's own weights are needed.
    try:
        ck = torch.load(src, map_location="cpu", weights_only=True)
    except Exception:
        # It holds more than plain tensors. It was verified against the known SHA-256 just above,
        # so this is the published file and nothing else.
        ck = torch.load(src, map_location="cpu", weights_only=False)
    weights = {k[len("model."):]: v for k, v in ck["state_dict"].items() if k.startswith("model.")}
    if not weights:
        raise RuntimeError("The downloaded model isn't in the expected format.")
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    torch.save(weights, dst + ".part")
    os.replace(dst + ".part", dst)
    os.remove(src)


def step_test(root):
    """Load the model the way the app does and separate three seconds of noise."""
    msst = os.path.join(root, "msst")
    sys.path.insert(0, root)
    os.chdir(msst)
    import numpy as np
    import torch

    import bandit_worker

    model, config, device, gpu = bandit_worker.load_model(
        msst, os.path.join(root, "model", "bandit_v2_dnr_multi.ckpt"), os.path.join(root, "bandit_v2_dnr_multi.yaml"), "auto")
    from utils.utils import demix

    noise = (np.random.default_rng(0).standard_normal((2, 48000 * 3)) * 0.05).astype(np.float32)
    stems = demix(config, model, noise, device, model_type="bandit_v2", callback=bandit_worker.ProgressHook(lambda p: None))
    for name in ("speech", "music", "sfx"):
        if name not in stems or stems[name].shape != noise.shape or not np.isfinite(stems[name]).all():
            raise RuntimeError("The engine started but gave an unexpected result.")
    return gpu, device.type == "cuda", torch.__version__


def main():
    global _out
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", required=True)
    args = ap.parse_args()
    root = os.path.abspath(args.root)

    # Keep stray prints from libraries off the JSON channel.
    _out = sys.stdout
    sys.stdout = sys.stderr

    step = "code"
    try:
        for step, run in (("code", step_code), ("ffmpeg", step_ffmpeg), ("model", step_model)):
            emit("step", id=step, state="start")
            run(root)
            emit("step", id=step, state="done")
        step = "test"
        emit("step", id=step, state="start")
        device, on_gpu, torch_version = step_test(root)
        emit("step", id=step, state="done")
        try:
            os.rmdir(os.path.join(root, "downloads"))
        except OSError:
            pass
        emit("done", device=device, gpu=on_gpu, torch=torch_version)
    except Exception as e:
        emit("fatal", id=step, message=str(e) or type(e).__name__, detail=traceback.format_exc())
        sys.exit(1)


if __name__ == "__main__":
    main()
