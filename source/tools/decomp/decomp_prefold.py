"""A decompiler from the game's MIPS code to structured Rust.

Each function becomes a Rust function with its registers as locals, real if / loop / match control flow (the control-flow graph
is structured with the dominator-tree algorithm of Ramsey's "Beyond Relooper") and calls as Rust calls. A function that does
not follow the calling convention (it reads a register nobody gave it, uses a register after a call, jumps somewhere it cannot
be followed, has a flow graph that cannot be structured) is reported and left to the translated blocks.

    python decomp.py <image index in overlays.json> [out.rs]
"""
import json, os, struct, sys, collections

sys.path.insert(0, os.path.dirname(__file__))
from discover import load_image, analyse, kind, simm

HERE = os.path.dirname(os.path.abspath(__file__))

# ---- registers ----
CALLEE_SAVED = set(range(16, 24)) | {28, 30}          # s0-s7, gp, s8
ARGS = {4, 5, 6, 7}
KEEP = CALLEE_SAVED | {29, 31, 0}                     # sp and ra are special
CLOBBERED = {1, 8, 9, 10, 11, 12, 13, 14, 15, 24, 25} | ARGS  # not v0 / v1


class Unsupported(Exception):
    pass


def fields(i):
    return (i >> 26) & 63, (i >> 21) & 31, (i >> 16) & 31, (i >> 11) & 31, (i >> 6) & 31, i & 63


def reg(n):
    return '0u32' if n == 0 else 'r%d' % n


def imm_u(i):
    return simm(i) & 0xFFFFFFFF


def uses_defs(i):
    """(registers read, registers written, is_call) of a non-control instruction."""
    op, rs, rt, rd, sh, fn = fields(i)
    if op == 0:
        if fn in (0, 2, 3): return {rt}, {rd}
        if fn in (4, 6, 7): return {rs, rt}, {rd}
        if fn in (16, 18): return set(), {rd}
        if fn in (17, 19): return {rs}, set()
        if fn in (24, 25, 26, 27): return {rs, rt}, set()
        if fn == 13: return set(), set()
        if fn in (32, 33, 34, 35, 36, 37, 38, 39, 42, 43): return {rs, rt}, {rd}
        raise Unsupported('special %d' % fn)
    if op in (8, 9, 10, 11, 12, 13, 14): return {rs}, {rt}
    if op == 15: return set(), {rt}
    if op in (32, 33, 36, 37, 35): return {rs}, {rt}
    if op == 34 or op == 38: return {rs, rt}, {rt}
    if op in (40, 41, 43, 42, 46): return {rs, rt}, set()
    if op == 18:
        if rs == 0: return set(), {rt}          # mfc2
        if rs == 2: return set(), {rt}          # cfc2
        if rs == 4 or rs == 6: return {rt}, set()  # mtc2 / ctc2
        if rs & 0x10: return set(), set()       # gte command
        raise Unsupported('cop2 %d' % rs)
    if op == 50: return {rs}, set()             # lwc2
    if op == 58: return {rs}, set()             # swc2
    raise Unsupported('op %d' % op)


class Block:
    def __init__(self, start):
        self.start = start
        self.insns = []        # (addr, word) of the straight-line part
        self.term = None       # ('goto', t) ('cond', expr_regs..., t, f) ('ret',) ('switch', ...) ('tail', t) ('tailind', reg)
        self.succ = []
        self.call = None       # a call made by the terminator-less end: handled in insns as pseudo
        self.delay = None


class Func:
    def __init__(self, entry):
        self.entry = entry
        self.blocks = {}
        self.fail = None


