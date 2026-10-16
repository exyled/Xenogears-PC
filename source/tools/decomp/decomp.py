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
import fold
sys.setrecursionlimit(20000)

FOLD = os.environ.get('XG_FOLD', '1') != '0'
ALLOC_SITES = {}      # (function, return address of the allocation call) -> kind: the pointer it returns is replaced (g.reloc(kind, ptr))
_structs = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'structs.json')
if os.path.exists(_structs):
    _sj = json.load(open(_structs))
    if '_state_block' in _sj:
        _b = _sj['_state_block']
        _lo, _n = int(_b['lo'], 16), int(_b['size'], 16)
        fold.REBASE = (_lo, _lo + _n, int(_b['guest'], 16) - _lo)
    for _n, _s in _sj.items():
        if _n.startswith('_'):
            continue
        fold.STRUCTS[_n] = {'fields': {int(k, 16): v for k, v in _s.get('fields', {}).items()},
                            'ptrs': {int(k, 16): v for k, v in _s.get('ptrs', {}).items()}}
        if 'global' in _s:
            fold.TYPED[int(_s['global'], 16)] = ('a' if _s.get('kind') == 'array' else 't', _n)
        if 'relocate' in _s:
            fold.RELOCATE[int(_s['global'], 16)] = int(_s['relocate'], 16)
        if 'alloc' in _s:
            ALLOC_SITES[(int(_s['alloc']['function'], 16), int(_s['alloc']['ret'], 16))] = _s['alloc'].get('kind', 1)
_names = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'names.json')
if os.path.exists(_names):
    fold.NAMES = {int(k, 16): v for k, v in json.load(open(_names)).items() if not k.startswith('_') and k != 'functions'}
if os.path.exists(_structs):
    for _n, _s in json.load(open(_structs)).items():
        if not _n.startswith('_') and 'global' in _s and 'global_name' in _s:
            fold.NAMES[int(_s['global'], 16)] = _s['global_name']

HERE = os.path.dirname(os.path.abspath(__file__))

# ---- registers ----
CALLEE_SAVED = set(range(16, 24)) | {28, 30}          # s0-s7, gp, s8
ARGS = {4, 5, 6, 7}
KEEP = CALLEE_SAVED | {29, 31, 0}                     # sp and ra are special
CLOBBERED = {1, 8, 9, 10, 11, 12, 13, 14, 15, 24, 25} | ARGS  # not v0 / v1


class Unsupported(Exception):
    pass


