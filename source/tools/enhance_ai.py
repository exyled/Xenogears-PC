"""Enhances the game's textures with the newer PyTorch super-resolution models (RGT, DAT2, HAT ... through spandrel) on the
graphics card. Same input and output as enhance.py (dump folder in, TextureReplace folder out), run with the python of the
virtual environment in ../../../ai:

    ai\\venv\\Scripts\\python.exe tools\\enhance_ai.py <dump folder> <replace folder> --model <file.pth> [--limit N] [--fix 16] [--blend 0]
    ai\\venv\\Scripts\\python.exe tools\\enhance_ai.py --compare <dump folder> <out.png> --models a.pth b.pth ... --keys k1 k2 ...
"""
import argparse
import os
import sys
import time

import numpy as np
import torch
from PIL import Image, ImageFilter, ImageDraw
from spandrel import ModelLoader

SIZE = 256
OUT = 2048  # 8x: two passes of a 4x model


def fill_transparent(rgb, alpha):
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
        known = known | grow
    out[~known] = mean.astype(np.uint8)
    return out


class Upscaler:
    def __init__(self, path, half=True):
        self.desc = ModelLoader().load_from_file(path)
        self.dev = torch.device("cuda" if torch.cuda.is_available() else "cpu")
        self.half = half and self.dev.type == "cuda" and self.desc.supports_half
        self.desc.to(self.dev)
        self.desc.eval()
        if self.half:
            self.desc.half()
        self.scale = self.desc.scale

    @torch.no_grad()
    def __call__(self, rgb: np.ndarray, tile=128, pad=16, skip=None) -> np.ndarray:
        """rgb: h x w x 3 uint8 -> (h*scale) x (w*scale) x 3 uint8, tiled with overlap so that big models fit."""
        h, w, _ = rgb.shape
        s = self.scale
        x = torch.from_numpy(rgb).permute(2, 0, 1).float().div(255.0).unsqueeze(0)
        x = x.to(self.dev)
        if self.half:
            x = x.half()
        out = torch.zeros((1, 3, h * s, w * s), device=self.dev, dtype=torch.float32)
        for ty in range(0, h, tile):
            for tx in range(0, w, tile):
                y0, y1 = max(ty - pad, 0), min(ty + tile + pad, h)
                x0, x1 = max(tx - pad, 0), min(tx + tile + pad, w)
                t = x[:, :, y0:y1, x0:x1]
                core = x[:, :, ty:ty + tile, tx:tx + tile]
                if (skip is not None and skip[ty:ty + tile, tx:tx + tile].all()) or float(core.max() - core.min()) < 0.008:
                    # a flat (usually empty) tile: nothing to invent, and it is most of many pages
                    th, tw = min(tile, h - ty) * s, min(tile, w - tx) * s
                    out[:, :, ty * s:ty * s + th, tx * s:tx * s + tw] = torch.nn.functional.interpolate(core.float(), scale_factor=s, mode="bicubic", align_corners=False).clamp(0, 1)[:, :, :th, :tw]
                    continue
                r = self.desc(t).float()
                oy0, ox0 = (ty - y0) * s, (tx - x0) * s
                th, tw = min(tile, h - ty) * s, min(tile, w - tx) * s
                out[:, :, ty * s:ty * s + th, tx * s:tx * s + tw] = r[:, :, oy0:oy0 + th, ox0:ox0 + tw]
        res = out.clamp(0, 1).mul(255.0).round().byte().squeeze(0).permute(1, 2, 0).cpu().numpy()
        return res


def postprocess(ai, plain_im, fix, blend):
    ai = ai.astype(np.float32)
    plain = np.asarray(plain_im).astype(np.float32)
    if fix > 0:
        lo_p = np.asarray(plain_im.filter(ImageFilter.GaussianBlur(fix * OUT / 1024))).astype(np.float32)
        lo_a = np.asarray(Image.fromarray(np.clip(ai, 0, 255).astype(np.uint8), "RGB").filter(ImageFilter.GaussianBlur(fix * OUT / 1024))).astype(np.float32)
        ai = ai + (lo_p - lo_a)
    if blend > 0:
        ai = ai * (1.0 - blend) + plain * blend
    return np.clip(ai, 0, 255).astype(np.uint8)


