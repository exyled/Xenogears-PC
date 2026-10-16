"""Names for the main executable's library functions that are recognised by their exact instruction words (the standard
graphics-library primitive initialisers and bit packers, the C library's fill/copy/random, the sine table lookups). Only
main-executable addresses are named: overlays share addresses with one another, names.json is keyed by address.

Usage: python idioms.py [--write]    (without --write it only lists what it found)"""
import json, os, sys
from discover import load_image

HERE = os.path.dirname(os.path.abspath(__file__))
rows = json.load(open(os.path.join(HERE, '..', 'overlays.json')))
base, words = load_image(rows[0])
end = base + 4 * len(words)
W = lambda a: words[(a - base) >> 2]

JR = 0x03E00008


def body(a):
    """the words of the function at a: up to and including the delay slot of its first `jr ra`"""
    out = []
    while a < end and len(out) < 64:
        w = W(a)
        out.append(w)
        if w == JR:
            out.append(W(a + 4))
            break
        a += 4
    return tuple(out)


# (name, address of the instance the pattern was taken from, why)
SAMPLES = [
    ('memzero', 0x8003F8E8, 'fills n bytes at the pointer with 0, returns the pointer (0 for a null pointer)'),
    ('memcpy', 0x8003F968, 'copies n bytes forward, returns the destination (0 for a null pointer)'),
    ('rand', 0x8003FA38, 'seed = seed * 1103515245 + 12345; returns (seed >> 16) & 0x7FFF'),
    ('srand', 0x8003FA68, 'stores the seed'),
    ('trig_sin', 0x8003F8B0, 'table[a & 4095].sin: 4096 units to the circle, 4096 = 1.0'),
    ('trig_cos', 0x8003F8CC, 'table[a & 4095].cos'),
    ('gpu_get_tpage', 0x80043A1C, 'packs (mode, blend, x, y) into the texture-page word'),
    ('gpu_get_clut', 0x80043A58, 'packs (x, y) into the palette word'),
    ('gpu_insert_prim', 0x80043B48, 'links the primitive after another in an ordering table (24-bit links)'),
    ('gte_set_rot_matrix', 0x80049EFC, 'control registers 0-4 from five words'),
    ('gte_set_light_matrix', 0x80049F2C, 'control registers 8-12 from five words'),
    ('gte_set_color_matrix', 0x80049F5C, 'control registers 16-20 from five words'),
    ('gte_set_trans_matrix', 0x80049F8C, 'control registers 5-7 from offsets 20, 24, 28'),
    ('gpu_prim_semi_trans', 0x80043BFC, 'sets or clears the semi-transparency bit of a primitive'),
    ('gpu_prim_shade_tex', 0x80043C24, 'sets or clears the "no texture brightness" bit of a primitive'),
]

# primitive initialisers: addiu v0,zero,LEN / sb v0,3(a0) / addiu v0,zero,CODE / jr ra / sb v0,7(a0)
PRIMS = {
    (4, 0x20): 'gpu_set_poly_f3', (5, 0x28): 'gpu_set_poly_f4', (7, 0x24): 'gpu_set_poly_ft3', (9, 0x2C): 'gpu_set_poly_ft4',
    (6, 0x30): 'gpu_set_poly_g3', (8, 0x38): 'gpu_set_poly_g4', (9, 0x34): 'gpu_set_poly_gt3', (12, 0x3C): 'gpu_set_poly_gt4',
    (3, 0x40): 'gpu_set_line_f2', (4, 0x50): 'gpu_set_line_g2', (5, 0x48): 'gpu_set_line_f3', (7, 0x58): 'gpu_set_line_g3',
    (6, 0x4C): 'gpu_set_line_f4', (9, 0x5C): 'gpu_set_line_g4', (3, 0x60): 'gpu_set_tile', (2, 0x68): 'gpu_set_tile_1',
    (2, 0x70): 'gpu_set_tile_8', (2, 0x78): 'gpu_set_tile_16', (4, 0x64): 'gpu_set_sprt', (3, 0x74): 'gpu_set_sprt_8',
    (3, 0x7C): 'gpu_set_sprt_16',
}