fold.Unsupported = Unsupported


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
    if op == 16:
        # system coprocessor: the machine reads it as 0 and ignores writes (the console's status register is not modelled)
        if rs in (0, 2): return set(), {rt}      # mfc0 / cfc0
        if rs in (4, 6): return {rt}, set()      # mtc0 / ctc0
        if rs & 0x10: return set(), set()        # rfe
        raise Unsupported('cop0 %d' % rs)
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
            # (the delay slot is not marked: another branch may land on it and go on from there)
            op, rs, rt, rd, sh, fn = fields(i)
            if k == 'cond' and op == 4 and rs == rt:
                # beq rX,rX: the branch that is always taken (the assembler's unconditional `b`)
                t = (a + 4 + (simm(i) << 2)) & 0xFFFFFFFF
                if (t in entries and t != entry) or not (base <= t < end):
                    info[a] = ('tail', t)
                else:
                    info[a] = ('goto', t)
                    leaders.add(t)
                    work.append(t)
                break
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
    if op == 16:
        if rs in (0, 2): return set_(rt, '0u32')
        return '/* cop0 */'
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
        self.sc = None
        self.type_in = {}
        self.reload = set()       # caller-saved registers the code reads after a call: the callee may leave results in them
        self.hilo = False         # the function multiplies or divides (or reads hi/lo): they are the machine's across calls
        self.all_defd = set()     # every register any instruction of the function writes
        self.helper = False       # takes registers beyond the arguments: what it leaves in them is its result as well
        self.fold = FOLD

    def w(self, s):
        self.lines.append('    ' * self.ind + s)

    def track(self, i):
        u, d = uses_defs(i)
        self.used |= u
        self.defd |= d

    def ins(self, a, i):
        """One instruction of the straight-line part (or a delay slot)."""
        self.track(i)
        if not self.fold:
            s = stmt(a, i)
            if s: self.w(s)
            return
        op = fold.lower(a, i)
        if op is not None:
            self.sc.run(op)

    def text(self, r):
        """The register's value as an expression (a pending value is written in), and the variable when folding is off."""
        if not self.fold:
            return reg(r)
        return self.sc.val(r)[0]

    def var(self, r):
        if not self.fold:
            return reg(r)
        return self.sc.var(r)

    def block_body(self, b):
        last = len(b.insns) - 1 if getattr(b, 'term_at', None) is not None else len(b.insns)
        for (a, i) in b.insns[:last]:
            self.ins(a, i)

    def flush_hilo(self):
        if self.hilo:
            self.w('*g.hi = hi; *g.lo = lo;')

    def flush_helper(self):
        self.flush_hilo()
        # what the function leaves in the caller-saved registers is read by code that called it by address (a callback
        # that clobbers a2 is seen to do so by the next callback the dispatcher calls): they go to the machine's registers
        for r in sorted(self.all_defd & (EXTRA_IN | ARGS)):
            self.w('g.r[%d] = %s;' % (r, self.var(r)))

    def flush_callee_saved(self):
        self.flush_helper()
        # v0 goes along: the host's own functions leave it as they find it (a call through a pointer in v0 returns the pointer)
        self.w('g.r[2] = %s;' % self.var(2))
        for r in sorted(CALLEE_SAVED & self.defd):
            self.w('g.r[%d] = %s;' % (r, self.var(r)))
        self.w('g.sp = %s;' % self.var(29))

    def clobbered(self, r):
        if self.fold:
            self.sc.clobber(r)

    def reload_callee_saved(self):
        for r in sorted(CALLEE_SAVED & self.defd):
            self.w('r%d = g.r[%d];' % (r, r))
            self.clobbered(r)

    def args(self, regs=(4, 5, 6, 7)):
        for r in regs:
            self.used.add(r)
        if not self.fold:
            return ', '.join(reg(r) for r in regs)
        self.sc.flush_impure()
        return ', '.join(self.sc.val(r)[0] for r in regs)

    def cond_text(self, i):
        op, rs, rt, rd, sh, fn = fields(i)
        if not self.fold:
            return cond_expr(i)
        v = lambda r: self.sc.val(r)[0]
        if op == 4: return '%s == %s' % (fold.recv(v(rs)), fold.opnd(v(rt)))
        if op == 5: return '%s != %s' % (fold.recv(v(rs)), fold.opnd(v(rt)))
        if op == 6: return '(%s as i32) <= 0' % fold.recv(v(rs))
        if op == 7: return '(%s as i32) > 0' % fold.recv(v(rs))
        if op == 1: return '(%s as i32) >= 0' % fold.recv(v(rs)) if rt & 1 else '(%s as i32) < 0' % fold.recv(v(rs))
        raise Unsupported('cond op %d' % op)

    def emit_block(self, b, live_out):
        """The code of one block and how it goes on. Two passes: the first counts what reads every value, the second writes."""
        if not self.fold:
            self.sc = None
            self.block_body(b)
            return self.emit_terminator(b)
        n0 = len(self.lines)
        used0, defd0, calls0 = set(self.used), set(self.defd), self.calls
        self.sc = fold.Scope(self, True, None, self.type_in.get(b.start))
        self.block_body(b)
        res = self.emit_terminator(b)
        if res[0] in ('goto', 'cond', 'switch'):
            self.sc.finish(live_out)
        uses = self.sc.cnt
        del self.lines[n0:]
        self.used, self.defd, self.calls = used0, defd0, calls0
        self.sc = fold.Scope(self, False, uses, self.type_in.get(b.start))
        self.block_body(b)
        res = self.emit_terminator(b)
        if res[0] in ('goto', 'cond', 'switch'):
            self.sc.finish(live_out)
        return res

    def emit_terminator(self, b):
        t = b.term
        a = getattr(b, 'term_at', None)
        k = t[0]
        if a is None and k == 'goto':
            return ('goto', t[1])
        if a is not None and k == 'goto':
            self.ins(a + 4, self.W(a + 4))
            return ('goto', t[1])
        if a is None:
            if k != 'tail': raise Unsupported('block without a branch')
            return self.emit_tail(t)
        i = self.W(a)
        d = self.W(a + 4)
        if k == 'cond':
            self.used |= uses_defs_cond(i)
            c = self.cond_text(i)
            self.w('let c = %s;' % c)
            self.ins(a + 4, d)
            return ('cond', 'c', t[1], t[2])
        if k in ('call', 'callind'):
            if k == 'callind':
                self.used.add(t[1])
                self.w('let tgt = %s;' % self.text(t[1]))
            self.ins(a + 4, d)
            ret = a + 8
            self.flush_callee_saved()
            self.calls += 1
            if k == 'call':
                probe = self.resolve(t[1], ret, '@ARGS@')
            else:
                probe = 'g.call_addr(tgt, 0x%08X, @ARGS@)' % ret
            if 'g.call_addr' in probe:
                # code that is not decompiled may read any register (the hand-written assembly does): all go to the machine
                for r in sorted(((self.defd | self.used | (self.all_defd & EXTRA_IN)) - CALLEE_SAVED - {0, 2, 29, 31, 4, 5, 6, 7}) | (self.reload & {4, 5, 6, 7})):
                    self.w('g.r[%d] = %s;' % (r, self.var(r)))
            expr = probe.replace('@ARGS@', self.args())
            self.w('r2 = %s;' % expr)
            self.clobbered(2)
            if (self.f.entry, ret) in ALLOC_SITES:
                self.w('r2 = g.reloc(%d, r2);' % ALLOC_SITES[(self.f.entry, ret)])
            self.w('r3 = g.r[3];')
            self.clobbered(3)
            if self.hilo:
                self.w('hi = *g.hi; lo = *g.lo;')
            self.defd |= {2, 3}
            for r in sorted(self.reload - {2, 3}):
                self.w('r%d = g.r[%d];' % (r, r))
                self.clobbered(r)
                self.defd.add(r)
            self.reload_callee_saved()
            return ('goto', ret)
        if k == 'ret':
            self.ins(a + 4, d)
            self.flush_helper()
            for r in sorted(CALLEE_SAVED & self.defd):
                self.w('g.r[%d] = %s;' % (r, self.var(r)))
            self.w('g.r[3] = %s;' % self.var(3))
            self.w('g.sp = %s;' % self.var(29))
            if 31 in self.defd:
                # a function that built its own return address does not go back to its caller
                r31 = self.var(31)
                a4 = self.args()
                self.w('if %s != ra { return g.call_addr(%s, ra, %s); }' % (r31, r31, a4))
            self.w('return %s;' % self.var(2))
            return ('ret',)
        if k == 'tail':
            self.ins(a + 4, d)
            return self.emit_tail(t)
        if k == 'switch':
            self.used.add(t[1])
            self.w('let tgt = %s;' % self.text(t[1]))
            self.ins(a + 4, d)
            return ('switch', 'tgt', t[2])
        if k == 'jr':
            self.used.add(t[1])
            self.w('let tgt = %s;' % self.text(t[1]))
            self.ins(a + 4, d)
            tab = jump_table_cache.get(b.start)
            if tab:
                return ('switch', 'tgt', tab[1])
            self.flush_callee_saved()
            self.w('g.r[3] = %s;' % self.var(3))
            self.used.add(31)
            r31 = self.var(31)
            a4 = self.args()
            self.w('return g.call_addr(tgt, %s, %s);' % (r31, a4))
            return ('ret',)
        raise Unsupported('terminator ' + k)

    def emit_tail(self, t):
        self.flush_callee_saved()
        self.w('g.r[3] = %s;' % self.var(3))
        self.used.add(31)
        self.var(31)
        a4 = self.args()
        self.w('let v = %s;' % self.resolve(t[1], None, a4))
        self.w('if %s != ra { return g.call_addr(%s, ra, %s); }' % (self.var(31), self.var(31), a4))
        self.w('return v;')
        return ('ret',)


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
    try:
        rpo, num, succ, children, headers, back, merge = structure(f)
    except Unsupported as ex:
        if str(ex) != 'irreducible flow':
            raise
        return decompile_flat(f, W, resolve_call, entry)
    em = Emitter(f, W, resolve_call)
    _el, _bad, em.live_in = liveness_check(f, rpo, None)
    em.helper = bool(_el & EXTRA_IN)
    em.all_defd = function_defs(f, W)
    em.hilo = function_uses_hilo(f, W)
    # the caller-saved registers the function writes anywhere: after a call they hold what the callee left, and that is what
    # the function returns in them unless it writes them again
    em.reload = set(_bad) | ((em.all_defd & (EXTRA_IN | ARGS)) - {2, 3})
    fold_in = fold.fold_liveness(f, W, succ, uses_defs, uses_defs_cond, CALLEE_SAVED, CLOBBERED, EXTRA_IN | ARGS) if FOLD else {}
    em.type_in = fold.infer_types(f, W, succ, uses_defs, CLOBBERED) if FOLD else {}

    def live_out_of(x):
        out = set()
        for s_ in succ.get(x, []):
            out |= fold_in.get(s_, set())
        return out
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
            lv_ = set(em.live_in.get(x, set()))
            if f.blocks[x].term[0] in ('ret', 'tail', 'jr'):
                lv_ |= CALLEE_SAVED      # what a function hands back in the saved registers is part of its result
            for r_ in lv_:
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
        res = em.emit_block(b, live_out_of(x))
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