def build_cfg(entry, W, base, end, entries, next_entry):
    """Blocks of the function at `entry`. Raises Unsupported when it cannot be followed."""
    f = Func(entry)
    # a function that builds its own return address (the start-up code) does not return through ra
    ra_built = False
    a_ = entry
    while a_ < min(next_entry, end):
        w_ = W(a_)
        op_, rs_, rt_, rd_, sh_, fn_ = fields(w_)
        if (op_ in (8, 9, 10, 11, 12, 13, 14, 15) and rt_ == 31) or (op_ == 0 and fn_ in (0, 2, 3, 4, 6, 7, 16, 18, 32, 33, 34, 35, 36, 37, 38, 39, 42, 43) and rd_ == 31):
            ra_built = True
            break
        a_ += 4
    leaders = {entry}
    seen = set()
    work = [entry]
    info = {}    # addr -> classification
    def at(a):
        if not (base <= a < end) or a & 3: raise Unsupported('flow leaves the image at %08X' % a)
        return W(a)
    while work:
        a = work.pop()
        while a not in seen:
            if a >= next_entry and a != entry and a in entries and a != entry:
                # runs into the next function: a fall-through (a tail without a jump); treat as tail call
                info[a] = ('fall',)
                break
            seen.add(a)
            i = at(a)
            k = kind(i)
            if k is None:
                a += 4
                continue
            d = at(a + 4)
            if kind(d) is not None:
                raise Unsupported('branch in a delay slot at %08X' % a)
            seen.add(a + 4)
            op, rs, rt, rd, sh, fn = fields(i)
            if k == 'cond':
                t = (a + 4 + (simm(i) << 2)) & 0xFFFFFFFF
                info[a] = ('cond', t, a + 8)
                leaders.add(t); leaders.add(a + 8)
                work.append(t)
                a += 8
                continue
            if k == 'bal':
                raise Unsupported('branch and link at %08X' % a)
            if k == 'j':
                t = (a & 0xF0000000) | ((i & 0x3FFFFFF) << 2)
                if (t in entries and t != entry) or not (base <= t < end):
                    info[a] = ('tail', t)
                else:
                    info[a] = ('goto', t)
                    leaders.add(t)
                    work.append(t)
                break
            if k == 'jal':
                t = (a & 0xF0000000) | ((i & 0x3FFFFFF) << 2)
                info[a] = ('call', t)
                leaders.add(a + 8)
                a += 8
                continue
            if k == 'jalr':
                info[a] = ('callind', rs, rd)
                leaders.add(a + 8)
                a += 8
                continue
            if k == 'jr':
                if rs == 31:
                    info[a] = ('ret',)
                else:
                    tab = find_table(W, a, rs, base, end, entry, next_entry)
                    if tab:
                        info[a] = ('switch', rs, tab[1], None)
                        for t in tab[1]:
                            leaders.add(t)
                            work.append(t)
                    else:
                        raise Unsupported('indirect jump at %08X not through a table' % a)
                break
            raise Unsupported('control %s' % k)
        # falls through into code that was seen: the earlier leader splits it
    # blocks
    order = sorted(a for a in seen if a in leaders)
    starts = sorted(leaders & seen)
    for s in starts:
        b = Block(s)
        a = s
        while True:
            if a != s and a in leaders: 
                b.term = ('goto', a)
                break
            inf = info.get(a)
            if inf is None:
                if a not in seen:
                    raise Unsupported('block runs out at %08X' % a)
                b.insns.append((a, W(a)))
                a += 4
                continue
            b.delay = (a + 4, W(a + 4))
            if inf[0] == 'fall':
                b.term = ('tail', a)
                break
            b.insns.append((a, W(a)))        # the branch itself (decoded again at emission)
            b.term = inf
            b.term_at = a
            break
        f.blocks[s] = b
    return f


def find_table(W, a_jr, rx, base, end, entry, next_entry):
    """Targets of the table an indirect jump at a_jr goes through, from the straight-line code before it."""
    # the window: back to just after the previous control instruction's delay slot
    lo = a_jr - 4
    n = 0
    while n < 120 and lo - 4 >= base:
        pk = kind(W(lo - 4))
        # conditional branches (the bounds check) are looked through; calls and jumps end the window
        if pk in ('jal', 'jalr', 'j', 'jr'):
            break
        if lo - 8 >= base and kind(W(lo - 8)) in ('jal', 'jalr', 'j', 'jr'):
            break
        lo -= 4
        n += 1
    vals = {}
    tbl = None
    for a in range(lo, a_jr, 4):
        i = W(a)
        op, rs, rt, rd, sh, fn = fields(i)
        if op == 15:
            vals[rt] = ('const', (i & 0xFFFF) << 16)
        elif op in (9, 8) and rs in vals and vals[rs][0] == 'const':
            vals[rt] = ('const', (vals[rs][1] + simm(i)) & 0xFFFFFFFF)
        elif op == 13 and rs in vals and vals[rs][0] == 'const':
            vals[rt] = ('const', vals[rs][1] | (i & 0xFFFF))
        elif op == 0 and fn == 33:
            if rs in vals and vals[rs][0] == 'const' and rt not in vals:
                vals[rd] = ('base', vals[rs][1])
            elif rt in vals and vals[rt][0] == 'const' and rs not in vals:
                vals[rd] = ('base', vals[rt][1])
            else:
                vals.pop(rd, None)
        elif op == 35 and rs in vals and vals[rs][0] in ('base', 'const') and rt == rx:
            tbl = (vals[rs][1] + simm(i)) & 0xFFFFFFFF
            vals.pop(rt, None)
        else:
            try:
                _, defs = uses_defs(i)
            except Unsupported:
                defs = set()
            for r in defs: vals.pop(r, None)
    if tbl is None or not (base <= tbl < end): return None
    targets = []
    a = tbl
    while base <= a < end and len(targets) < 1024:
        t = W(a)
        if t & 3 or not (base <= t < end) or t < entry or (t >= next_entry and next_entry > entry):
            break
        targets.append(t)
        a += 4
    if len(targets) < 2: return None
    return tbl, targets