DATA = ['vxy0', 'vz0', 'vxy1', 'vz1', 'vxy2', 'vz2', 'rgbc', 'otz', 'ir0', 'ir1', 'ir2', 'ir3', 'sxy0', 'sxy1', 'sxy2', 'sxyp',
        'sz0', 'sz1', 'sz2', 'sz3', 'rgb0', 'rgb1', 'rgb2', 'res1', 'mac0', 'mac1', 'mac2', 'mac3', 'irgb', 'orgb', 'lzcs', 'lzcr']
CTRL = ['r11r12', 'r13r21', 'r22r23', 'r31r32', 'r33', 'trx', 'try', 'trz', 'l11l12', 'l13l21', 'l22l23', 'l31l32', 'l33',
        'rbk', 'gbk', 'bbk', 'lr1lr2', 'lr3lg1', 'lg2lg3', 'lb1lb2', 'lb3', 'rfc', 'gfc', 'bfc', 'ofx', 'ofy', 'h', 'dqa', 'dqb',
        'zsf3', 'zsf4', 'flag']


def gte_mover(a):
    """a function made only of coprocessor register moves / loads / stores: its name from the registers it touches"""
    parts = []
    kinds = set()
    k = a
    while k < end and len(parts) < 12:
        w = W(k)
        op = w >> 26
        if w == JR:
            if W(k + 4) != 0:
                return None
            break
        if w == 0:
            k += 4
            continue
        rs, rt, rd = (w >> 21) & 31, (w >> 16) & 31, (w >> 11) & 31
        if op == 18 and not w & 0x02000000 and rs in (0, 2, 4, 6):
            kinds.add({0: 'get', 2: 'get', 4: 'set', 6: 'set'}[rs])
            parts.append((DATA if rs in (0, 4) else CTRL)[rd])
        elif op == 50:
            kinds.add('load'); parts.append(DATA[rt])
        elif op == 58:
            kinds.add('store'); parts.append(DATA[rt])
        else:
            return None
        k += 4
    if not parts or len(kinds) != 1:
        return None
    return 'gte_%s_%s' % (kinds.pop(), '_'.join(parts))


def main():
    found = {}
    for name, at, why in SAMPLES:
        pat = body(at)
        hits = [a for a in range(base, end - 4 * len(pat), 4) if tuple(W(a + 4 * k) for k in range(len(pat))) == pat]
        # the same words can occur more than once only if the compiler emitted two copies: all of them get the name
        for a in hits:
            found.setdefault(a, (name if len(hits) == 1 else '%s_%08X' % (name, a), why))
    # function starts: the word before the previous one is a `jr ra` (its delay slot sits in between)
    for a in range(base + 8, end - 4, 4):
        if a in found or W(a - 8) != JR:
            continue
        n = gte_mover(a)
        if n:
            found.setdefault(a, (n, 'moves coprocessor registers only'))
    for a in range(base, end - 20, 4):
        if W(a) >> 16 == 0x2402 and W(a + 4) == 0xA0820003 and W(a + 8) >> 16 == 0x2402 and W(a + 12) == JR and W(a + 16) == 0xA0820007:
            key = (W(a) & 0xFFFF, W(a + 8) & 0xFFFF)
            if key in PRIMS:
                found.setdefault(a, (PRIMS[key], 'sets the length byte %d and the code byte 0x%02X of the primitive' % key))
    return found


if __name__ == '__main__':
    found = main()
    for a in sorted(found):
        print('%08X  %s' % (a, found[a][0]))
    print(len(found), 'functions recognised')
    if '--write' in sys.argv:
        p = os.path.join(HERE, 'names.json')
        j = json.load(open(p))
        f = j.setdefault('functions', {})
        added = 0
        for a, (n, why) in found.items():
            k = '%08X' % a
            if k not in f:
                f[k] = n
                added += 1
        json.dump(j, open(p, 'w'), indent=1)
        print('names.json: %d names added' % added)