def function_uses_hilo(f, W):
    for a, b in f.blocks.items():
        ta = getattr(b, 'term_at', None)
        ins = [i for (ia, i) in b.insns if ia != ta]
        if ta is not None:
            ins.append(W(ta + 4))
        for i in ins:
            if (i >> 26) == 0 and (i & 63) in (16, 17, 18, 19, 24, 25, 26, 27):
                return True
    return False


def function_defs(f, W):
    out = set()
    for a, b in f.blocks.items():
        ta = getattr(b, 'term_at', None)
        for (ia, i) in b.insns:
            if ta == ia:
                continue
            try:
                out |= uses_defs(i)[1]
            except Exception:
                pass
        if ta is not None:
            try:
                out |= uses_defs(W(ta + 4))[1]
            except Exception:
                pass
    return out


def block_succ(f):
    succ = {a: [] for a in f.blocks}
    for a, b in f.blocks.items():
        t = b.term
        if t[0] == 'goto': succ[a] = [t[1]]
        elif t[0] == 'cond': succ[a] = [t[1], t[2]] if t[1] != t[2] else [t[1]]
        elif t[0] == 'switch': succ[a] = list(dict.fromkeys(t[2] + [t[3]] if t[3] else t[2]))
        elif t[0] in ('call', 'callind'): succ[a] = [b.term_at + 8]
    for a in succ:
        for t in succ[a]:
            if t not in f.blocks:
                raise Unsupported('edge to %08X outside the blocks' % t)
    return succ


