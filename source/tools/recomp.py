# Static recompiler for the game's code: cuts the code an instrumented run executed into basic blocks and writes Rust
# for each block to src/machine/native/gen.rs. The code is the program's executable and the overlays it loads, taken from
# the trace of runs (run the player with XG_TRACE=<file>), one "stretch" of the trace at a time: a stretch is the code that
# ran between two reads of data from the host, with the words as they were in memory.
#
# The blocks run inside the same machine as the interpreter and agree with it instruction for instruction: the same
# registers, the same memory paths, the same step count, a block is left (so that the interpreter can take an interrupt)
# at the first store that reaches the devices, and any address the host has a hook on is never inside a block. A block
# carries the words it was made from and only runs while memory holds those same words.
#
#   python recomp.py [--static overlays.json --data <GameData>] <trace.txt> [more traces...]
#
# With --static, the code images listed in the json (rows [file, kind, base]: an archive file of the game data, "raw" or
# "lzss" (unpacked the way the game does it), and the address it is loaded at) are cut into blocks as well, in full and
# not only on the paths the runs took: from the addresses the traces entered inside the image, the jal targets and the
# pointers to function starts the image holds, and every jump table (a run of pointers into the image).
import re
import struct
import sys
import os

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
MAX_BLOCK = 200

# ---- addresses the host answers itself (hooks): never inside a block ----
src = open(os.path.join(ROOT, "src", "machine.rs"), encoding="utf-8").read()
inst = src[src.index("fn install_program"):src.index("fn observe")]
hooked = set(int(x, 16) for x in re.findall(r"0x(8[0-9A-Fa-f]{7})", inst))
hooked |= {0x8001996C, 0x80019ACC, 0x80037B88}
hooked -= {0x8003C020, 0x8003634C, 0x80019524}

# ---- the traces ----
# events: ("W", address, word), ("N", address) or ("E",): the words as they ran, the addresses entered other than by
# running on, and the marks at which the host was about to put new data in memory
args = sys.argv[1:]
STATIC = None
DATA = None
while args and args[0].startswith("--"):
    if args[0] == "--static":
        STATIC = args[1]
    elif args[0] == "--data":
        DATA = args[1]
    args = args[2:]
events = []
for fname in args:
    for line in open(fname):
        p = line.split()
        if not p:
            continue
        if p[0] == "W":
            events.append(("W", int(p[1], 16), int(p[2], 16)))
        elif p[0] == "N":
            events.append(("N", int(p[1], 16), 0))
        elif p[0] == "E":
            events.append(("E", 0, 0))
    events.append(("E", 0, 0))
IMAGE = {}


def word(a):
    return IMAGE.get(a)


# ---- decoding ----
def fields(i):
    return (i >> 26) & 63, (i >> 21) & 31, (i >> 16) & 31, (i >> 11) & 31, (i >> 6) & 31, i & 63


def simm(i):
    v = i & 0xFFFF
    return v - 0x10000 if v & 0x8000 else v


SUPPORTED_SPECIAL = {0, 2, 3, 4, 6, 7, 8, 9, 12, 16, 17, 18, 19, 24, 25, 26, 27, 32, 33, 34, 35, 36, 37, 38, 39, 42, 43}
SUPPORTED_OP = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 32, 33, 34, 35, 36, 37, 38, 40, 41, 42, 43, 46, 50, 58}


def supported(i):
    op, rs, rt, rd, sh, fn = fields(i)
    if op == 0:
        return fn in SUPPORTED_SPECIAL
    if op == 1:
        return rt in (0, 1, 0x10, 0x11)
    return op in SUPPORTED_OP


def kind(i):
    op, rs, rt, rd, sh, fn = fields(i)
    if op == 0:
        if fn == 8:
            return "jr"
        if fn == 9:
            return "jalr"
        return None
    if op == 1:
        return "bal" if (rt & 0x1E) == 0x10 else "cond"
    if op == 2:
        return "j"
    if op == 3:
        return "jal"
    if op in (4, 5, 6, 7):
        return "cond"
    return None


def branch_target(a, i):
    return (a + 4 + (simm(i) << 2)) & 0xFFFFFFFF


def jump_target(a, i):
    return ((a & 0xF0000000) | ((i & 0x3FFFFFF) << 2)) & 0xFFFFFFFF