def jump_table(f, b, W, base, end, entry, next_entry):
    """The targets of an indirect jump through a table, found by looking back through the block."""
    a_jr, rx = b.term_at, b.term[1]
    vals = {}
    insns = [x for x in b.insns[:-1]]
    # symbolic: register -> ('const', v) | ('lui', v) | other
    tbl = None
    for (a, i) in insns:
        op, rs, rt, rd, sh, fn = fields(i)
        if op == 15:
            vals[rt] = ('const', (i & 0xFFFF) << 16)
        elif op in (9, 8) and rs in vals and vals[rs][0] == 'const':
            vals[rt] = ('const', (vals[rs][1] + simm(i)) & 0xFFFFFFFF)
        elif op == 13 and rs in vals and vals[rs][0] == 'const':
            vals[rt] = ('const', vals[rs][1] | (i & 0xFFFF))
        elif op == 0 and fn == 33:      # addu: keep a constant when the other operand is unknown (index)
            if rs in vals and vals[rs][0] == 'const' and rt not in vals:
                vals[rd] = ('base', vals[rs][1])
            elif rt in vals and vals[rt][0] == 'const' and rs not in vals:
                vals[rd] = ('base', vals[rt][1])
            else:
                vals.pop(rd, None)
        elif op == 35 and rs in vals and vals[rs][0] in ('base', 'const') and rt == rx:
            tbl = (vals[rs][1] + simm(i)) & 0xFFFFFFFF
            vals.pop(rt, None)
        else:
            try:
                _, defs = uses_defs(i)
            except Unsupported:
                defs = set()
            for r in defs: vals.pop(r, None)
    # the delay slot of jr may also be the load (lw in the delay slot is not allowed before the jr in the old
    # architecture; the compiler puts a nop there)
    if tbl is None: return None
    targets = []
    a = tbl
    while base <= a < end and len(targets) < 4096:
        t = W(a)
        if t is None or t & 3 or not (entry <= t < max(next_entry, entry + 4)) and not (base <= t < end and t < next_entry and t >= entry):
            break
        targets.append(t)
        a += 4
    if not targets: return None
    return tbl, targets


def structure(f):
    """Dominator tree, reverse post-order, loop headers and merge nodes."""
    entry = f.entry
    succ = {a: [] for a in f.blocks}
    for a, b in f.blocks.items():
        t = b.term
        if t[0] == 'goto': succ[a] = [t[1]]
        elif t[0] == 'cond': succ[a] = [t[1], t[2]] if t[1] != t[2] else [t[1]]
        elif t[0] == 'switch': succ[a] = list(dict.fromkeys(t[2] + [t[3]] if t[3] else t[2]))
        elif t[0] in ('call', 'callind'): succ[a] = [b.term_at + 8]
        else: succ[a] = []
    for a in succ:
        for s in succ[a]:
            if s not in f.blocks:
                raise Unsupported('edge to %08X outside the blocks' % s)
    # reverse post-order
    seen = set(); post = []
    stack = [(entry, iter(succ[entry]))]
    seen.add(entry)
    while stack:
        n, it = stack[-1]
        for s in it:
            if s not in seen:
                seen.add(s)
                stack.append((s, iter(succ[s])))
                break
        else:
            post.append(n)
            stack.pop()
    rpo = list(reversed(post))
    num = {a: i for i, a in enumerate(rpo)}
    preds = {a: [] for a in rpo}
    for a in rpo:
        for s in succ[a]:
            preds[s].append(a)
    # immediate dominators (Cooper, Harvey, Kennedy)
    idom = {entry: entry}
    changed = True
    def intersect(a, b):
        while a != b:
            while num[a] > num[b]: a = idom[a]
            while num[b] > num[a]: b = idom[b]
        return a
    while changed:
        changed = False
        for n in rpo[1:]:
            ps = [p for p in preds[n] if p in idom]
            nd = ps[0]
            for p in ps[1:]: nd = intersect(nd, p)
            if idom.get(n) != nd:
                idom[n] = nd
                changed = True
    def dominates(a, b):
        while True:
            if a == b: return True
            if b == entry: return False
            b = idom[b]
    children = {a: [] for a in rpo}
    for n in rpo[1:]:
        children[idom[n]].append(n)
    back = set()
    for a in rpo:
        for s in succ[a]:
            if num[s] <= num[a]:
                if not dominates(s, a):
                    raise Unsupported('irreducible flow')
                back.add((a, s))
    headers = {s for (_, s) in back}
    fwd_in = {a: 0 for a in rpo}
    for a in rpo:
        for s in succ[a]:
            if (a, s) not in back:
                fwd_in[s] += 1
    merge = {a for a in rpo if fwd_in[a] >= 2}
    return rpo, num, succ, children, headers, back, merge


# ---- emission -------------------------------------------------------------------------------------------------

LOAD = {32: 'lb', 33: 'lh', 35: 'lw', 36: 'lbu', 37: 'lhu'}
STORE = {40: 'sb', 41: 'sh', 43: 'sw'}


