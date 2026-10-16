"""Function discovery over the game's code images (tools/overlays.json): entry points from jal targets, function pointers
(words that point at a prologue) and returns, then each function's extent by following its control flow."""
import json, struct, sys, os, collections

# the game's data folder: XENOGEARS_DATA, else project/GameData of the unified folder this tree sits in
_here = os.path.dirname(os.path.abspath(__file__))
GD = os.environ.get('XENOGEARS_DATA') or os.path.normpath(os.path.join(_here, '..', '..', '..', '..', 'project', 'GameData'))


def lzss(data):
    size = struct.unpack_from('<I', data, 0)[0]
    out = bytearray(); src = 4; n = len(data)
    while len(out) < size and src < n:
        flags = data[src]; src += 1
        for bit in range(8):
            if len(out) >= size or src >= n: break
            if not (flags >> bit) & 1:
                out.append(data[src]); src += 1
            else:
                if src + 1 >= n: src = n; break
                b0 = data[src]; b1 = data[src + 1]; src += 2
                dist = ((b1 & 0x0F) << 8) | b0; ln = (b1 >> 4) + 3
                if dist == 0 or dist > len(out): return None
                for _ in range(ln):
                    if len(out) >= size: break
                    out.append(out[-dist])
    return bytes(out)


def load_image(row):
    name, kind, base = row[0], row[1], int(row[2], 16)
    data = open(os.path.join(GD, *name.split('/')), 'rb').read()
    if kind == 'lzss':
        data = lzss(data)
    if len(row) > 3:
        data = data[row[3]:]
        base += row[3]
    n = len(data) // 4
    return base, struct.unpack_from('<%dI' % n, data, 0)


def kind(i):
    op = i >> 26
    rt = (i >> 16) & 31
    if op == 0:
        fn = i & 63
        return 'jr' if fn == 8 else 'jalr' if fn == 9 else None
    if op == 1: return 'bal' if (rt & 0x1E) == 0x10 else 'cond'
    if op == 2: return 'j'
    if op == 3: return 'jal'
    if op in (4, 5, 6, 7): return 'cond'
    return None


def simm(i):
    v = i & 0xFFFF
    return v - 0x10000 if v & 0x8000 else v


def analyse(base, words, extra_entries=()):
    n = len(words)
    end = base + 4 * n
    W = lambda a: words[(a - base) >> 2] if base <= a < end and not a & 3 else None
    entries = set(extra_entries)
    # jal targets
    for i, w in enumerate(words):
        if (w >> 26) == 3:
            t = ((base + 4 * i) & 0xF0000000) | ((w & 0x3FFFFFF) << 2)
            if base <= t < end: entries.add(t)
    # prologues and code after a return
    for i, w in enumerate(words):
        if (w >> 16) == 0x27BD and (w & 0x8000):
            entries.add(base + 4 * i)
    # pointers to prologues (function pointer tables)
    for w in words:
        if base <= w < end and not w & 3:
            x = W(w)
            if x is not None and (x >> 16) == 0x27BD and (x & 0x8000): entries.add(w)
    return entries


if __name__ == '__main__':
    rows = json.load(open(os.path.join(os.path.dirname(__file__), '..', 'overlays.json')))
    total = 0
    for row in rows:
        base, words = load_image(row)
        ent = analyse(base, words)
        print('%-26s base %08X words %6d function entries %5d' % (row[0], base, len(words), len(ent)))
        total += len(ent)
    print('total entries', total)