def analyze(entries, runs=True):
    """Leaders and visited addresses of the current IMAGE."""
    roots = set(a for a in entries if a in IMAGE)
    # the start of every run of consecutive words (code the run reached by falling in from data or a hook)
    if runs:
        for a in IMAGE:
            if (a - 4) not in IMAGE:
                roots.add(a)
    leaders = set(roots)
    visited = set()
    work = list(roots)
    while work:
        a = work.pop()
        while a in IMAGE and a not in visited:
            if a in hooked:
                break
            i = word(a)
            if not supported(i):
                break
            k = kind(i)
            if k is None:
                visited.add(a)
                a += 4
                continue
            d = word(a + 4)
            if d is None or a + 4 in hooked or not supported(d) or kind(d) is not None:
                break
            visited.add(a)
            visited.add(a + 4)
            after = a + 8
            if k in ("cond", "bal"):
                t = branch_target(a, i)
                if t in IMAGE:
                    leaders.add(t)
                    work.append(t)
                leaders.add(after)
                a = after
                continue
            if k == "j":
                t = jump_target(a, i)
                if t in IMAGE:
                    leaders.add(t)
                    work.append(t)
                leaders.add(after)
                break
            if k == "jal":
                t = jump_target(a, i)
                if t in IMAGE:
                    leaders.add(t)
                    work.append(t)
                leaders.add(after)
                a = after
                continue
            if k == "jalr":
                leaders.add(after)
                a = after
                continue
            if k == "jr":
                leaders.add(after)
                break
    leaders = {a for a in leaders if a in visited and a not in hooked}
    return leaders, visited


# ---- emission ----
def R(n):
    return "0u32" if n == 0 else "(*r.add(%d))" % n


def setr(n, expr):
    return "" if n == 0 else "*r.add(%d) = %s;" % (n, expr)


def hexv(v):
    return "0x%08X" % (v & 0xFFFFFFFF)