def stmt(a, i):
    """Rust for one non-control instruction (registers are locals)."""
    op, rs, rt, rd, sh, fn = fields(i)
    R = reg
    def set_(r, e):
        return '' if r == 0 else 'r%d = %s;' % (r, e)
    if i == 0: return ''
    if op == 0:
        if fn == 0: return set_(rd, '%s << %d' % (R(rt), sh))
        if fn == 2: return set_(rd, '%s >> %d' % (R(rt), sh))
        if fn == 3: return set_(rd, '((%s as i32) >> %d) as u32' % (R(rt), sh))
        if fn == 4: return set_(rd, '%s << (%s & 31)' % (R(rt), R(rs)))
        if fn == 6: return set_(rd, '%s >> (%s & 31)' % (R(rt), R(rs)))
        if fn == 7: return set_(rd, '((%s as i32) >> (%s & 31)) as u32' % (R(rt), R(rs)))
        if fn == 13: return 'g.brk(0x%X);' % ((i >> 6) & 0xFFFFF)
        if fn == 16: return set_(rd, 'hi')
        if fn == 18: return set_(rd, 'lo')
        if fn == 17: return 'hi = %s;' % R(rs)
        if fn == 19: return 'lo = %s;' % R(rs)
        if fn == 24: return '{ let p = (%s as i32 as i64) * (%s as i32 as i64); hi = (p >> 32) as u32; lo = p as u32; }' % (R(rs), R(rt))
        if fn == 25: return '{ let p = (%s as u64) * (%s as u64); hi = (p >> 32) as u32; lo = p as u32; }' % (R(rs), R(rt))
        if fn == 26:
            return ('{ let (n, d) = (%s as i32, %s as i32); if d == 0 { lo = if n < 0 { 1 } else { 0xFFFF_FFFF }; hi = n as u32; } '
                    'else if n == i32::MIN && d == -1 { lo = 0x8000_0000; hi = 0; } else { lo = (n / d) as u32; hi = (n %% d) as u32; } }') % (R(rs), R(rt))
        if fn == 27:
            return ('{ let (n, d) = (%s, %s); if d == 0 { lo = 0xFFFF_FFFF; hi = n; } else { lo = n / d; hi = n %% d; } }') % (R(rs), R(rt))
        if fn in (32, 33): return set_(rd, '%s.wrapping_add(%s)' % (R(rs), R(rt)))
        if fn in (34, 35): return set_(rd, '%s.wrapping_sub(%s)' % (R(rs), R(rt)))
        if fn == 36: return set_(rd, '%s & %s' % (R(rs), R(rt)))
        if fn == 37: return set_(rd, '%s | %s' % (R(rs), R(rt)))
        if fn == 38: return set_(rd, '%s ^ %s' % (R(rs), R(rt)))
        if fn == 39: return set_(rd, '!(%s | %s)' % (R(rs), R(rt)))
        if fn == 42: return set_(rd, '((%s as i32) < (%s as i32)) as u32' % (R(rs), R(rt)))
        if fn == 43: return set_(rd, '(%s < %s) as u32' % (R(rs), R(rt)))
        raise Unsupported('special %d' % fn)
    if op in (8, 9): return set_(rt, '%s.wrapping_add(0x%08X)' % (R(rs), imm_u(i)))
    if op == 10: return set_(rt, '((%s as i32) < %d) as u32' % (R(rs), simm(i)))
    if op == 11: return set_(rt, '(%s < 0x%08X) as u32' % (R(rs), imm_u(i)))
    if op == 12: return set_(rt, '%s & 0x%X' % (R(rs), i & 0xFFFF))
    if op == 13: return set_(rt, '%s | 0x%X' % (R(rs), i & 0xFFFF))
    if op == 14: return set_(rt, '%s ^ 0x%X' % (R(rs), i & 0xFFFF))
    if op == 15: return set_(rt, '0x%08X' % ((i & 0xFFFF) << 16))
    ea = '%s.wrapping_add(0x%08X)' % (R(rs), imm_u(i))
    if op in LOAD: return set_(rt, 'g.%s(%s)' % (LOAD[op], ea))
    if op == 34: return set_(rt, 'g.lwl(%s, %s)' % (ea, R(rt)))
    if op == 38: return set_(rt, 'g.lwr(%s, %s)' % (ea, R(rt)))
    if op in STORE: return 'g.%s(%s, %s);' % (STORE[op], ea, R(rt))
    if op == 42: return 'g.swl(%s, %s);' % (ea, R(rt))
    if op == 46: return 'g.swr(%s, %s);' % (ea, R(rt))
    if op == 18:
        if rs == 0: return set_(rt, 'g.gte_data(%d)' % rd)
        if rs == 2: return set_(rt, 'g.gte_ctrl(%d)' % rd)
        if rs == 4: return 'g.gte_set_data(%d, %s);' % (rd, R(rt))
        if rs == 6: return 'g.gte_set_ctrl(%d, %s);' % (rd, R(rt))
        if rs & 0x10: return 'g.gte_cmd(0x%X);' % (i & 0x1FFFFFF)
    if op == 50: return '{ let v = g.lw(%s); g.gte_set_data(%d, v); }' % (ea, rt)
    if op == 58: return 'g.sw(%s, g.gte_data(%d));' % (ea, rt)
    raise Unsupported('op %d' % op)


def cond_expr(i):
    op, rs, rt, rd, sh, fn = fields(i)
    R = reg
    if op == 4: return '%s == %s' % (R(rs), R(rt))
    if op == 5: return '%s != %s' % (R(rs), R(rt))
    if op == 6: return '(%s as i32) <= 0' % R(rs)
    if op == 7: return '(%s as i32) > 0' % R(rs)
    if op == 1: return '(%s as i32) >= 0' % R(rs) if rt & 1 else '(%s as i32) < 0' % R(rs)
    raise Unsupported('cond op %d' % op)