def decompile_flat(f, W, resolve_call, entry):
    """A function whose flow graph cannot be structured (jumps into the middle of loops): one loop around a match on the
    address of the block to run next. Each block is written exactly as in the structured form."""
    succ = block_succ(f)
    em = Emitter(f, W, resolve_call)
    _el, _bad, em.live_in = liveness_check(f, None, None)
    em.helper = bool(_el & EXTRA_IN)
    em.all_defd = function_defs(f, W)
    em.hilo = function_uses_hilo(f, W)
    # the caller-saved registers the function writes anywhere: after a call they hold what the callee left, and that is what
    # the function returns in them unless it writes them again
    em.reload = set(_bad) | ((em.all_defd & (EXTRA_IN | ARGS)) - {2, 3})
    fold_in = fold.fold_liveness(f, W, succ, uses_defs, uses_defs_cond, CALLEE_SAVED, CLOBBERED, EXTRA_IN | ARGS) if FOLD else {}
    em.type_in = fold.infer_types(f, W, succ, uses_defs, CLOBBERED) if FOLD else {}

    def live_out_of(x):
        out = set()
        for s_ in succ.get(x, []):
            out |= fold_in.get(s_, set())
        return out
    em.w('let mut pc: u32 = 0x%08X;' % f.entry)
    em.w("'dispatch: loop {")
    em.ind += 1
    em.w('g.tick();')
    em.w('match pc {')
    em.ind += 1
    for x in sorted(f.blocks):
        b = f.blocks[x]
        em.w('0x%08X => {' % x)
        em.ind += 1
        n_ = len(b.insns) + (1 if getattr(b, 'term_at', None) is not None else 0)
        if n_: em.w('g.cost(%d);' % n_)
        res = em.emit_block(b, live_out_of(x))
        if res[0] == 'goto':
            em.w("pc = 0x%08X; continue 'dispatch;" % res[1])
        elif res[0] == 'cond':
            em.w('pc = if %s { 0x%08X } else { 0x%08X };' % (res[1], res[2], res[3]))
            em.w("continue 'dispatch;")
        elif res[0] == 'switch':
            em.w("pc = %s; continue 'dispatch;" % res[1])
        em.ind -= 1
        em.w('}')
    em.w('_ => unreachable!(),')
    em.ind -= 1
    em.w('}')
    em.ind -= 1
    em.w('}')
    return em, None


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
# hand-written helpers take more than the four argument registers: these come from the machine's registers, which the
# calling code writes before every call by address
EXTRA_IN = {1, 2, 3, 8, 9, 10, 11, 12, 13, 14, 15, 24, 25}


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
EXTRA_ENTRIES = set()
# addresses the program was seen to call (computed calls) that sit inside another function's code, as the library's
# multi-entry routines do: they become entries of their own although the code before them falls into them
FORCE_ENTRIES = set()
_force = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'force_entries.txt')
if os.path.exists(_force):
    for _l in open(_force):
        _l = _l.split('#')[0].strip()
        if _l:
            FORCE_ENTRIES.add(int(_l, 16))