def simple(i, a, ctx):
    """Statements for one non-control instruction at address a; ctx = (pc_expr, npc_expr, count) for state on slow paths
    and early exit. Returns a list of statements, or None when it can't be emitted."""
    op, rs, rt, rd, sh, fn = fields(i)
    imm = simm(i)
    immu = imm & 0xFFFFFFFF
    pc, npc, cnt = ctx
    st = "m.cur_pc=%s;m.pc=%s;m.npc=%s;" % (hexv(a), pc, npc)
    out = []
    if op == 0:
        if fn == 0:
            if rd != 0:
                out.append(setr(rd, "%s << %d" % (R(rt), sh)))
        elif fn == 2:
            out.append(setr(rd, "%s >> %d" % (R(rt), sh)))
        elif fn == 3:
            out.append(setr(rd, "((%s as i32) >> %d) as u32" % (R(rt), sh)))
        elif fn == 4:
            out.append(setr(rd, "%s << (%s & 31)" % (R(rt), R(rs))))
        elif fn == 6:
            out.append(setr(rd, "%s >> (%s & 31)" % (R(rt), R(rs))))
        elif fn == 7:
            out.append(setr(rd, "((%s as i32) >> (%s & 31)) as u32" % (R(rt), R(rs))))
        elif fn == 12:
            out.append("if *r.add(4) == 1 { *r.add(2) = if m.critical { 0 } else { 1 }; m.critical = true; } else { if *r.add(4) == 2 { m.critical = false; } *r.add(2) = 1; }")
        elif fn == 16:
            out.append(setr(rd, "m.hi"))
        elif fn == 17:
            out.append("m.hi = %s;" % R(rs))
        elif fn == 18:
            out.append(setr(rd, "m.lo"))
        elif fn == 19:
            out.append("m.lo = %s;" % R(rs))
        elif fn == 24:
            out.append("{ let p = (%s as i32 as i64) * (%s as i32 as i64); m.lo = p as u32; m.hi = (p >> 32) as u32; }" % (R(rs), R(rt)))
        elif fn == 25:
            out.append("{ let p = (%s as u64) * (%s as u64); m.lo = p as u32; m.hi = (p >> 32) as u32; }" % (R(rs), R(rt)))
        elif fn == 26:
            out.append("{ let a = %s as i32; let b = %s as i32; if b == 0 { m.lo = if a >= 0 { 0xFFFFFFFF } else { 1 }; m.hi = a as u32; } else if a == i32::MIN && b == -1 { m.lo = 0x80000000; m.hi = 0; } else { m.lo = (a / b) as u32; m.hi = (a %% b) as u32; } }" % (R(rs), R(rt)))
        elif fn == 27:
            out.append("{ let a = %s; let b = %s; if b == 0 { m.lo = 0xFFFFFFFF; m.hi = a; } else { m.lo = a / b; m.hi = a %% b; } }" % (R(rs), R(rt)))
        elif fn in (32, 33):
            out.append(setr(rd, "%s.wrapping_add(%s)" % (R(rs), R(rt))))
        elif fn in (34, 35):
            out.append(setr(rd, "%s.wrapping_sub(%s)" % (R(rs), R(rt))))
        elif fn == 36:
            out.append(setr(rd, "%s & %s" % (R(rs), R(rt))))
        elif fn == 37:
            out.append(setr(rd, "%s | %s" % (R(rs), R(rt))))
        elif fn == 38:
            out.append(setr(rd, "%s ^ %s" % (R(rs), R(rt))))
        elif fn == 39:
            out.append(setr(rd, "!(%s | %s)" % (R(rs), R(rt))))
        elif fn == 42:
            out.append(setr(rd, "if (%s as i32) < (%s as i32) { 1 } else { 0 }" % (R(rs), R(rt))))
        elif fn == 43:
            out.append(setr(rd, "if %s < %s { 1 } else { 0 }" % (R(rs), R(rt))))
        else:
            return None
        return [s for s in out if s]
    if op in (8, 9):
        out.append(setr(rt, "%s.wrapping_add(%s)" % (R(rs), hexv(immu))))
    elif op == 10:
        out.append(setr(rt, "if (%s as i32) < (%d) { 1 } else { 0 }" % (R(rs), imm)))
    elif op == 11:
        out.append(setr(rt, "if %s < %s { 1 } else { 0 }" % (R(rs), hexv(immu))))
    elif op == 12:
        out.append(setr(rt, "%s & %s" % (R(rs), hexv(i & 0xFFFF))))
    elif op == 13:
        out.append(setr(rt, "%s | %s" % (R(rs), hexv(i & 0xFFFF))))
    elif op == 14:
        out.append(setr(rt, "%s ^ %s" % (R(rs), hexv(i & 0xFFFF))))
    elif op == 15:
        out.append(setr(rt, hexv((i & 0xFFFF) << 16)))
    elif op == 16:
        if rs in (0, 2):
            out.append(setr(rt, "0u32"))
    elif op == 18:
        if i & 0x02000000:
            out.append("m.gte.command(%s);" % hexv(i & 0x1FFFFFF))
        elif rs == 0:
            out.append(setr(rt, "m.gte.read_data(%d)" % rd) or "let _ = m.gte.read_data(%d);" % rd)
        elif rs == 2:
            out.append(setr(rt, "m.gte.read_control(%d)" % rd) or "let _ = m.gte.read_control(%d);" % rd)
        elif rs == 4:
            out.append("m.gte.write_data(%d, %s);" % (rd, R(rt)))
        elif rs == 6:
            out.append("m.gte.write_control(%d, %s);" % (rd, R(rt)))
    elif op == 32:  # lb
        out.append("{ let ea = %s.wrapping_add(%s); let e2 = ea & 0x1FFFFFFF; let v = if e2 < RAM_SIZE && e2.wrapping_sub(STATE_LO) >= STATE_LEN { (((*w.add((e2 >> 2) as usize)) >> ((e2 & 3) * 8)) as i8) as i32 as u32 } else { %s (m.read8(ea) as i8) as i32 as u32 }; %s }"
                   % (R(rs), hexv(immu), st, setr(rt, "v")))
    elif op == 33:  # lh
        out.append("{ let ea = %s.wrapping_add(%s); let e2 = ea & 0x1FFFFFFF; let v = if e2 < RAM_SIZE && e2.wrapping_sub(STATE_LO) >= STATE_LEN { (((*w.add((e2 >> 2) as usize)) >> ((e2 & 2) * 8)) as i16) as i32 as u32 } else { %s (m.read16(ea) as i16) as i32 as u32 }; %s }"
                   % (R(rs), hexv(immu), st, setr(rt, "v")))
    elif op == 34:  # lwl
        out.append("{ let ea = %s.wrapping_add(%s); let wd = m.read32(ea & !3); let s = ea & 3; let keep = if s == 3 { 0 } else { 0xFFFFFFFFu32 >> ((s + 1) * 8) }; let v = (%s & keep) | (wd << ((3 - s) * 8)); %s }"
                   % (R(rs), hexv(immu), R(rt), setr(rt, "v")))
    elif op == 35:  # lw
        out.append("{ let ea = %s.wrapping_add(%s); let e2 = ea & 0x1FFFFFFF; let v = if e2 < RAM_SIZE && e2 & 3 == 0 && e2.wrapping_sub(STATE_LO) >= STATE_LEN { *w.add((e2 >> 2) as usize) } else { %s m.read32(ea) }; %s }"
                   % (R(rs), hexv(immu), st, setr(rt, "v")))
    elif op == 36:  # lbu
        out.append("{ let ea = %s.wrapping_add(%s); let e2 = ea & 0x1FFFFFFF; let v = if e2 < RAM_SIZE && e2.wrapping_sub(STATE_LO) >= STATE_LEN { ((*w.add((e2 >> 2) as usize)) >> ((e2 & 3) * 8)) & 0xFF } else { %s m.read8(ea) as u32 }; %s }"
                   % (R(rs), hexv(immu), st, setr(rt, "v")))
    elif op == 37:  # lhu
        out.append("{ let ea = %s.wrapping_add(%s); let e2 = ea & 0x1FFFFFFF; let v = if e2 < RAM_SIZE && e2.wrapping_sub(STATE_LO) >= STATE_LEN { ((*w.add((e2 >> 2) as usize)) >> ((e2 & 2) * 8)) & 0xFFFF } else { %s m.read16(ea) as u32 }; %s }"
                   % (R(rs), hexv(immu), st, setr(rt, "v")))
    elif op == 38:  # lwr
        out.append("{ let ea = %s.wrapping_add(%s); let wd = m.read32(ea & !3); let s = ea & 3; let keep = if s == 0 { 0 } else { 0xFFFFFFFFu32 << ((4 - s) * 8) }; let v = (%s & keep) | (wd >> (s * 8)); %s }"
                   % (R(rs), hexv(immu), R(rt), setr(rt, "v")))
    elif op in (40, 41, 43):
        # stores: RAM directly, else through the machine; a store that reached the devices leaves the block when an
        # interrupt became due (or the machine stopped), so the interpreter takes it at the next instruction
        if op == 40:
            fast = "let s = (e2 & 3) * 8; let wi = (e2 >> 2) as usize; *w.add(wi) = (*w.add(wi) & !(0xFFu32 << s)) | ((%s & 0xFF) << s);" % R(rt)
            cond = "e2 < RAM_SIZE && ((*m.mem.flag.get_unchecked((e2 >> 8) as usize)) >> ((e2 >> 2) & 63)) & 1 == 0"
            slow = "m.write8(ea, %s as u8);" % R(rt)
        elif op == 41:
            fast = "let s = (e2 & 2) * 8; let wi = (e2 >> 2) as usize; *w.add(wi) = (*w.add(wi) & !(0xFFFFu32 << s)) | ((%s & 0xFFFF) << s);" % R(rt)
            cond = "e2 < RAM_SIZE && e2 & 1 == 0 && ((*m.mem.flag.get_unchecked((e2 >> 8) as usize)) >> ((e2 >> 2) & 63)) & 1 == 0"
            slow = "m.write16(ea, %s as u16);" % R(rt)
        else:
            fast = "*w.add((e2 >> 2) as usize) = %s;" % R(rt)
            cond = "e2 < RAM_SIZE && e2 & 3 == 0 && ((*m.mem.flag.get_unchecked((e2 >> 8) as usize)) >> ((e2 >> 2) & 63)) & 1 == 0"
            slow = "m.write32(ea, %s);" % R(rt)
        out.append("{ let ea = %s.wrapping_add(%s); let e2 = ea & 0x1FFFFFFF; if %s { %s } else { %s %s if m.irq_pending || m.stop_reason.is_some() || %s { return %d; } } }"
                   % (R(rs), hexv(immu), cond, fast, st, slow, own_hit("e2"), cnt))
    elif op == 42:  # swl
        out.append("{ let ea = %s.wrapping_add(%s); let wd = m.read32(ea & !3); let s = ea & 3; let keep = if s == 3 { 0 } else { 0xFFFFFFFFu32 << ((s + 1) * 8) }; %s let v = (wd & keep) | (%s >> ((3 - s) * 8)); m.write32(ea & !3, v); if m.irq_pending || m.stop_reason.is_some() || %s { return %d; } }"
                   % (R(rs), hexv(immu), st, R(rt), own_hit("ea"), cnt))
    elif op == 46:  # swr
        out.append("{ let ea = %s.wrapping_add(%s); let wd = m.read32(ea & !3); let s = ea & 3; let keep = if s == 0 { 0 } else { 0xFFFFFFFFu32 >> ((4 - s) * 8) }; %s let v = (wd & keep) | (%s << (s * 8)); m.write32(ea & !3, v); if m.irq_pending || m.stop_reason.is_some() || %s { return %d; } }"
                   % (R(rs), hexv(immu), st, R(rt), own_hit("ea"), cnt))
    elif op == 50:  # lwc2
        out.append("{ let v = m.read32(%s.wrapping_add(%s)); m.gte.write_data(%d, v); }" % (R(rs), hexv(immu), rt))
    elif op == 58:  # swc2
        out.append("{ %s let v = m.gte.read_data(%d); let ea = %s.wrapping_add(%s); m.write32(ea, v); if m.irq_pending || m.stop_reason.is_some() || %s { return %d; } }"
                   % (st, rt, R(rs), hexv(immu), own_hit("ea"), cnt))
    else:
        return None
    return [s for s in out if s]