class Emitter:
    def __init__(self, f, W, resolve_call):
        self.f = f
        self.W = W
        self.resolve = resolve_call
        self.lines = []
        self.ind = 1
        self.used = set()
        self.defd = set()
        self.calls = 0

    def w(self, s):
        self.lines.append('    ' * self.ind + s)

    def track(self, i):
        u, d = uses_defs(i)
        self.used |= u
        self.defd |= d

    def block_body(self, b):
        """Statements of the straight-line part of a block, then its delay slot and terminator pieces. Returns the code
        for the terminator as ('goto'|...) data prepared in b.term."""
        last = len(b.insns) - 1 if getattr(b, 'term_at', None) is not None else len(b.insns)
        for (a, i) in b.insns[:last]:
            self.track(i)
            s = stmt(a, i)
            if s: self.w(s)

    def flush_callee_saved(self):
        # v0 goes along: the host's own functions leave it as they find it (a call through a pointer in v0 returns the pointer)
        self.w('g.r[2] = r2;')
        for r in sorted(CALLEE_SAVED & self.defd):
            self.w('g.r[%d] = r%d;' % (r, r))
        self.w('g.sp = r29;')

    def reload_callee_saved(self):
        for r in sorted(CALLEE_SAVED & self.defd):
            self.w('r%d = g.r[%d];' % (r, r))

    def emit_terminator(self, b, ctx, tree):
        t = b.term
        a = getattr(b, 'term_at', None)
        k = t[0]
        if a is None and k == 'goto':
            return ('goto', t[1])
        if a is not None and k == 'goto':
            d = self.W(a + 4)
            self.track(d)
            s_ = stmt(a + 4, d)
            if s_: self.w(s_)
            return ('goto', t[1])
        if a is None:
            if k != 'tail': raise Unsupported('block without a branch')
            self.flush_callee_saved()
            self.w('g.r[3] = r3;')
            for r in (4, 5, 6, 7): self.used.add(r)
            self.used.add(31)
            self.w('let v = %s;' % self.resolve(t[1], None, 'r4, r5, r6, r7'))
            self.w('if r31 != ra { return g.call_addr(r31, ra, r4, r5, r6, r7); }')
            self.w('return v;')
            return ('ret',)
        i = self.W(a)
        d = self.W(a + 4)
        if k == 'cond':
            c = cond_expr(i)
            self.track(d)
            self.used |= uses_defs_cond(i)
            self.w('let c = %s;' % c)
            s = stmt(a + 4, d)
            if s: self.w(s)
            return ('cond', 'c', t[1], t[2])
        if k in ('call', 'callind'):
            if k == 'callind':
                self.used.add(t[1])
                self.w('let tgt = %s;' % reg(t[1]))
            self.track(d)
            s = stmt(a + 4, d)
            if s: self.w(s)
            ret = a + 8
            self.flush_callee_saved()
            self.calls += 1
            args = 'r4, r5, r6, r7'
            for r in (4, 5, 6, 7): self.used.add(r)
            if k == 'call':
                expr = self.resolve(t[1], ret, args)
            else:
                expr = 'g.call_addr(tgt, 0x%08X, %s)' % (ret, args)
            if 'g.call_addr' in expr:
                # code that is not decompiled may read any register (the hand-written assembly does): all go to the machine
                for r in sorted((self.defd | self.used) - CALLEE_SAVED - {0, 2, 29, 31, 4, 5, 6, 7}):
                    self.w('g.r[%d] = r%d;' % (r, r))
            self.w('r2 = %s;' % expr)
            self.w('r3 = g.r[3];')
            self.defd |= {2, 3}
            self.reload_callee_saved()
            return ('goto', ret)
        if k == 'ret':
            self.track(d)
            s = stmt(a + 4, d)
            if s: self.w(s)
            for r in sorted(CALLEE_SAVED & self.defd):
                self.w('g.r[%d] = r%d;' % (r, r))
            self.w('g.r[3] = r3;')
            self.w('g.sp = r29;')
            if 31 in self.defd:
                # a function that built its own return address does not go back to its caller
                for r in (4, 5, 6, 7): self.used.add(r)
                self.w('if r31 != ra { return g.call_addr(r31, ra, r4, r5, r6, r7); }')
            self.w('return r2;')
            return ('ret',)
        if k == 'tail':
            if a is not None:
                self.track(d)
                s = stmt(a + 4, d)
                if s: self.w(s)
            self.flush_callee_saved()
            self.w('g.r[3] = r3;')
            for r in (4, 5, 6, 7): self.used.add(r)
            self.used.add(31)
            self.w('let v = %s;' % self.resolve(t[1], None, 'r4, r5, r6, r7'))
            self.w('if r31 != ra { return g.call_addr(r31, ra, r4, r5, r6, r7); }')
            self.w('return v;')
            return ('ret',)
        if k == 'switch':
            self.used.add(t[1])
            self.w('let tgt = %s;' % reg(t[1]))
            self.track(d)
            s_ = stmt(a + 4, d)
            if s_: self.w(s_)
            return ('switch', 'tgt', t[2])
        if k == 'jr':
            self.used.add(t[1])
            self.w('let tgt = %s;' % reg(t[1]))
            self.track(d)
            s = stmt(a + 4, d)
            if s: self.w(s)
            tab = jump_table_cache.get(b.start)
            if tab:
                return ('switch', 'tgt', tab[1])
            self.flush_callee_saved()
            self.w('g.r[3] = r3;')
            for r in (4, 5, 6, 7): self.used.add(r)
            self.used.add(31)
            self.w('return g.call_addr(tgt, r31, r4, r5, r6, r7);')
            return ('ret',)
        raise Unsupported('terminator ' + k)


