"""Upscales the game's movies 4x (320 x 224 -> 1280 x 896) with Real-ESRGAN General v3 (the video model of Upscayl's set) and packs
each one as <entry>.xgv for the player:

    python tools/movies.py <GameData/movies folder> <output folder> [--ffmpeg ffmpeg.exe] [--bin upscayl-bin.exe] [--models folder]

.xgv: "XGV1", u32 count, u16 width, u16 height, u32 frame numbers[count] (the numbers in the movie's own sector headers),
u32 offsets[count + 1] from the start of the file, then the frames as baseline JPEG (4:4:4). Resumes: finished movies are kept.
"""
import argparse, os, shutil, struct, subprocess, sys, time
from PIL import Image

def frame_numbers(path):
    out, last = [], None
    with open(path, "rb") as f:
        while True:
            s = f.read(2352)
            if len(s) < 2352:
                break
            if s[18] & 4:
                continue
            if struct.unpack_from("<H", s, 26)[0] != 0x8001:
                continue
            n = struct.unpack_from("<I", s, 24 + 8)[0]
            if n != last:
                out.append(n)
                last = n
    return out

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("movies"); ap.add_argument("out")
    ap.add_argument("--ffmpeg", default=r"C:\ffmpeg\ffmpeg.exe")
    ap.add_argument("--bin", default=r"C:\Program Files\Upscayl\resources\bin\upscayl-bin.exe")
    ap.add_argument("--models", default=r"C:\Program Files\Upscayl\resources\models")
    ap.add_argument("--model", default="RealESRGAN_General_x4_v3")
    ap.add_argument("--quality", type=int, default=93)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    work = os.path.join(a.out, "_work")
    t0 = time.time()
    for name in sorted(os.listdir(a.movies)):
        if not name.lower().endswith(".str"):
            continue
        entry = name[:-4]
        dst = os.path.join(a.out, entry + ".xgv")
        if os.path.exists(dst):
            continue
        shutil.rmtree(work, ignore_errors=True)
        din, dout = os.path.join(work, "in"), os.path.join(work, "out")
        os.makedirs(din); os.makedirs(dout)
        src = os.path.join(a.movies, name)
        subprocess.run([a.ffmpeg, "-v", "error", "-i", src, "-vsync", "0", os.path.join(din, "%06d.png")], check=False)
        frames = sorted(os.listdir(din))
        nums = frame_numbers(src)
        if len(nums) != len(frames):
            print("%s: %d frames decoded but %d frame numbers in the sectors; numbering by order" % (name, len(frames), len(nums)), flush=True)
            nums = list(range(1, len(frames) + 1))
        if not frames:
            continue
        subprocess.run([a.bin, "-i", din, "-o", dout, "-n", a.model, "-m", a.models, "-s", "4", "-f", "png"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        blobs, w, h = [], 0, 0
        for fn in frames:
            p = os.path.join(dout, fn)
            if not os.path.exists(p):
                print("%s: %s missing after upscaling" % (name, fn), flush=True)
                sys.exit(1)
            im = Image.open(p).convert("RGB")
            w, h = im.size
            tmp = os.path.join(work, "f.jpg")
            im.save(tmp, "JPEG", quality=a.quality, subsampling=0)
            with open(tmp, "rb") as f:
                blobs.append(f.read())
        head = 4 + 4 + 4 + 4 * len(blobs) + 4 * (len(blobs) + 1)
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
        print("%s: %d frames %dx%d, %.1f MB (%.0f s)" % (name, len(blobs), w, h, pos / 1048576, time.time() - t0), flush=True)
    shutil.rmtree(work, ignore_errors=True)
    print("ALL DONE", flush=True)

if __name__ == "__main__":
    main()
