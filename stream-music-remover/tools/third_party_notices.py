"""Write THIRD-PARTY-NOTICES.txt: the licences of everything that is built into the .exe.

Run from the stream-music-remover folder before publishing a release:

    python tools/third_party_notices.py

It asks cargo which libraries go into the Windows release build (test-only libraries are left
out) and copies each one's licence files from cargo's local package folder. Identical licence
texts are printed once, with the list of libraries they cover.
"""

import collections
import glob
import os
import re
import subprocess
import sys

TARGET = "x86_64-pc-windows-msvc"
OWN_PACKAGE = "stream-music-remover"
HERE = os.path.dirname(os.path.abspath(__file__))
CRATE_DIR = os.path.dirname(HERE)
OUTPUT = os.path.join(os.path.dirname(CRATE_DIR), "THIRD-PARTY-NOTICES.txt")
LICENCE_FILE = re.compile(r"^(licen[sc]e|copying|notice|unlicense|copyright|ofl|ufl)([-_. ].*)?$", re.IGNORECASE)

INTRO = """\
THIRD-PARTY NOTICES for Stream Music Remover
============================================

Stream Music Remover is built with, and at setup downloads, software and data made by others.
This file lists them with their licences.


1. Downloaded to your PC during setup (not part of the app's own download)
---------------------------------------------------------------------------

The app's setup fetches these from their publishers. They are listed here for credit and so you
know what ends up on your PC.

  Bandit v2 model weights ("Remastering Divide and Remaster: A Cinematic Audio Source Separation
    Dataset with Multilingual Support") by Karn Watcharasupat, Chih-Wei Wu and Iroro Orife.
    Creative Commons Attribution-ShareAlike 4.0.  https://zenodo.org/records/12701995
    Setup keeps only the model's own weights from the published training checkpoint.

  Bandit v2 model code and separation utilities from MSST-WebUI by SUC-DriverOld and contributors.
    GNU Affero General Public License v3.0.  https://github.com/SUC-DriverOld/MSST-WebUI
    (The model code originates from https://github.com/kwatcharasupat/bandit-v2, Apache-2.0, and
    https://github.com/ZFTurbo/Music-Source-Separation-Training, MIT.)

  FFmpeg, "essentials" build by Gyan Doshi.  GNU General Public License v3.0.
    https://ffmpeg.org  https://www.gyan.dev/ffmpeg/builds/

  PyTorch and torchaudio (BSD-3-Clause), with NVIDIA CUDA libraries under NVIDIA's licence when
    the graphics-card build is installed.  https://pytorch.org

  Python 3.10 (Python Software Foundation License), installed with uv (MIT OR Apache-2.0), and
    the Python packages listed in worker/requirements.txt, each under its own licence.


2. Model settings file
----------------------

worker/bandit_v2_dnr_multi.yaml (built into the app) is configs/config_dnr_bandit_v2_mus64.yaml
from https://github.com/ZFTurbo/Music-Source-Separation-Training, used under the MIT License:

MIT License

Copyright (c) 2024 Roman Solovyev (ZFTurbo)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""


def release_packages():
    """(name, version, licence expression, repository) of every library in the Windows release build."""
    out = subprocess.run(
        ["cargo", "tree", "--edges", "normal,build", "--target", TARGET, "--prefix", "none", "--format", "{p}\t{l}\t{r}"],
        cwd=CRATE_DIR, capture_output=True, text=True, encoding="utf-8", check=True).stdout
    packages = set()
    for line in out.splitlines():
        parts = line.replace(" (*)", "").split("\t")  # "(*)" marks a library cargo already listed
        if len(parts) < 3:
            continue
        head = parts[0].split()
        if len(head) < 2 or head[0] == OWN_PACKAGE:
            continue
        name, version = head[0], head[1].lstrip("v")
        packages.add((name, version, parts[1].strip() or "see the package", parts[2].strip()))
    return sorted(packages, key=lambda p: (p[0].lower(), p[1]))


def licence_texts(name, version):
    """Licence files shipped inside a library's package: [(file name, text)]."""
    home = os.environ.get("CARGO_HOME") or os.path.join(os.path.expanduser("~"), ".cargo")
    found = []
    for root in glob.glob(os.path.join(home, "registry", "src", "*", f"{name}-{version}")):
        for folder, dirs, files in os.walk(root):
            depth = os.path.relpath(folder, root).count(os.sep) + (folder != root)
            if depth >= 3:
                dirs[:] = []
            dirs[:] = [d for d in dirs if d not in ("target", "tests", "benches", "examples", ".git")]
            for file in sorted(files):
                if LICENCE_FILE.match(os.path.splitext(file)[0]) or LICENCE_FILE.match(file):
                    path = os.path.join(folder, file)
                    if os.path.getsize(path) > 200_000:
                        continue
                    with open(path, encoding="utf-8", errors="replace") as f:
                        text = f.read().replace("\r\n", "\n").strip()
                    if text:
                        found.append((os.path.relpath(path, root).replace(os.sep, "/"), text))
        break
    return found


def main():
    packages = release_packages()
    if not packages:
        sys.exit("cargo tree listed no libraries")
    by_text = collections.OrderedDict()  # normalised text -> (text, [(library, file)])
    without = []
    for name, version, licence, _ in packages:
        texts = licence_texts(name, version)
        if not texts:
            without.append(f"{name} {version} ({licence})")
        for file, text in texts:
            key = re.sub(r"\s+", " ", text)
            by_text.setdefault(key, (text, []))[1].append(f"{name} {version} [{file}]")

    lines = [INTRO, "", "3. Libraries built into the app", "-" * 31, "",
             f"The app's .exe contains these {len(packages)} Rust libraries. Their licence texts follow in section 4.", ""]
    width = max(len(f"{n} {v}") for n, v, _, _ in packages)
    for name, version, licence, repo in packages:
        lines.append(f"  {f'{name} {version}':{width}}  {licence}" + (f"  {repo}" if repo else ""))
    if without:
        lines += ["", "These packages don't ship a licence file; the licence named above applies, and its",
                  "standard text is available at https://spdx.org/licenses/ :", ""]
        lines += [f"  {w}" for w in without]
    lines += ["", "", "4. Licence texts", "-" * 16]
    ordered = sorted(by_text.values(), key=lambda item: (-len(item[1]), item[1][0].lower()))
    for text, users in ordered:
        lines += ["", "=" * 100, "Applies to:"]
        lines += [f"  {u}" for u in users]
        lines += ["=" * 100, "", text, ""]
    with open(OUTPUT, "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines).rstrip() + "\n")
    print(f"wrote {OUTPUT}: {len(packages)} libraries, {len(by_text)} distinct licence texts, {len(without)} without a licence file, {os.path.getsize(OUTPUT) // 1024} KB")


if __name__ == "__main__":
    main()