jump_table_cache = {}
TRACE = False


def uses_defs_cond(i):
    op, rs, rt, rd, sh, fn = fields(i)
    return {rs, rt} if op in (4, 5) else {rs}


def decompile(f, W, resolve_call, base, end, entries, entry, next_entry):
    """Rust source of one function, plus (live-in registers, calls)."""
    # jump tables first (they change the successors)
    for a, b in f.blocks.items():
        if b.term[0] == 'jr' and False:
            jt = jump_table(f, b, W, base, end, entry, next_entry)
            if jt is None:
                b.term = ('jr', b.term[1])
            else:
                tbl, targets = jt
                jump_table_cache[a] = (tbl, targets)
                b.term = ('switch', b.term[1], targets, None)
                for t in targets:
                    if t not in f.blocks:
                        raise Unsupported('jump table target %08X is not a block' % t)
    rpo, num, succ, children, headers, back, merge = structure(f)
    em = Emitter(f, W, resolve_call)
    _el, _bad, em.live_in = liveness_check(f, rpo, None)
    out = []
    labels = {}

    def do_tree(x, ctx):
        kids = [y for y in children[x] if y in merge]
        if x in headers:
            em.w("'l%08X: loop {" % x)
            em.ind += 1
            em.w('g.tick();')
            node_within(x, kids, ctx + [('loop', x)])
            em.ind -= 1
            em.w('}')
        else:
            node_within(x, kids, ctx)

    def node_within(x, ys, ctx):
        if not ys and TRACE:
            mask_ = 0
            for r_ in em.live_in.get(x, set()):
                mask_ |= 1 << r_
            em.w('g.tb(0x%08X, 0x%08X, [%s]);' % (x, mask_, ', '.join(['0u32'] + ['r%d' % n for n in range(1, 32)])))
        if not ys:
            b_ = f.blocks[x]
            n_ = len(b_.insns) + (1 if getattr(b_, 'term_at', None) is not None else 0)
            if n_: em.w('g.cost(%d);' % n_)
        if ys:
            ys = sorted(ys, key=lambda y: -num[y])
            y1 = ys[0]
            em.w("'b%08X: {" % y1)
            em.ind += 1
            node_within(x, ys[1:], ctx + [('block', y1)])
            em.ind -= 1
            em.w('}')
            do_tree(y1, ctx)
            return
        b = f.blocks[x]
        em.block_body(b)
        res = em.emit_terminator(b, ctx, None)
        if res[0] == 'ret':
            return
        if res[0] == 'goto':
            do_branch(x, res[1], ctx)
        elif res[0] == 'cond':
            t, fl = res[2], res[3]
            em.w('if %s {' % res[1])
            em.ind += 1
            do_branch(x, t, ctx)
            em.ind -= 1
            em.w('} else {')
            em.ind += 1
            do_branch(x, fl, ctx)
            em.ind -= 1
            em.w('}')
        elif res[0] == 'switch':
            em.w('match %s {' % res[1])
            em.ind += 1
            seen = {}
            for t in res[2]:
                seen.setdefault(t, True)
            for t in seen:
                em.w('0x%08X => {' % t)
                em.ind += 1
                do_branch(x, t, ctx)
                em.ind -= 1
                em.w('}')
            em.w('_ => unreachable!(),')
            em.ind -= 1
            em.w('}')

    def do_branch(src, tgt, ctx):
        if (src, tgt) in back:
            em.w("continue 'l%08X;" % tgt)
        elif tgt in merge:
            em.w("break 'b%08X;" % tgt)
        else:
            do_tree(tgt, ctx)

    do_tree(f.entry, [])
    return em, rpo


def liveness_check(f, rpo, succ_fn):
    """Registers read before written on some path from the entry (live-in) and whether a caller-saved register is read
    after a call without being written."""
    use = {}; defs = {}
    callsite = {}
    for a, b in f.blocks.items():
        u, d = set(), set()
        seq = list(b.insns)
        term = b.term
        pieces = []
        for (ia, i) in seq:
            if getattr(b, 'term_at', None) == ia: continue
            pieces.append(i)
        if getattr(b, 'term_at', None) is not None:
            ta = b.term_at
            i = [x for (aa, x) in seq if aa == ta][0]
            op, rs, rt, rd, sh, fn = fields(i)
            # the branch reads first, then the delay slot runs
            ru = set()
            if term[0] == 'cond': ru = uses_defs_cond(i)
            elif term[0] in ('jr', 'callind', 'switch'): ru = {term[1]}
            for x in pieces:
                uu, dd = uses_defs(x)
                u |= (uu - d); d |= dd
            u |= (ru - d)
            if b.delay:
                uu, dd = uses_defs(b.delay[1])
                u |= (uu - d); d |= dd
            if term[0] in ('call', 'callind'):
                u |= ({4, 5, 6, 7} - d)
                d |= CLOBBERED | {2, 3}
                callsite[a] = True
            elif term[0] == 'tail' or term[0] == 'jr':
                u |= ({4, 5, 6, 7} - d)
            elif term[0] == 'ret':
                pass
        else:
            for x in pieces:
                uu, dd = uses_defs(x)
                u |= (uu - d); d |= dd
        use[a] = u; defs[a] = d
    # backward dataflow
    live_in = {a: set() for a in f.blocks}
    changed = True
    sc = {}
    for a, b in f.blocks.items():
        t = b.term
        if t[0] == 'goto': sc[a] = [t[1]]
        elif t[0] == 'cond': sc[a] = [t[1], t[2]]
        elif t[0] == 'switch': sc[a] = list(t[2])
        elif t[0] in ('call', 'callind'): sc[a] = [b.term_at + 8]
        else: sc[a] = []
    while changed:
        changed = False
        for a in f.blocks:
            out = set()
            for s in sc[a]: out |= live_in.get(s, set())
            nin = use[a] | (out - defs[a])
            if nin != live_in[a]:
                live_in[a] = nin
                changed = True
    entry_live = live_in[f.entry]
    bad_after_call = set()
    for a, b in f.blocks.items():
        if a in callsite:
            out = set()
            for s in sc[a]: out |= live_in.get(s, set())
            bad_after_call |= (out & (CLOBBERED - {2, 3}))
    return entry_live, bad_after_call, live_in