CUR_L = 0


def own_hit(ea_expr):
    """true when a store at ea_expr lands inside the words of the block being built (up to MAX_BLOCK of them)"""
    return "((%s) & 0x1FFFFFFF).wrapping_sub(0x%08X) < 0x%X" % (ea_expr, CUR_L & 0x1FFFFFFF, MAX_BLOCK * 4 + 8)


def build(L, leaders, visited):
    global CUR_L
    CUR_L = L
    body = []
    a = L
    n = 0
    end_pc = None
    while True:
        if n and (a in leaders or n >= MAX_BLOCK or a not in visited or a in hooked):
            end_pc = a
            break
        i = word(a)
        k = kind(i)
        if k is None:
            n += 1
            st = simple(i, a, (hexv(a + 4), hexv(a + 8), n))
            if st is None:
                n -= 1
                end_pc = a
                break
            body.extend(st)
            a += 4
            continue
        d = word(a + 4)
        op, rs, rt, rd, sh, fn = fields(i)
        pre = []
        if k == "cond":
            if op == 1:
                cmp = "(%s as i32) >= 0" % R(rs) if rt & 1 else "(%s as i32) < 0" % R(rs)
            elif op == 4:
                cmp = "%s == %s" % (R(rs), R(rt))
            elif op == 5:
                cmp = "%s != %s" % (R(rs), R(rt))
            elif op == 6:
                cmp = "(%s as i32) <= 0" % R(rs)
            else:
                cmp = "(%s as i32) > 0" % R(rs)
            pre.append("let tgt: u32 = if %s { %s } else { %s };" % (cmp, hexv(branch_target(a, i)), hexv(a + 8)))
        elif k == "bal":
            cmp = "(%s as i32) >= 0" % R(rs) if rt & 1 else "(%s as i32) < 0" % R(rs)
            pre.append("let tgt: u32 = if %s { %s } else { %s };" % (cmp, hexv(branch_target(a, i)), hexv(a + 8)))
            pre.append("*r.add(31) = %s;" % hexv(a + 8))
        elif k == "j":
            pre.append("let tgt: u32 = %s;" % hexv(jump_target(a, i)))
        elif k == "jal":
            pre.append("let tgt: u32 = %s;" % hexv(jump_target(a, i)))
            pre.append("*r.add(31) = %s;" % hexv(a + 8))
        elif k == "jr":
            pre.append("let tgt: u32 = %s;" % R(rs))
        else:  # jalr
            pre.append("let tgt: u32 = %s;" % R(rs))
            if rd != 0:
                pre.append("*r.add(%d) = %s;" % (rd, hexv(a + 8)))
        n += 2  # the branch and its delay slot
        body.extend(pre)
        ds = simple(d, a + 4, ("tgt", "tgt.wrapping_add(4)", n))
        if ds is None:
            return None
        body.extend(ds)
        body.append("m.cur_pc = %s; m.pc = tgt; m.npc = tgt.wrapping_add(4); return %d;" % (hexv(a + 4), n))
        return n, body
    if n == 0:
        return None
    body.append("m.cur_pc = %s; m.pc = %s; m.npc = %s; return %d;" % (hexv(end_pc - 4), hexv(end_pc), hexv(end_pc + 4), n))
    return n, body


