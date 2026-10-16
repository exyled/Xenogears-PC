"""Upscales the game's movies 4x (320 x 224 -> 1280 x 896) with the Stable Diffusion x4 upscaler (generative), one frame at a time
with a fixed seed (so the invented detail does not shimmer from frame to frame), keeps the original's brightness and colour at
coarse scale, and packs each movie as <entry>.xgv (the format tools/movies.py writes and the player reads). Resumes: finished frames
and movies are kept.

    ai\venv\Scripts\python.exe tools\movies_ai.py <GameData/movies> <output folder> --model ai\models\sd-x4-upscaler [--steps 10]
"""
import argparse, os, shutil, struct, subprocess, sys, time
import numpy as np
import torch
from PIL import Image, ImageFilter

def frame_numbers(path):
    out, last = [], None
    with open(path, "rb") as f:
        while True:
            s = f.read(2352)
            if len(s) < 2352:
                break
            if s[18] & 4 or struct.unpack_from("<H", s, 26)[0] != 0x8001:
                continue
            n = struct.unpack_from("<I", s, 24 + 8)[0]
            if n != last:
                out.append(n)
                last = n
    return out

def tone(sd, src, r):
    n = src.resize(sd.size, Image.BICUBIC)
    lo_n = np.asarray(n.filter(ImageFilter.GaussianBlur(r))).astype(np.float32)
    lo_s = np.asarray(sd.filter(ImageFilter.GaussianBlur(r))).astype(np.float32)
    return Image.fromarray(np.clip(np.asarray(sd).astype(np.float32) + lo_n - lo_s, 0, 255).astype(np.uint8))

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("movies"); ap.add_argument("out")
    ap.add_argument("--model", required=True)
    ap.add_argument("--ffmpeg", default=r"C:\ffmpeg\ffmpeg.exe")
    ap.add_argument("--steps", type=int, default=10)
    ap.add_argument("--quality", type=int, default=95)
    ap.add_argument("--first", default="0014,0017", help="movies done first (comma list)")
    ap.add_argument("--limit", type=int, default=0, help="stop after this many frames in total (testing)")
    a = ap.parse_args()
    from diffusers import StableDiffusionUpscalePipeline
    pipe = StableDiffusionUpscalePipeline.from_pretrained(a.model, torch_dtype=torch.float16, variant="fp16").to("cuda")
    pipe.enable_attention_slicing()
    pipe.vae.enable_tiling()
    pipe.set_progress_bar_config(disable=True)
    os.makedirs(a.out, exist_ok=True)
    names = sorted(n[:-4] for n in os.listdir(a.movies) if n.lower().endswith(".str"))
    first = [x for x in a.first.split(",") if x in names]
    names = first + [n for n in names if n not in first]
    t0 = time.time()
    total = 0
    for entry in names:
        dst = os.path.join(a.out, entry + ".xgv")
        if os.path.exists(dst):
            continue
        src = os.path.join(a.movies, entry + ".str")
        work = os.path.join(a.out, "_work_" + entry)
        din, dout = os.path.join(work, "in"), os.path.join(work, "out")
        os.makedirs(din, exist_ok=True); os.makedirs(dout, exist_ok=True)
        if not os.listdir(din):
            subprocess.run([a.ffmpeg, "-v", "error", "-i", src, "-vsync", "0", os.path.join(din, "%06d.png")], check=False)
        frames = sorted(os.listdir(din))
        nums = frame_numbers(src)
        if len(nums) != len(frames):
            print("%s: %d frames decoded but %d frame numbers; numbering by order" % (entry, len(frames), len(nums)), flush=True)
            nums = list(range(1, len(frames) + 1))
        for i, fn in enumerate(frames):
            o = os.path.join(dout, fn[:-4] + ".jpg")
            if os.path.exists(o):
                continue
            im = Image.open(os.path.join(din, fn)).convert("RGB")
            g = torch.Generator("cuda").manual_seed(7)
            r = pipe(prompt="", image=im, num_inference_steps=a.steps, guidance_scale=1.0, noise_level=0, generator=g).images[0]
            tone(r, im, 6).save(o + ".tmp", "JPEG", quality=a.quality, subsampling=0)
            os.replace(o + ".tmp", o)
            total += 1
            if total % 50 == 0:
                print("%s: %d / %d  (%.0f s)" % (entry, i + 1, len(frames), time.time() - t0), flush=True)
            if a.limit and total >= a.limit:
                return 0
        blobs = [open(os.path.join(dout, fn[:-4] + ".jpg"), "rb").read() for fn in frames]
        w, h = Image.open(os.path.join(dout, frames[0][:-4] + ".jpg")).size
        head = 12 + 4 * len(blobs) + 4 * (len(blobs) + 1)
        offs, pos = [], head
        for b in blobs:
            offs.append(pos); pos += len(b)
        offs.append(pos)
        with open(dst + ".part", "wb") as f:
            f.write(b"XGV1" + struct.pack("<IHH", len(blobs), w, h))
            f.write(struct.pack("<%dI" % len(nums), *nums))
            f.write(struct.pack("<%dI" % len(offs), *offs))
            for b in blobs:
                f.write(b)
        os.replace(dst + ".part", dst)
        shutil.rmtree(work, ignore_errors=True)
        print("%s: %d frames %dx%d, %.1f MB (%.0f s)" % (entry, len(blobs), w, h, pos / 1048576, time.time() - t0), flush=True)
    print("ALL DONE", flush=True)

if __name__ == "__main__":
    sys.exit(main())
