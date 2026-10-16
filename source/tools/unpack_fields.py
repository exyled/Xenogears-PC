# Splits every room file (fields/NNNN.bin) into its parts: fields/NNNN/header.bin and sNN_<section>.lzss (the sections
# exactly as the game reads them, joined back by the player through layout.csv), and writes the sections decompressed
# for reading to fields/NNNN/unpacked/ (textures also as separate .tim files).
#   python unpack_fields.py <GameData folder>          split
#   python unpack_fields.py <GameData folder> --undo   join the parts back into fields/NNNN.bin
import os, sys, struct, shutil

NAMES = ["textures", "walkmesh", "models", "sprites", "palettes", "scripts", "unknown6", "dialogue", "triggers"]
SIZES_AT, OFFSETS_AT, HEADER_END = 0x10C, 0x130, 0x154


def lzss(data, off, limit):
    size = min(struct.unpack_from("<I", data, off)[0], limit)
    out = bytearray(size)
    src, dst = off + 4, 0
    while dst < size:
        if src >= len(data):
            break
        flags = data[src]
        src += 1
        for _ in range(8):
            if dst >= size:
                break
            if flags & 1 == 0:
                if src >= len(data):
                    return bytes(out[:dst])
                out[dst] = data[src]
                dst += 1
                src += 1
            else:
                if src + 1 >= len(data):
                    return bytes(out[:dst])
                b0, b1 = data[src], data[src + 1]
                src += 2
                dist = ((b1 & 15) << 8) | b0
                ln = (b1 >> 4) + 3
                frm = dst - dist
                if dist == 0 or frm < 0:
                    return bytes(out[:dst])
                for i in range(ln):
                    if dst >= size:
                        break
                    out[dst] = out[frm + i]
                    dst += 1
            flags >>= 1
    return bytes(out[:dst])


def parse(d):
    if len(d) < HEADER_END:
        return None
    secs = []
    for i in range(9):
        size = struct.unpack_from("<i", d, SIZES_AT + 4 * i)[0]
        off = struct.unpack_from("<i", d, OFFSETS_AT + 4 * i)[0]
        if size < 0 or size > 2 * 1024 * 1024:
            return None
        if size == 0:
            continue
        if off < HEADER_END or off + 4 > len(d):
            return None
        un = struct.unpack_from("<I", d, off)[0]
        if un < size or un > size + 16:
            return None
        secs.append((off, i, size))
    if len(secs) < 3:
        return None
    secs.sort()
    return secs


def main():
    root = sys.argv[1]
    lay = os.path.join(root, "layout.csv")
    rows = [l.rstrip("\n") for l in open(lay, encoding="utf-8")]
    undo = "--undo" in sys.argv
    out, n = [], 0
    for line in rows:
        p = line.split(",", 3)
        if len(p) != 4 or p[1] != "archive" or not p[2].startswith("fields/"):
            out.append(line)
            continue
        entry, cls, path, orig = p
        num = int(entry)
        if undo:
            if "|" not in path:
                out.append(line)
                continue
            data = b"".join(open(os.path.join(root, q), "rb").read() for q in path.split("|"))
            open(os.path.join(root, "fields/%04d.bin" % num), "wb").write(data)
            shutil.rmtree(os.path.join(root, "fields/%04d" % num), ignore_errors=True)
            out.append("%s,archive,fields/%04d.bin,%s" % (entry, num, orig))
            n += 1
            continue
        if "|" in path:
            out.append(line)
            continue
        f = os.path.join(root, path)
        d = open(f, "rb").read()
        secs = parse(d)
        if not secs:
            out.append(line)
            continue
        folder = "fields/%04d" % num
        parts = [("header.bin", 0, secs[0][0])]
        for k, (off, i, size) in enumerate(secs):
            end = secs[k + 1][0] if k + 1 < len(secs) else len(d)
            parts.append(("s%02d_%s.lzss" % (i, NAMES[i]), off, end))
        if b"".join(d[a:b] for _, a, b in parts) != d:
            out.append(line)  # the parts do not cover the file exactly: leave it whole
            continue
        os.makedirs(os.path.join(root, folder, "unpacked"), exist_ok=True)
        for name, a, b in parts:
            open(os.path.join(root, folder, name), "wb").write(d[a:b])
        for off, i, size in secs:
            raw = lzss(d, off, size)
            open(os.path.join(root, folder, "unpacked", "%02d_%s.dat" % (i, NAMES[i])), "wb").write(raw)
            if i == 0 and len(raw) >= 4:
                cnt = struct.unpack_from("<I", raw, 0)[0]
                if 0 < cnt < 256 and 4 + 4 * cnt <= len(raw):
                    offs = [struct.unpack_from("<I", raw, 4 + 4 * k)[0] for k in range(cnt)] + [len(raw)]
                    td = os.path.join(root, folder, "unpacked", "textures")
                    os.makedirs(td, exist_ok=True)
                    for k in range(cnt):
                        if offs[k] < offs[k + 1] <= len(raw):
                            open(os.path.join(td, "%02d.tim" % k), "wb").write(raw[offs[k]:offs[k + 1]])
        os.remove(f)
        out.append("%s,archive,%s,%s" % (entry, "|".join(folder + "/" + nm for nm, _, _ in parts), orig))
        n += 1
    open(lay, "w", encoding="utf-8", newline="\n").write("\n".join(out) + "\n")
    print(("joined " if undo else "split ") + str(n) + " room files")


if __name__ == "__main__":
    main()