blocks = {}
order = []
entries_all = set()


def snapshot():
    leaders, visited = analyze(entries_all)
    for L in sorted(leaders):
        r = build(L, leaders, visited)
        if r is None:
            continue
        n, body = r
        ws = tuple(word(L + 4 * k) for k in range(n))
        if None in ws:
            continue
        key = (L, ws)
        if key in blocks:
            continue
        blocks[key] = (n, body)
        order.append(key)


# the slow part (the traces cut into blocks) is kept between runs while the traces and this file's emitter are unchanged
import hashlib
import pickle
_src = open(__file__, encoding="utf-8").read()
_ckey = hashlib.sha1((repr([(f, os.path.getsize(f)) for f in args]) + _src[:_src.index("# ---- the code images of the game data ----")]).encode()).hexdigest()
_cpath = os.path.join(os.path.dirname(ROOT), "cmp", "recomp_cache.pkl")
_cached = None
if os.path.exists(_cpath):
    try:
        _cached = pickle.load(open(_cpath, "rb"))
        if _cached[0] != _ckey:
            _cached = None
    except Exception:
        _cached = None
if _cached is not None:
    _, blocks, order, entries_all = _cached
    print("trace pass taken from", _cpath)
else:
    dirty = False
    for kind_, a_, w_ in events:
        if kind_ == "W":
            IMAGE[a_] = w_
            dirty = True
        elif kind_ == "N":
            entries_all.add(a_)
            dirty = True
        elif dirty:
            snapshot()
            dirty = False
    if dirty:
        snapshot()
    try:
        pickle.dump((_ckey, blocks, order, entries_all), open(_cpath, "wb"))
    except Exception:
        pass
