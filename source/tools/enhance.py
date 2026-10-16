"""Enhances the game's textures outside the game: the pages the game uses are dumped by the player (XG_TEX_DUMP=<folder>), this
tool upscales every page 4x with an AI upscaler on the graphics card (Upscayl's bundled Real-ESRGAN, installed on this
machine) and writes the replacement pictures the player loads (XG_TEX_REPLACE=<folder>).

    python enhance.py <dump folder> <replace folder> [--model digital-art-4x] [--bin <upscayl-bin.exe>] [--models <folder>]
                      [--limit N] [--tile 0]

Each page is a 256 x 256 picture with transparency (what the game's palette gives it); the colours of transparent texels are
filled from their neighbours before the upscaler sees them (so no dark fringes appear), and the transparency itself is
carried over smoothly. The result is a raw 1024 x 1024 RGBA file named after the page's key. Pages already done are skipped,
so the tool can be run again after more of the game has been played with the dump on.
"""
import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import time

import numpy as np
from PIL import Image, ImageFilter

DEFAULT_BIN = r"C:\Program Files\Upscayl\resources\bin\upscayl-bin.exe"
DEFAULT_MODELS = r"C:\Program Files\Upscayl\resources\models"
SIZE = 256
OUT = 1024


def fill_transparent(rgb: np.ndarray, alpha: np.ndarray) -> np.ndarray:
    """RGB with the colours of transparent texels taken from the nearest opaque ones (a few passes of spreading)."""
    out = rgb.copy()
    known = alpha > 0
    if not known.any():
        out[:] = 0
        return out
    if known.all():
        return out
    mean = rgb[known].mean(axis=0)
    for _ in range(64):
        if known.all():
            break
        new_known = known.copy()
        acc = np.zeros_like(out, dtype=np.float32)
        cnt = np.zeros(known.shape, dtype=np.float32)
        for dy, dx in ((1, 0), (-1, 0), (0, 1), (0, -1)):
            sh = np.roll(np.roll(out, dy, axis=0), dx, axis=1).astype(np.float32)
            sk = np.roll(np.roll(known, dy, axis=0), dx, axis=1)
            acc += sh * sk[..., None]
            cnt += sk
        grow = (~known) & (cnt > 0)
        if not grow.any():
            break
        out[grow] = (acc[grow] / cnt[grow][..., None]).astype(np.uint8)
        new_known[grow] = True
        known = new_known
    out[~known] = mean.astype(np.uint8)
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("dump")
    ap.add_argument("replace")
    ap.add_argument("--model", default="realesrgan-x4plus-anime")
    ap.add_argument("--bin", default=DEFAULT_BIN)
    ap.add_argument("--models", default=DEFAULT_MODELS)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--tile", type=int, default=0)
    ap.add_argument("--gpu", default="")
    ap.add_argument("--blend", type=float, default=0.0, help="share of the plain (non-AI) enlargement mixed back in, 0..1")
    ap.add_argument("--fix", type=float, default=16.0, help="radius of the colour correction (the AI result takes the plain enlargement's low-frequency colour); 0 off")
    a = ap.parse_args()

    if not os.path.exists(a.bin):
        print("the upscaler is not at", a.bin, file=sys.stderr)
        return 2
    os.makedirs(a.replace, exist_ok=True)
    todo = []
    for name in sorted(os.listdir(a.dump)):
        if not name.lower().endswith(".png"):
            continue
        key = name[:-4]
        if os.path.exists(os.path.join(a.replace, key + ".rgba")):
            continue
        todo.append(key)
        if a.limit and len(todo) >= a.limit:
            break
    print("%d pages to do" % len(todo))
    if not todo:
        return 0

    with tempfile.TemporaryDirectory() as tmp:
        tin = os.path.join(tmp, "in")
        tout = os.path.join(tmp, "out")
        os.makedirs(tin)
        os.makedirs(tout)
        alphas = {}
        for key in todo:
            im = Image.open(os.path.join(a.dump, key + ".png")).convert("RGBA")
            arr = np.asarray(im)
            rgb = fill_transparent(arr[..., :3], arr[..., 3])
            Image.fromarray(rgb, "RGB").save(os.path.join(tin, key + ".png"))
            alphas[key] = arr[..., 3]
        cmd = [a.bin, "-i", tin, "-o", tout, "-n", a.model, "-m", a.models, "-s", "4", "-f", "png", "-t", str(a.tile)]
        if a.gpu:
            cmd += ["-g", a.gpu]
        t0 = time.time()
        print("running", " ".join(cmd))
        r = subprocess.run(cmd, capture_output=True, text=True)
        if r.returncode != 0:
            print(r.stdout[-2000:], r.stderr[-2000:], file=sys.stderr)
            return r.returncode
        print("upscaled in %.1f s" % (time.time() - t0))
        done = 0
        for key in todo:
            p = os.path.join(tout, key + ".png")
            if not os.path.exists(p):
                print("missing output for", key, file=sys.stderr)
                continue
            up = Image.open(p).convert("RGB")
            if up.size != (OUT, OUT):
                up = up.resize((OUT, OUT), Image.LANCZOS)
            al = Image.fromarray(alphas[key], "L").resize((OUT, OUT), Image.BICUBIC)
            ai = np.asarray(up).astype(np.float32)
            if a.fix > 0 or a.blend > 0:
                plain_im = Image.open(os.path.join(tin, key + ".png")).convert("RGB").resize((OUT, OUT), Image.LANCZOS)
                plain = np.asarray(plain_im).astype(np.float32)
                if a.fix > 0:
                    # the AI keeps its detail but takes the colour of the plain enlargement at low frequencies
                    lo_p = np.asarray(plain_im.filter(ImageFilter.GaussianBlur(a.fix))).astype(np.float32)
                    lo_a = np.asarray(Image.fromarray(np.clip(ai, 0, 255).astype(np.uint8), "RGB").filter(ImageFilter.GaussianBlur(a.fix))).astype(np.float32)
                    ai = ai + (lo_p - lo_a)
                if a.blend > 0:
                    ai = ai * (1.0 - a.blend) + plain * a.blend
            rgba = np.dstack([np.clip(ai, 0, 255).astype(np.uint8), np.asarray(al)])
            with open(os.path.join(a.replace, key + ".rgba"), "wb") as f:
                f.write(rgba.astype(np.uint8).tobytes())
            done += 1
        print("%d replacement pictures written to %s" % (done, a.replace))
    return 0


if __name__ == "__main__":
    sys.exit(main())