_extra = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'extra_entries.txt')
if os.path.exists(_extra):
    for _l in open(_extra):
        _l = _l.strip()
        if _l and not _l.startswith('#'):
            EXTRA_ENTRIES.add(int(_l, 16))


FUNC_NAMES = {}
if os.path.exists(_names):
    FUNC_NAMES = {int(k, 16): v for k, v in json.load(open(_names)).get('functions', {}).items()}


def func_name(a):
    return FUNC_NAMES.get(a, 'f_%08X' % a)


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
        elif r in (2, 3) or r in EXTRA_IN: head.append('    let mut r%d = g.r[%d];' % (r, r))
        else: head.append('    let mut r%d = 0u32;' % r)
    if 29 not in regs: head.append('    let mut r29 = g.sp;')
    head.append('    let mut hi = *g.hi; let mut lo = *g.lo;' if em.hilo else '    let mut hi = 0u32; let mut lo = 0u32;')
    tail = ['    unreachable!()', '}']
    return NL.join(head + em.lines + tail) + NL, entry_live, bad


def generate(image_index, main_ok=None, main_module='main_exe', is_main=False, dynamic=False):
    rows = json.load(open(os.path.join(HERE, '..', 'overlays.json')))
    row = rows[image_index]
    base, words = load_image(row)
    end = base + 4 * len(words)
    W = lambda a: words[(a - base) >> 2]
    entries = referenced_entries(base, words)
    _ne = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'not_entries.txt')
    if os.path.exists(_ne):
        entries = set(entries) - {int(l.split('#')[0].strip(), 16) for l in open(_ne) if l.split('#')[0].strip()}
    # entries seen when the program ran (a call through a pointer to code nothing else points at, tools/decomp/
    # extra_entries.txt) count when they are not inside a function that is already followed: a resumed loop is not an entry
    cand = sorted(a for a in EXTRA_ENTRIES if base <= a < end and not a & 3 and a not in entries)
    if cand:
        el0 = sorted(entries)
        nx0 = {a: (el0[k + 1] if k + 1 < len(el0) else end) for k, a in enumerate(el0)}
        covered = set()
        for e in el0:
            try:
                f0 = build_cfg(e, W, base, end, entries, nx0[e])
            except Exception:
                continue
            for b0 in f0.blocks.values():
                for (ia, _i) in b0.insns:
                    covered.add(ia)
                if getattr(b0, 'term_at', None) is not None:
                    covered.add(b0.term_at + 4)
        for a in cand:
            k = (a - base) >> 2
            # a function starts where the one before it ended (a return or a jump with its delay slot before it)
            w2 = words[k - 2] if k >= 2 else 0x03E00008
            starts = w2 == 0x03E00008 or (w2 >> 26) == 2 or ((w2 >> 26) == 0 and (w2 & 0x3F) == 8)
            if (a not in covered and starts) or a in FORCE_ENTRIES:
                entries = set(entries) | {a}
    elist = sorted(entries)
    nxt = {a: (elist[k + 1] if k + 1 < len(elist) else end) for k, a in enumerate(elist)}
    host = host_addresses()
    elist = [e for e in elist if e not in host]
    _nv = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'never_decompile.txt')
    if os.path.exists(_nv):
        never = {int(l.split('#')[0].strip(), 16) for l in open(_nv) if l.split('#')[0].strip()}
        elist = [e for e in elist if e not in never]
    ok = set()
    fail = {}
    def dyn(t, ret, args):
        nm = FUNC_NAMES.get(t)
        return 'g.call_addr(0x%08X%s, %s, %s)' % (t, (' /* %s */' % nm) if nm else '', '0x%08X' % ret if ret is not None else 'r31', args)
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
        if live - ALLOWED_IN - EXTRA_IN:
            fail[e] = 'reads %s it was not given' % sorted(live - ALLOWED_IN - EXTRA_IN)
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
             'use crate::G;', 'use crate::globals::*;', 'use crate::layout::*;', '']
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
    lines = ['// Generated by tools/decomp/decomp.py: the modules and their function tables.', '', 'pub mod globals;', 'pub mod layout;', '']
    for n in names:
        lines.append('pub mod %s;' % n)
    lines.append('')
    lines.append("/// every module's table of (address, first words, function), in the order the machine hooks them")
    lines.append('pub static TABLES: &[&[(u32, [u32; 6], crate::DecFn)]] = &[')
    for n in names:
        lines.append('    %s::TABLE,' % n)
    lines.append('];')
    open(os.path.join(out_dir, 'modules.rs'), 'w', encoding='utf-8', newline=NL).write(NL.join(lines) + NL)
    gl = ['// Generated by tools/decomp/decomp.py: every data address the program code names directly (a, b = loads, stores).',
          '#![allow(dead_code)]', '']
    for a in sorted(fold.GLOBALS):
        kinds = sorted(k for k in fold.GLOBALS[a] if k)
        gl.append('pub const %s: u32 = 0x%08X; // %s' % (fold.global_name(a), fold.rebase(a), ' '.join(kinds)))
    open(os.path.join(out_dir, 'globals.rs'), 'w', encoding='utf-8', newline=NL).write(NL.join(gl) + NL)
    print('named data addresses', len(fold.GLOBALS))
    ly = ['// Generated by tools/decomp/decomp.py from tools/decomp/structs.json: the offsets of the fields the code reaches through',
          '// a pointer whose structure is known (a, b = loads, stores are in the code; the names are the projects own).',
          '#![allow(dead_code, non_snake_case, non_upper_case_globals)]', 'use crate::G;', '']
    for st in sorted(fold.STRUCTS):
        offs = set(fold.FIELDS_USED.get(st, set())) | set(fold.STRUCTS[st]['fields'])
        ly.append('pub mod %s {' % st)
        ly.append('    use crate::G;')
        for o in sorted(offs):
            ly.append('    pub const %s: u32 = 0x%X;' % (fold.field_name(st, o), o))
        for (k, nm) in sorted(fold.ACCESS.get(st, set()), key=lambda x: (x[1], x[0])):
            if k.startswith('l'):
                ly.append('    #[inline(always)] pub unsafe fn %s_%s(g: &mut G, p: u32) -> u32 { g.%s(p.wrapping_add(%s)) }' % (k, nm, k, nm))
            else:
                ly.append('    #[inline(always)] pub unsafe fn %s_%s(g: &mut G, p: u32, v: u32) { g.%s(p.wrapping_add(%s), v) }' % (k, nm, k, nm))
        ly.append('}')
        ly.append('')
    open(os.path.join(out_dir, 'layout.rs'), 'w', encoding='utf-8', newline=NL).write(NL.join(ly) + NL)
    print('total decompiled', total)


if __name__ == '__main__' and len(sys.argv) > 1 and sys.argv[1] == 'writeall':
    if 'trace' in sys.argv[3:]:
        TRACE = True
    write_all(sys.argv[2], dynamic=('dynamic' in sys.argv[3:]))