epochs = events
traced_total = sum(blocks[k][0] for k in order)
traced_blocks = len(order)


# ---- the code images of the game data ----
def lzss(data):
    """The game's LZSS: u32 size, then flag bytes (bit set = two-byte back reference). Input ending early gives what there is."""
    if len(data) < 4:
        return None
    size = struct.unpack_from("<I", data, 0)[0]
    if size <= 0 or size > 2 * 1024 * 1024:
        return None
    out = bytearray()
    src = 4
    n = len(data)
    while len(out) < size:
        if src >= n:
            break
        flags = data[src]
        src += 1
        for bit in range(8):
            if len(out) >= size or src >= n:
                break
            if not (flags >> bit) & 1:
                out.append(data[src])
                src += 1
            else:
                if src + 1 >= n:
                    src = n
                    break
                b0 = data[src]
                b1 = data[src + 1]
                src += 2
                dist = ((b1 & 0x0F) << 8) | b0
                ln = (b1 >> 4) + 3
                if dist == 0 or dist > len(out):
                    return None
                for _ in range(ln):
                    if len(out) >= size:
                        break
                    out.append(out[-dist])
    return bytes(out) if len(out) * 2 >= size else None


def static_image(row):
    name, kind, base = row[0], row[1], int(row[2], 16)
    data = open(os.path.join(DATA, name), "rb").read()
    if kind == "lzss":
        data = lzss(data)
        if data is None:
            return None
    if len(row) > 3:  # bytes to skip at the start (the executable's header)
        data = data[row[3]:]
        base += row[3]
    n = len(data) // 4
    words = struct.unpack_from("<%dI" % n, data, 0)
    return base, words


starts = {}
for (L, ws) in order:
    starts.setdefault(L, []).append(ws)