ALLOWED_IN = {0, 4, 5, 6, 7, 29, 31} | CALLEE_SAVED


def host_addresses():
    """Addresses the host answers itself (hooks and native services): the code there is never run, so nothing decompiled may
    stand in for them."""
    import re
    root = os.path.dirname(os.path.dirname(HERE))
    src = open(os.path.join(root, 'src', 'machine.rs'), encoding='utf-8').read()
    inst = src[src.index('fn install_program'):src.index('fn observe')]
    hooked = set(int(x, 16) for x in re.findall(r'0x(8[0-9A-Fa-f]{7})', inst))
    svc = open(os.path.join(root, 'src', 'machine', 'services.rs'), encoding='utf-8').read()
    hooked |= set(int(x, 16) for x in re.findall(r'\(\d+, 0x(8[0-9A-Fa-f]{7}), \[', svc))
    return hooked


def referenced_entries(base, words):
    """Addresses something calls or points at: jal targets and words that hold the address of a prologue."""
    end = base + 4 * len(words)
    out = set()
    for i, w in enumerate(words):
        if (w >> 26) == 3:
            t = ((base + 4 * i) & 0xF0000000) | ((w & 0x3FFFFFF) << 2)
            if base <= t < end: out.add(t)
        elif (w >> 26) == 2:
            # a jump to another function's prologue is a tail call
            t = ((base + 4 * i) & 0xF0000000) | ((w & 0x3FFFFFF) << 2)
            if base <= t < end and not t & 3:
                x = words[(t - base) >> 2]
                if (x >> 16) == 0x27BD and (x & 0x8000) and abs(t - (base + 4 * i)) > 0x40: out.add(t)
        elif base <= w < end and not w & 3:
            x = words[(w - base) >> 2]
            if (x >> 16) == 0x27BD and (x & 0x8000): out.add(w)
    return out
NL = chr(10)


def func_name(a):
    return 'f_%08X' % a


def gen_function(f, W, base, end, entries, entry, next_entry, resolve):
    """(rust text, live-in registers, registers used after a call that the call clobbered)."""
    jump_table_cache.clear()
    em, rpo = decompile(f, W, resolve, base, end, entries, entry, next_entry)
    entry_live, bad, _live = liveness_check(f, rpo, None)
    regs = sorted((em.used | em.defd | {2, 3}) - {0})
    if TRACE:
        regs = list(range(1, 32))
    head = []
    head.append('#[allow(unused_mut, unused_assignments, unused_variables, unreachable_code, unused_labels, non_snake_case)]')
    head.append('pub unsafe fn %s(g: &mut G, ra: u32, a0: u32, a1: u32, a2: u32, a3: u32) -> u32 {' % func_name(entry))
    for r in regs:
        if r == 4: head.append('    let mut r4 = a0;')
        elif r == 5: head.append('    let mut r5 = a1;')
        elif r == 6: head.append('    let mut r6 = a2;')
        elif r == 7: head.append('    let mut r7 = a3;')
        elif r == 29: head.append('    let mut r29 = g.sp;')
        elif r == 31: head.append('    let mut r31 = ra;')
        elif r in CALLEE_SAVED: head.append('    let mut r%d = g.r[%d];' % (r, r))
        elif r in (2, 3): head.append('    let mut r%d = g.r[%d];' % (r, r))
        else: head.append('    let mut r%d = 0u32;' % r)
    if 29 not in regs: head.append('    let mut r29 = g.sp;')
    head.append('    let mut hi = 0u32; let mut lo = 0u32;')
    tail = ['    unreachable!()', '}']
    return NL.join(head + em.lines + tail) + NL, entry_live, bad