def dedither(rgb, s):
    """The console's pictures are dithered; a model reads that noise as texture and keeps it. Blur it away where it is only noise
    (the median and the blur agree) and keep real lines and edges (they disagree), so the model has clean shapes to paint."""
    o = rgb.astype(np.float32)
    im = Image.fromarray(rgb, "RGB")
    g = np.asarray(im.filter(ImageFilter.GaussianBlur(s))).astype(np.float32)
    m = np.asarray(im.filter(ImageFilter.MedianFilter(3))).astype(np.float32)
    w = np.clip(1.0 - np.abs(m - g).mean(2, keepdims=True) / 18.0, 0, 1)
    return np.ascontiguousarray(np.clip(o * (1 - w) + g * w, 0, 255).astype(np.uint8))


DEDITHER = 0.0
PASSES = 2


def epx(p):
    """EPX (Scale2x) on packed pixels: each pixel is compared with its four neighbours, so edges stay hard and diagonals are
    smoothed. Three rounds make 8x."""
    q = np.pad(p, 1, mode="edge")
    P = q[1:-1, 1:-1]; A = q[:-2, 1:-1]; B = q[1:-1, 2:]; C = q[1:-1, :-2]; D = q[2:, 1:-1]
    o = np.empty((p.shape[0] * 2, p.shape[1] * 2), p.dtype)
    o[0::2, 0::2] = np.where((C == A) & (C != D) & (A != B), A, P)
    o[0::2, 1::2] = np.where((A == B) & (A != C) & (B != D), B, P)
    o[1::2, 0::2] = np.where((D == C) & (D != B) & (C != A), C, P)
    o[1::2, 1::2] = np.where((B == D) & (B != A) & (D != C), D, P)
    return o


def epx8(rgba):
    p = (rgba[..., 0].astype(np.uint32) << 24) | (rgba[..., 1].astype(np.uint32) << 16) | (rgba[..., 2].astype(np.uint32) << 8) | rgba[..., 3]
    for _ in range(3):
        p = epx(p)
    return np.dstack([(p >> 24) & 255, (p >> 16) & 255, (p >> 8) & 255, p & 255]).astype(np.uint8)


BLOCK = 16


def crisp_blocks(rgba):
    """Which 16 x 16 texel blocks are hard-edged art (a few flat colours: text, interface, sprites, empty space): they get the
    edge-aware algorithm, which keeps them sharp; the rest is photo-like or dithered and gets the model."""
    h, w, _ = rgba.shape
    n = h // BLOCK
    k = (rgba[..., 0].astype(np.uint32) << 16) | (rgba[..., 1].astype(np.uint32) << 8) | rgba[..., 2]
    k = np.where(rgba[..., 3] > 0, k, 0xFFFFFFFF)
    out = np.zeros((n, n), bool)
    for by in range(n):
        for bx in range(n):
            b = k[by * BLOCK:(by + 1) * BLOCK, bx * BLOCK:(bx + 1) * BLOCK]
            u = np.unique(b)
            if len(u) <= 1:
                out[by, bx] = True
                continue
            flat = ((b[:, 1:] == b[:, :-1]).mean() + (b[1:, :] == b[:-1, :]).mean()) / 2
            out[by, bx] = len(u) <= 8 and flat >= 0.55
    # only blocks whose neighbours are crisp too, so the two methods do not meet in the middle of a picture
    e = out.copy()
    e[1:, :] &= out[:-1, :]; e[:-1, :] &= out[1:, :]
    e[:, 1:] &= out[:, :-1]; e[:, :-1] &= out[:, 1:]
    return e