static_blocks = 0
if STATIC:
    import json
    traced_addrs = set(a for k_, a, w in events if k_ == "W")
    for row in json.load(open(STATIC)):
        img = static_image(row)
        if img is None:
            print("static image not read:", row)
            continue
        base, words = img
        end = base + 4 * len(words)
        IMAGE.clear()
        for i, w in enumerate(words):
            IMAGE[base + 4 * i] = w
        # the addresses the runs entered other than by running on (every traced address would cut the code into one-word blocks)
        roots = set(a for a in traced_addrs if base <= a < end and IMAGE.get(a) is not None and (a - 4) not in traced_addrs)
        roots |= set(a for a in entries_all if base <= a < end)
        # pointers into the image: function starts (a prologue, or right after a return and its delay slot) and jump tables
        def fstart(t):
            if not (base <= t < end) or t & 3:
                return False
            w = IMAGE[t]
            if (w >> 16) == 0x27BD and (w & 0x8000):
                return True
            p = IMAGE.get(t - 8)
            return p == 0x03E00008
        run = []
        for i, w in enumerate(words):
            a = base + 4 * i
            if base <= w < end and not (w & 3):
                run.append(w)
                if fstart(w):
                    roots.add(w)
            else:
                if len(run) >= 3:
                    roots.update(run)
                run = []
            if (w >> 26) == 3:
                t = jump_target(a, w)
                if fstart(t):
                    roots.add(t)
        if len(run) >= 3:
            roots.update(run)
        roots.add(base)
        leaders, visited = analyze(roots, runs=False)
        made = 0
        for L in sorted(leaders):
            r = build(L, leaders, visited)
            if r is None:
                continue
            n, body = r
            ws = tuple(word(L + 4 * k) for k in range(n))
            if None in ws:
                continue
            if (L, ws) in blocks:
                continue
            if any(ws[:min(n, len(o))] == o[:min(n, len(o))] for o in starts.get(L, [])):
                continue
            blocks[(L, ws)] = (n, body)
            order.append((L, ws))
            starts.setdefault(L, []).append(ws)
            made += 1
        static_blocks += made
        print("static %-10s %-5s base %08X: %6d words, %5d roots, %5d new blocks" % (row[0], row[1], base, len(words), len(roots), made))

out = []
out.append("// Generated by tools/recomp.py from the trace of the program's runs: do not edit.")
out.append("#![allow(unused_variables, unused_mut, unused_unsafe, unused_parens, clippy::all)]")
out.append("use crate::{BlockFn, Ctx, RAM_SIZE};")
out.append("")


def adapt(line):
    """what a block says about the machine, in terms of Ctx"""
    for a_, b_ in (("m.irq_pending || m.stop_reason.is_some() ||", "m.exit ||"),
                   ("m.mem.flag.get_unchecked(", "m.flag.add("),
                   ("m.gte.", "(*m.gte).")):
        line = line.replace(a_, b_)
    return line


names = {}
for idx, key in enumerate(order):
    L = key[0]
    n, body = blocks[key]
    name = "b%d_%08X" % (idx, L)
    names[key] = name
    out.append("unsafe fn %s(m: &mut Ctx) -> i32 {" % name)
    out.append("    let r = m.r; let w = m.w;")
    for s in body:
        out.append("    " + adapt(s))
    out.append("}")
out.append("")
allwords = []
offsets = {}
for key in order:
    offsets[key] = len(allwords)
    allwords.extend(key[1])
out.append("pub static WORDS: [u32; %d] = [" % len(allwords))
for k in range(0, len(allwords), 8):
    out.append("    " + ", ".join("0x%08X" % w for w in allwords[k:k + 8]) + ",")
out.append("];")
out.append("")
out.append("/// (address, instructions, offset into WORDS of the words it was made from, the block)")
out.append("pub static BLOCKS: &[(u32, u32, u32, BlockFn)] = &[")
for key in order:
    n, body = blocks[key]
    out.append("    (0x%08X, %d, %d, %s)," % (key[0], n, offsets[key], names[key]))
out.append("];")
path = os.path.join(ROOT, "blocks", "src", "gen.rs")
os.makedirs(os.path.dirname(path), exist_ok=True)
open(path, "w", encoding="utf-8", newline="\n").write("\n".join(out) + "\n")
tot = sum(blocks[k][0] for k in order)
print("stretches: %d, blocks: %d, instructions: %d, distinct start addresses: %d" % (len(epochs), len(order), tot, len(set(k[0] for k in order))))
print("from the traces: %d blocks, %d instructions; from the images: %d blocks, %d instructions" % (traced_blocks, traced_total, static_blocks, tot - traced_total))