def generate(image_index, main_ok=None, main_module='main_exe', is_main=False, dynamic=False):
    rows = json.load(open(os.path.join(HERE, '..', 'overlays.json')))
    row = rows[image_index]
    base, words = load_image(row)
    end = base + 4 * len(words)
    W = lambda a: words[(a - base) >> 2]
    entries = referenced_entries(base, words)
    elist = sorted(entries)
    nxt = {a: (elist[k + 1] if k + 1 < len(elist) else end) for k, a in enumerate(elist)}
    host = host_addresses()
    elist = [e for e in elist if e not in host]
    ok = set()
    fail = {}
    def dyn(t, ret, args):
        return 'g.call_addr(0x%08X, %s, %s)' % (t, '0x%08X' % ret if ret is not None else 'r31', args)
    for e in elist:
        try:
            f = build_cfg(e, W, base, end, entries, nxt[e])
            t, live, bad = gen_function(f, W, base, end, entries, e, nxt[e], dyn)
        except Unsupported as ex:
            fail[e] = str(ex)
            continue
        except Exception as ex:
            fail[e] = 'error %s: %s' % (type(ex).__name__, ex)
            continue
        if live - ALLOWED_IN:
            fail[e] = 'reads %s it was not given' % sorted(live - ALLOWED_IN)
            continue
        ok.add(e)
    def direct(t, ret, args):
        r = '0x%08X' % ret if ret is not None else 'r31'
        if dynamic:
            return dyn(t, ret, args)
        if t in ok:
            return '%s(g, %s, %s)' % (func_name(t), r, args)
        if main_ok is not None and t in main_ok and not (base <= t < end):
            return 'crate::%s::%s(g, %s, %s)' % (main_module, func_name(t), r, args)
        return dyn(t, ret, args)
    out = {}
    for e in sorted(ok):
        f = build_cfg(e, W, base, end, entries, nxt[e])
        t, live, bad = gen_function(f, W, base, end, entries, e, nxt[e], direct)
        out[e] = t
    return row, base, words, elist, ok, fail, out


if __name__ == '__main__' and not (len(sys.argv) > 1 and sys.argv[1] in ('write', 'writeall')):
    idx = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    row, base, words, elist, ok, fail, out = generate(idx)
    print('%s: %d functions, %d decompiled, %d left to the blocks' % (row[0], len(elist), len(ok), len(fail)))
    c = collections.Counter(' '.join(v.split(' ')[:2]) for v in fail.values())
    for k, v in c.most_common(10): print('  %5d  %s' % (v, k))
    for e in sorted(fail)[:15]: print('   %08X %s' % (e, fail[e]))
    print('source bytes', sum(len(t) for t in out.values()))


def write_module(image_index, path, main_ok=None, dynamic=False):
    row, base, words, elist, ok, fail, out = generate(image_index, main_ok=main_ok, dynamic=dynamic)
    lines = ['// Generated by tools/decomp/decomp.py from %s (loaded at 0x%08X): do not edit.' % (row[0], base),
             'use crate::G;', '']
    for e in sorted(out):
        lines.append(out[e])
    lines.append('')
    lines.append('pub static TABLE: &[(u32, [u32; 6], crate::DecFn)] = &[')
    for e in sorted(out):
        sig = ', '.join('0x%08X' % words[((e - base) >> 2) + k] for k in range(6))
        lines.append('    (0x%08X, [%s], %s),' % (e, sig, func_name(e)))
    lines.append('];')
    open(path, 'w', encoding='utf-8', newline='\n').write(NL.join(lines) + NL)
    return ok, fail


if __name__ == '__main__' and len(sys.argv) > 2 and sys.argv[1] == 'write':
    if len(sys.argv) > 4 and 'trace' in sys.argv[4:]:
        TRACE = True
    ok, fail = write_module(int(sys.argv[2]), sys.argv[3], dynamic=(len(sys.argv) > 4 and 'dynamic' in sys.argv[4:]))
    print('written %s: %d functions, %d left' % (sys.argv[3], len(ok), len(fail)))


def write_all(out_dir, dynamic=False):
    rows = json.load(open(os.path.join(HERE, '..', 'overlays.json')))
    names = ['main_exe'] + ['ovl_%d' % i for i in range(1, len(rows))]
    ok0, fail0 = write_module(0, os.path.join(out_dir, names[0] + '.rs'), dynamic=dynamic)
    print('%-10s %-26s %5d decompiled %4d left' % (names[0], rows[0][0], len(ok0), len(fail0)))
    total = len(ok0)
    for i in range(1, len(rows)):
        ok, fail = write_module(i, os.path.join(out_dir, names[i] + '.rs'), main_ok=ok0, dynamic=dynamic)
        print('%-10s %-26s %5d decompiled %4d left' % (names[i], rows[i][0], len(ok), len(fail)))
        total += len(ok)
    lines = ['// Generated by tools/decomp/decomp.py: the modules and their function tables.', '']
    for n in names:
        lines.append('pub mod %s;' % n)
    lines.append('')
    lines.append("/// every module's table of (address, first words, function), in the order the machine hooks them")
    lines.append('pub static TABLES: &[&[(u32, [u32; 6], crate::DecFn)]] = &[')
    for n in names:
        lines.append('    %s::TABLE,' % n)
    lines.append('];')
    open(os.path.join(out_dir, 'modules.rs'), 'w', encoding='utf-8', newline=NL).write(NL.join(lines) + NL)
    print('total decompiled', total)


if __name__ == '__main__' and len(sys.argv) > 1 and sys.argv[1] == 'writeall':
    if 'trace' in sys.argv[3:]:
        TRACE = True
    write_all(sys.argv[2], dynamic=('dynamic' in sys.argv[3:]))