def upscale_page(up: Upscaler, path, fix, blend):
    im0 = Image.open(path).convert("RGBA")
    a0 = np.asarray(im0)
    crisp = crisp_blocks(a0)
    skip = np.kron(crisp, np.ones((BLOCK, BLOCK), bool))
    res = upscale_ai(up, a0, fix, blend, skip)
    if crisp.any():
        e = epx8(np.ascontiguousarray(a0))
        if e.shape[0] != OUT:
            e = np.asarray(Image.fromarray(e, "RGBA").resize((OUT, OUT), Image.NEAREST))
        m = np.kron(crisp, np.ones((OUT // crisp.shape[0], OUT // crisp.shape[0]), bool))
        res = np.where(m[..., None], e, res)
    return res


def upscale_ai(up: Upscaler, a0, fix, blend, skip=None):
    return _upscale_page(up, a0, fix, blend, skip)


def _upscale_page(up: Upscaler, arr, fix, blend, skip):
    rgb = fill_transparent(arr[..., :3], arr[..., 3])
    if DEDITHER > 0:
        rgb = dedither(rgb, DEDITHER)
    res = up(rgb, skip=skip)
    if PASSES > 1:
        sk2 = None if skip is None else np.kron(skip, np.ones((4, 4), bool))
        res = up(res, skip=sk2)
    if res.shape[0] != OUT:
        res = np.asarray(Image.fromarray(res, "RGB").resize((OUT, OUT), Image.LANCZOS))
    plain_im = Image.fromarray(rgb, "RGB").resize((OUT, OUT), Image.LANCZOS)
    res = postprocess(res, plain_im, fix, blend)
    al = Image.fromarray(arr[..., 3], "L").resize((OUT, OUT), Image.BICUBIC)
    return np.dstack([res, np.asarray(al)])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dump")
    ap.add_argument("replace")
    ap.add_argument("--model", help="the .pth/.safetensors file")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--fix", type=float, default=16.0)
    ap.add_argument("--blend", type=float, default=0.0)
    ap.add_argument("--compare", action="store_true")
    ap.add_argument("--passes", type=int, default=2, help="2: two passes of the 4x model, 8x in all (2048 x 2048 pages)")
    ap.add_argument("--dedither", type=float, default=0.0, help="blur radius that removes the console's dither noise before upscaling (1.0 works)")
    ap.add_argument("--models", nargs="*", default=[])
    ap.add_argument("--keys", nargs="*", default=[])
    a = ap.parse_args()
    global DEDITHER
    DEDITHER = a.dedither
    global PASSES
    PASSES = a.passes

    if a.compare:
        # dump = the dump folder, replace = the picture to write
        cols = []
        names = ["original (x4 nearest)"]
        base = [Image.open(os.path.join(a.dump, k + ".png")).convert("RGBA") for k in a.keys]
        cols.append([im.resize((OUT, OUT), Image.NEAREST) for im in base])
        for mp in a.models:
            up = Upscaler(mp)
            names.append(os.path.basename(mp))
            t0 = time.time()
            cols.append([Image.fromarray(upscale_page(up, os.path.join(a.dump, k + ".png"), a.fix, a.blend), "RGBA") for k in a.keys])
            print(names[-1], "%.1f s for %d pages" % (time.time() - t0, len(a.keys)))
            del up
            torch.cuda.empty_cache()
        th = 384
        sheet = Image.new("RGB", (len(cols) * (th + 4), len(a.keys) * (th + 4) + 16), (50, 50, 50))
        d = ImageDraw.Draw(sheet)
        for c, col in enumerate(cols):
            d.text((c * (th + 4) + 2, 2), names[c][:34], fill=(255, 255, 255))
            for r, im in enumerate(col):
                bg = Image.new("RGBA", im.size, (90, 0, 90, 255))
                bg.alpha_composite(im.convert("RGBA"))
                sheet.paste(bg.convert("RGB").resize((th, th), Image.LANCZOS), (c * (th + 4), 16 + r * (th + 4)))
        sheet.save(a.replace)
        return 0

    os.makedirs(a.replace, exist_ok=True)
    up = Upscaler(a.model)
    todo = [n[:-4] for n in sorted(os.listdir(a.dump)) if n.lower().endswith(".png") and not os.path.exists(os.path.join(a.replace, n[:-4] + ".rgba"))]
    if a.limit:
        todo = todo[:a.limit]
    print("%d pages to do with %s" % (len(todo), os.path.basename(a.model)))
    t0 = time.time()
    for i, key in enumerate(todo):
        rgba = upscale_page(up, os.path.join(a.dump, key + ".png"), a.fix, a.blend)
        with open(os.path.join(a.replace, key + ".rgba"), "wb") as f:
            f.write(rgba.tobytes())
        if (i + 1) % 25 == 0:
            print("  %d / %d  (%.1f s)" % (i + 1, len(todo), time.time() - t0))
    print("%d pages written to %s in %.1f s" % (len(todo), a.replace, time.time() - t0))
    return 0


if __name__ == "__main__":
    sys.exit(main())
