"""Expression folding for the decompiler's output.

A basic block's instructions are lowered to small operations; a register that is set and used once inside the block is
written into the expression that uses it, and a constant (a `lui` and its offsets) is folded into the address or the value
it ends up in. A value stays in its variable only when something later needs it there: it is read more than once, it is
live when the block is left, or the code around a call reads the variable.

The two passes the emitter runs over each block (the first counts the uses of every value, the second writes the code) both
go through the Scope class below.
"""
import re
from discover import simm

M = 0xFFFFFFFF
LOAD = {32: 'lb', 33: 'lh', 35: 'lw', 36: 'lbu', 37: 'lhu'}
STORE = {40: 'sb', 41: 'sh', 43: 'sw'}


class Unsupported(Exception):
    pass


def fields(i):
    return (i >> 26) & 63, (i >> 21) & 31, (i >> 16) & 31, (i >> 11) & 31, (i >> 6) & 31, i & 63


def s32(v):
    v &= M
    return v - (1 << 32) if v & 0x80000000 else v


def is_atom(t):
    if t.startswith('!'):
        return False
    depth = 0
    for ch in t:
        if ch in '([':
            depth += 1
        elif ch in ')]':
            depth -= 1
        elif ch == ' ' and depth == 0:
            return False
    return True


LIT = re.compile(r'^(0x[0-9A-Fa-f]+|\d+)$')


def lit(v):
    v &= M
    return '%d' % v if v < 256 else '0x%X' % v


def recv(t):
    """text usable before a method call or `as`"""
    if LIT.match(t):
        return t + 'u32'
    return t if is_atom(t) else '(' + t + ')'


def opnd(t):
    return t if is_atom(t) else '(' + t + ')'


class Op:
    __slots__ = ('kind', 'dst', 'srcs', 'build', 'fold', 'tag', 'imm')

    def __init__(self, kind, dst, srcs, build, fold=None, tag=None, imm=0):
        self.tag = tag        # 'add2' / 'or2' / 'addi' / 'ori' / 'lw': what the type of the result follows from
        self.imm = imm
        self.kind = kind      # 'pure' (value of dst from srcs), 'load' (the same, but it reads state that stores change),
        self.dst = dst        # 'store' / 'raw' (a statement; nothing after it may move before it)
        self.srcs = srcs
        self.build = build    # (texts, consts) -> text
        self.fold = fold      # consts -> value, when every source is a constant


# every constant data address the code loads from or stores to (address -> the access kinds seen); written out as named
# constants by decomp.py (globals.rs). NAMES maps an address to a name when one is known.
GLOBALS = {}
NAMES = {}
NAME_RANGE = (0x80010000, 0x80200000)
_access = ['']


def global_name(addr):
    if addr in NAMES:
        return NAMES[addr]
    return 'D_%08X' % addr


# structures: STRUCTS[name] = {'fields': {offset: name}, 'ptrs': {offset: struct}}; TYPED[address] = ('a'|'t', struct) for
# the globals that hold a pointer to one (an array of records or a single one); FIELDS_USED collects the offsets named.
STRUCTS = {}
TYPED = {}
FIELDS_USED = {}
RELOCATE = {}      # address of a global -> the value the program is told it holds when it stores a pointer there
_ty = [None]
_aff = [None]
_field = [None]
REBASE = None      # (lo, hi inclusive, delta): addresses the program is told differently (the state block moved out of the image)


def rebase(v):
    if REBASE is not None and REBASE[0] <= v <= REBASE[1]:
        return (v + REBASE[2]) & M
    return v


def unrebase(v):
    if REBASE is not None and REBASE[0] + REBASE[2] <= v <= REBASE[1] + REBASE[2]:
        return (v - REBASE[2]) & M
    return v
ACCESS = {}


def field_name(st, off):
    d = STRUCTS.get(st, {})
    n = d.get('fields', {}).get(off)
    return n if n else 'F_%02X' % off


def ea_text(v, c, imm):
    imm &= M
    _field[0] = None
    if c is not None:
        addr = rebase((c + imm) & M)
        orig = unrebase(addr)
        if NAME_RANGE[0] <= orig < NAME_RANGE[1]:
            GLOBALS.setdefault(orig, set()).add(_access[0])
            return global_name(orig)
        return lit(addr)
    if imm == 0:
        return v
    k = _aff[0]
    if k is not None and REBASE is not None and REBASE[0] <= (k + imm) & M <= REBASE[1]:
        # a constant part (a `lui` and the offset in the access) that lands in the moved block, with an index added: the
        # offset carries the move
        imm = (imm + REBASE[2]) & M
    if imm >= 0x80000000:
        return '%s.wrapping_sub(%s)' % (recv(v), lit((-imm) & M))
    ty = _ty[0]
    if ty is not None and ty[0] in ('t', 'a') and ty[1] in STRUCTS and imm < 0x1000:
        FIELDS_USED.setdefault(ty[1], set()).add(imm)
        _field[0] = (ty[1], field_name(ty[1], imm))
        return '%s.wrapping_add(%s::%s)' % (recv(v), ty[1], field_name(ty[1], imm))
    return '%s.wrapping_add(%s)' % (recv(v), lit(imm))


def lower(a, i):
    """The operation of one non-control instruction, or None for a nop / a write to $zero that does nothing."""
    op, rs, rt, rd, sh, fn = fields(i)
    if i == 0:
        return None

    def pure(dst, srcs, build, fold=None):
        if dst == 0:
            return None
        return Op('pure', dst, srcs, build, fold)

    if op == 0:
        if fn == 0:
            return pure(rd, [rt], lambda v, c: '%s << %d' % (recv(v[0]), sh), lambda c: (c[0] << sh) & M)
        if fn == 2:
            return pure(rd, [rt], lambda v, c: '%s >> %d' % (recv(v[0]), sh), lambda c: c[0] >> sh)
        if fn == 3:
            return pure(rd, [rt], lambda v, c: '((%s as i32) >> %d) as u32' % (recv(v[0]), sh), lambda c: (s32(c[0]) >> sh) & M)
        if fn == 4:
            return pure(rd, [rt, rs], lambda v, c: '%s << (%s & 31)' % (recv(v[0]), opnd(v[1])), lambda c: (c[0] << (c[1] & 31)) & M)
        if fn == 6:
            return pure(rd, [rt, rs], lambda v, c: '%s >> (%s & 31)' % (recv(v[0]), opnd(v[1])), lambda c: c[0] >> (c[1] & 31))
        if fn == 7:
            return pure(rd, [rt, rs], lambda v, c: '((%s as i32) >> (%s & 31)) as u32' % (recv(v[0]), opnd(v[1])), lambda c: (s32(c[0]) >> (c[1] & 31)) & M)
        if fn == 13:
            return Op('raw', None, [], lambda v, c: 'g.brk(0x%X);' % ((i >> 6) & 0xFFFFF))
        if fn == 16:
            return None if rd == 0 else Op('load', rd, [], lambda v, c: 'hi')
        if fn == 18:
            return None if rd == 0 else Op('load', rd, [], lambda v, c: 'lo')
        if fn == 17:
            return Op('raw', None, [rs], lambda v, c: 'hi = %s;' % v[0])
        if fn == 19:
            return Op('raw', None, [rs], lambda v, c: 'lo = %s;' % v[0])
        if fn == 24:
            return Op('raw', None, [rs, rt], lambda v, c: '{ let p = (%s as i32 as i64) * (%s as i32 as i64); hi = (p >> 32) as u32; lo = p as u32; }' % (recv(v[0]), recv(v[1])))
        if fn == 25:
            return Op('raw', None, [rs, rt], lambda v, c: '{ let p = (%s as u64) * (%s as u64); hi = (p >> 32) as u32; lo = p as u32; }' % (recv(v[0]), recv(v[1])))
        if fn == 26:
            return Op('raw', None, [rs, rt], lambda v, c: ('{ let (n, d) = (%s as i32, %s as i32); if d == 0 { lo = if n < 0 { 1 } else { 0xFFFF_FFFF }; hi = n as u32; } '
                      'else if n == i32::MIN && d == -1 { lo = 0x8000_0000; hi = 0; } else { lo = (n / d) as u32; hi = (n %% d) as u32; } }') % (recv(v[0]), recv(v[1])))
        if fn == 27:
            return Op('raw', None, [rs, rt], lambda v, c: ('{ let (n, d) = (%s, %s); if d == 0 { lo = 0xFFFF_FFFF; hi = n; } else { lo = n / d; hi = n %% d; } }') % (v[0], v[1]))
        if fn in (32, 33):
            def b(v, c):
                if c[1] == 0: return v[0]
                if c[0] == 0: return v[1]
                return '%s.wrapping_add(%s)' % (recv(v[0]), v[1])
            o = pure(rd, [rs, rt], b, lambda c: (c[0] + c[1]) & M)
            if o: o.tag = 'add2'
            return o
        if fn in (34, 35):
            def b(v, c):
                if c[1] == 0: return v[0]
                return '%s.wrapping_sub(%s)' % (recv(v[0]), v[1])
            return pure(rd, [rs, rt], b, lambda c: (c[0] - c[1]) & M)
        if fn == 36:
            return pure(rd, [rs, rt], lambda v, c: '%s & %s' % (opnd(v[0]), opnd(v[1])), lambda c: c[0] & c[1])
        if fn == 37:
            def b(v, c):
                if c[1] == 0: return v[0]
                if c[0] == 0: return v[1]
                return '%s | %s' % (opnd(v[0]), opnd(v[1]))
            o = pure(rd, [rs, rt], b, lambda c: c[0] | c[1])
            if o: o.tag = 'or2'
            return o
        if fn == 38:
            def b(v, c):
                if c[1] == 0: return v[0]
                if c[0] == 0: return v[1]
                return '%s ^ %s' % (opnd(v[0]), opnd(v[1]))
            return pure(rd, [rs, rt], b, lambda c: c[0] ^ c[1])
        if fn == 39:
            return pure(rd, [rs, rt], lambda v, c: '!(%s | %s)' % (opnd(v[0]), opnd(v[1])), lambda c: ~(c[0] | c[1]) & M)
        if fn == 42:
            return pure(rd, [rs, rt], lambda v, c: '((%s as i32) < (%s as i32)) as u32' % (recv(v[0]), recv(v[1])), lambda c: 1 if s32(c[0]) < s32(c[1]) else 0)
        if fn == 43:
            return pure(rd, [rs, rt], lambda v, c: '(%s < %s) as u32' % (opnd(v[0]), opnd(v[1])), lambda c: 1 if c[0] < c[1] else 0)
        raise Unsupported('special %d' % fn)
    imm = simm(i) & M
    if op in (8, 9):
        def b(v, c):
            if imm == 0: return v[0]
            if imm >= 0x80000000: return '%s.wrapping_sub(%s)' % (recv(v[0]), lit((-imm) & M))
            return '%s.wrapping_add(%s)' % (recv(v[0]), lit(imm))
        o = pure(rt, [rs], b, lambda c: (c[0] + imm) & M)
        if o: o.tag, o.imm = 'addi', imm
        return o
    if op == 10:
        return pure(rt, [rs], lambda v, c: '((%s as i32) < %d) as u32' % (recv(v[0]), simm(i)), lambda c: 1 if s32(c[0]) < simm(i) else 0)
    if op == 11:
        return pure(rt, [rs], lambda v, c: '(%s < %s) as u32' % (opnd(v[0]), lit(imm)), lambda c: 1 if c[0] < imm else 0)
    if op == 12:
        return pure(rt, [rs], lambda v, c: '%s & %s' % (opnd(v[0]), lit(i & 0xFFFF)), lambda c: c[0] & (i & 0xFFFF))
    if op == 13:
        def b(v, c):
            if (i & 0xFFFF) == 0: return v[0]
            return '%s | %s' % (opnd(v[0]), lit(i & 0xFFFF))
        o = pure(rt, [rs], b, lambda c: c[0] | (i & 0xFFFF))
        if o: o.tag, o.imm = 'ori', i & 0xFFFF
        return o
    if op == 14:
        def b(v, c):
            if (i & 0xFFFF) == 0: return v[0]
            return '%s ^ %s' % (opnd(v[0]), lit(i & 0xFFFF))
        return pure(rt, [rs], b, lambda c: c[0] ^ (i & 0xFFFF))
    if op == 15:
        return pure(rt, [], lambda v, c: lit((i & 0xFFFF) << 16), lambda c: (i & 0xFFFF) << 16)
    if op in LOAD:
        if rt == 0:
            return None
        def b(v, c, k=LOAD[op]):
            _access[0] = k
            t = ea_text(v[0], c[0], imm)
            if _field[0] is not None:
                st, nm = _field[0]
                ACCESS.setdefault(st, set()).add((k, nm))
                return '%s::%s_%s(g, %s)' % (st, k, nm, v[0])
            return 'g.%s(%s)' % (k, t)
        o = Op('load', rt, [rs], b)
        if op == 35: o.tag, o.imm = 'lw', imm
        return o
    if op == 34:
        return None if rt == 0 else Op('load', rt, [rs, rt], lambda v, c: 'g.lwl(%s, %s)' % (ea_text(v[0], c[0], imm), v[1]))
    if op == 38:
        return None if rt == 0 else Op('load', rt, [rs, rt], lambda v, c: 'g.lwr(%s, %s)' % (ea_text(v[0], c[0], imm), v[1]))
    if op in STORE:
        def b(v, c, k=STORE[op]):
            _access[0] = k
            t = ea_text(v[0], c[0], imm)
            if _field[0] is not None:
                st, nm = _field[0]
                ACCESS.setdefault(st, set()).add((k, nm))
                return '%s::%s_%s(g, %s, %s);' % (st, k, nm, v[0], v[1])
            return 'g.%s(%s, %s);' % (k, t, v[1])
        o = Op('store', None, [rs, rt], b)
        if op == 43: o.tag, o.imm = 'sw', imm
        return o
    if op == 42:
        return Op('store', None, [rs, rt], lambda v, c: 'g.swl(%s, %s);' % (ea_text(v[0], c[0], imm), v[1]))
    if op == 46:
        return Op('store', None, [rs, rt], lambda v, c: 'g.swr(%s, %s);' % (ea_text(v[0], c[0], imm), v[1]))
    if op == 18:
        if rs == 0:
            return None if rt == 0 else Op('load', rt, [], lambda v, c: 'g.gte_data(%d)' % rd)
        if rs == 2:
            return None if rt == 0 else Op('load', rt, [], lambda v, c: 'g.gte_ctrl(%d)' % rd)
        if rs == 4:
            return Op('raw', None, [rt], lambda v, c: 'g.gte_set_data(%d, %s);' % (rd, v[0]))
        if rs == 6:
            return Op('raw', None, [rt], lambda v, c: 'g.gte_set_ctrl(%d, %s);' % (rd, v[0]))
        if rs & 0x10:
            return Op('raw', None, [], lambda v, c: 'g.gte_cmd(0x%X);' % (i & 0x1FFFFFF))
    if op == 16:
        # system coprocessor: read as 0, writes ignored (the machine does not model the status register)
        if rs in (0, 2):
            return None if rt == 0 else pure(rt, [], lambda v, c: '0', lambda c: 0)
        return None
    if op == 50:
        return Op('raw', None, [rs], lambda v, c: '{ let v = g.lw(%s); g.gte_set_data(%d, v); }' % (ea_text(v[0], c[0], imm), rt))
    if op == 58:
        return Op('raw', None, [rs], lambda v, c: 'g.sw(%s, g.gte_data(%d));' % (ea_text(v[0], c[0], imm), rt))
    raise Unsupported('op %d' % op)


class Pend:
    __slots__ = ('text', 'deps', 'impure', 'const')

    def __init__(self, text, deps, impure, const):
        self.text = text
        self.deps = deps
        self.impure = impure
        self.const = const


class Scope:
    """The values of one block's registers while its code is written."""

    def __init__(self, em, counting, uses, entry=None):
        self.em = em
        self.counting = counting
        self.uses = uses              # the use counts of every value of the block (second pass)
        self.cnt = []                 # ... and the first pass's count of them
        self.pend = {}                # register -> Pend: the value is not in the variable yet
        self.known = {}               # register -> constant value (in the variable or pending)
        self.defid = {}               # register -> id of its current value
        if entry:
            for r, v in entry.items():
                if v[0] == 'c':
                    self.known[r] = v[1]
        self.entry_ty = {r: v for r, v in (entry or {}).items() if v[0] in ('t', 'a')}
        self.entry_aff = {r: v[1] for r, v in (entry or {}).items() if v[0] == 'k'}
        self.busy = set()             # registers being written while their dependents are
        self.aff = dict(self.entry_aff)   # register -> K: the value is K plus something not known
        self.ty = dict(self.entry_ty)  # register -> ('t'|'a', struct): what the pointer in it points at

    # ---- reading ----
    def use(self, r):
        if self.counting:
            d = self.defid.get(r)
            if d is not None:
                self.cnt[d] += 1

    def val(self, r):
        """(text, constant or None, variables the text reads, reads state a store may change)"""
        if r == 0:
            return ('0', 0, frozenset(), False)
        p = self.pend.get(r)
        if p is not None:
            self.use(r)
            if p.const is None:
                # a value with one reader: it goes into the reader's text and is gone
                del self.pend[r]
            return (p.text, p.const, p.deps, p.impure)
        k = self.known.get(r)
        if k is not None:
            return (lit(k), k, frozenset(), False)
        self.use(r)
        return ('r%d' % r, None, frozenset([r]), False)

    def val_nog(self, r):
        """Like val, but a value whose text calls the machine goes into its variable: the machine cannot be used twice at once
        in one expression."""
        p = self.pend.get(r)
        if p is not None and 'g.' in p.text:
            self.use(r)
            self.materialize(r)
            return ('r%d' % r, None, frozenset([r]), False)
        return self.val(r)

    def var(self, r):
        """The name of the variable, with the value in it."""
        if r == 0:
            return '0u32'
        if r in self.pend:
            self.use(r)
            self.materialize(r)
        else:
            self.use(r)
        return 'r%d' % r

    # ---- writing ----
    def materialize(self, r):
        p = self.pend.get(r)
        if p is None:
            return
        self.busy.add(r)
        for q in list(self.pend):
            if q != r and q in self.pend and q not in self.busy and r in self.pend[q].deps:
                self.materialize(q)
        self.busy.discard(r)
        p = self.pend.pop(r)
        # values that wait for this one to be written and also need its old content (a swap): the old content is kept
        waiting = [q for q in self.busy if q in self.pend and r in self.pend[q].deps]
        if waiting and p.text != 'r%d' % r:
            self.em.ntmp = getattr(self.em, 'ntmp', 0) + 1
            name = 't%d' % self.em.ntmp
            self.em.w('let %s = r%d;' % (name, r))
            for q in waiting:
                pq = self.pend[q]
                pq.text = re.sub(r'r%d' % r, name, pq.text)
                pq.deps = frozenset(pq.deps - {r})
        if p.text != 'r%d' % r:
            self.em.w('r%d = %s;' % (r, p.text))

    def flush_deps(self, r):
        for q in list(self.pend):
            if q != r and q in self.pend and r in self.pend[q].deps:
                self.materialize(q)

    def flush_impure(self):
        for q in list(self.pend):
            if q in self.pend and self.pend[q].impure:
                self.materialize(q)

    def clobber(self, r):
        """The variable was written by code that is not an instruction (a call's result, a reload)."""
        self.flush_deps(r)
        self.pend.pop(r, None)
        self.known.pop(r, None)
        self.defid.pop(r, None)
        self.ty.pop(r, None)
        self.aff.pop(r, None)

    def newdef(self, r):
        d = len(self.cnt) if self.counting else self.nd
        if self.counting:
            self.cnt.append(0)
        else:
            self.nd += 1
        self.defid[r] = d
        return d

    nd = 0

    def run(self, op):
        em = self.em
        if op.tag == 'sw':
            kb = self.known.get(op.srcs[0]) if op.srcs[0] else 0
            if kb is None and op.srcs[0] in self.pend:
                kb = self.pend[op.srcs[0]].const
            if kb is not None and ((kb + op.imm) & M) in RELOCATE:
                # the pointer the program stores here is replaced: the structure lives somewhere else (the switch is the
                # machine's); the register that held the allocation holds the new address from here on, so the code that
                # clears the table goes there too
                rt = op.srcs[1]
                v = self.var(rt)
                self.em.w('%s = g.reloc(0, %s);' % (v, v))
                self.clobber(rt)
        if op.kind != 'pure':
            # a statement that calls the machine takes plain values: whatever still waits to be written goes first (and with
            # it nothing is written between taking the operands and using them)
            self.flush_impure()
        vals = [self.val(r) for r in op.srcs]
        _ty[0] = self.ty.get(op.srcs[0]) if op.kind in ('load', 'store', 'raw') and op.srcs else None
        _aff[0] = self.aff.get(op.srcs[0]) if op.kind in ('load', 'store', 'raw') and op.srcs else None
        texts = [v[0] for v in vals]
        consts = [v[1] for v in vals]
        if op.kind in ('store', 'raw'):
            line = op.build(texts, consts)
            # the values the statement reads are evaluated before it runs; what else is pending may not move behind it
            em.w(line)
            if op.dst is not None:
                self.clobber(op.dst)
            return
        dst = op.dst
        # what the value points at
        ty = None
        tg = op.tag
        if tg in ('add2', 'or2'):
            ra, rb = op.srcs
            ca, cb = consts
            if cb == 0 and ra != 0:
                ty = self.ty.get(ra)
            elif ca == 0 and rb != 0:
                ty = self.ty.get(rb)
            elif tg == 'add2':
                ta, tb = self.ty.get(ra), self.ty.get(rb)
                if ta and ta[0] == 'a' and cb is None and not tb:
                    ty = ('t', ta[1])
                elif tb and tb[0] == 'a' and ca is None and not ta:
                    ty = ('t', tb[1])
        elif tg in ('addi', 'ori'):
            if op.imm == 0 and op.srcs[0] != 0:
                ty = self.ty.get(op.srcs[0])
        elif tg == 'lw':
            if consts[0] is not None:
                ty = TYPED.get((consts[0] + op.imm) & M)
            else:
                bt = self.ty.get(op.srcs[0])
                if bt:
                    ty = STRUCTS.get(bt[1], {}).get('ptrs', {}).get(op.imm & M)
                    ty = ('t', ty) if ty else None
        if ty is None:
            self.ty.pop(dst, None)
        else:
            self.ty[dst] = ty
        # constant part of a value that is not constant (base + index)
        ak = None
        if tg == 'add2':
            ra, rb = op.srcs
            ca, cb = consts
            if cb is not None and ca is None:
                ak = (cb + self.aff.get(ra, 0)) & M
            elif ca is not None and cb is None:
                ak = (ca + self.aff.get(rb, 0)) & M
            elif ca is None and cb is None:
                ka, kb = self.aff.get(ra), self.aff.get(rb)
                if ka is not None or kb is not None:
                    ak = ((ka or 0) + (kb or 0)) & M
        elif tg == 'addi' and consts[0] is None:
            ak = (self.aff.get(op.srcs[0], 0) + op.imm) & M
        if ak is None:
            self.aff.pop(dst, None)
        else:
            self.aff[dst] = ak
        # the value of dst
        if op.kind == 'pure' and op.fold is not None and all(c is not None for c in consts):
            value = op.fold(consts) & M
            text, deps, impure, const = lit(value), frozenset(), False, value
        else:
            text = op.build(texts, consts)
            deps = frozenset().union(*[v[2] for v in vals]) if vals else frozenset()
            impure = op.kind == 'load' or any(v[3] for v in vals)
            const = None
        did = self.newdef(dst)
        self.pend.pop(dst, None)
        self.known.pop(dst, None)
        if const is not None:
            self.known[dst] = const
            self.pend[dst] = Pend(text, deps, False, const)
            return
        n = self.uses[did] if not self.counting else 99
        if not self.counting and n == 0:
            # nothing reads it (and it is not live when the block is left): a read of memory still runs
            if op.kind == 'load' and not (text.startswith('hi') or text.startswith('lo') or text.startswith('g.gte')):
                em.w('let _ = %s;' % text)
            return
        if not self.counting and n == 1:
            self.pend[dst] = Pend(text, deps, impure, None)
            return
        # more than one reader, or the first pass: the variable gets it now
        self.flush_deps(dst)
        if op.kind == 'load' or impure:
            pass
        if text != 'r%d' % dst:
            em.w('r%d = %s;' % (dst, text))

    def finish(self, live):
        """The block is left: what is pending and live afterwards goes into its variable."""
        if self.counting:
            for r, d in self.defid.items():
                if r in live:
                    self.cnt[d] += 1
            return
        for r in sorted(self.pend):
            if r in self.pend and r in live:
                self.materialize(r)
        self.pend.clear()


def fold_liveness(f, W, succ, uses_defs, uses_defs_cond, CALLEE_SAVED, CLOBBERED, extra_out=frozenset()):
    """Registers live at the start of every block, counting what the code around calls and returns reads."""
    use = {}
    defs = {}
    ALL = set(range(1, 32))
    for a, b in f.blocks.items():
        u, d = set(), set()
        ta = getattr(b, 'term_at', None)
        for (ia, i) in b.insns:
            if ta == ia:
                continue
            uu, dd = uses_defs(i)
            u |= (uu - d)
            d |= dd
        t = b.term
        k = t[0]
        if ta is not None:
            i = W(ta)
            ru = set()
            if k == 'cond':
                ru = uses_defs_cond(i)
            elif k in ('jr', 'callind', 'switch'):
                ru = {t[1]}
            u |= (ru - d)
            uu, dd = uses_defs(W(ta + 4))
            u |= (uu - d)
            d |= dd
        if k in ('call', 'callind'):
            u |= (ALL - d)
            d |= CLOBBERED | {2, 3}
        elif k == 'ret':
            u |= (({2, 3, 29, 31} | CALLEE_SAVED | set(extra_out)) - d)
        elif k in ('tail', 'jr'):
            u |= (({2, 3, 4, 5, 6, 7, 29, 31} | CALLEE_SAVED | set(extra_out)) - d)
        use[a] = u
        defs[a] = d
    live_in = {a: set() for a in f.blocks}
    changed = True
    while changed:
        changed = False
        for a in f.blocks:
            out = set()
            for s in succ[a]:
                out |= live_in.get(s, set())
            nin = use[a] | (out - defs[a])
            if nin != live_in[a]:
                live_in[a] = nin
                changed = True
    return live_in


BOTTOM_ENTRY_OFF = False
EXTRA_IN_REGS = set()


def infer_types(f, W, succ, uses_defs, CLOBBERED):
    """Forward dataflow over the registers of a function: the constants in them and the structures they point at. Returns
    {block: {register: ('c', value) | ('t', struct) | ('a', struct)}} at the start of every block."""
    def ev(st, i):
        op, rs, rt, rd, sh, fn = fields(i)
        g = lambda r: (('c', 0) if r == 0 else (None if st.get(r, ('', 0))[0] == 'u' else st.get(r)))
        res = None
        dst = None
        if i == 0:
            return
        if op == 15:
            dst, res = rt, ('c', (i & 0xFFFF) << 16)
        elif op in (8, 9):
            dst = rt
            a = g(rs)
            imm = simm(i) & M
            if a and a[0] == 'c': res = ('c', (a[1] + imm) & M)
            elif imm == 0: res = a
            elif a and a[0] == 'k': res = ('k', (a[1] + imm) & M)
            else: res = ('k', imm)
        elif op == 13:
            dst = rt
            a = g(rs)
            if a and a[0] == 'c': res = ('c', a[1] | (i & 0xFFFF))
            elif (i & 0xFFFF) == 0: res = a
        elif op == 0 and fn in (32, 33, 37):
            dst = rd
            a, b = g(rs), g(rt)
            if a and b and a[0] == 'c' and b[0] == 'c':
                res = ('c', ((a[1] + b[1]) if fn != 37 else (a[1] | b[1])) & M)
            elif b and b[0] == 'c' and b[1] == 0: res = a
            elif a and a[0] == 'c' and a[1] == 0: res = b
            elif fn != 37 and a and a[0] == 'c' and not (b and b[0] == 'c'):
                res = ('k', (a[1] + (b[1] if b and b[0] == 'k' else 0)) & M)
            elif fn != 37 and b and b[0] == 'c' and not (a and a[0] == 'c'):
                res = ('k', (b[1] + (a[1] if a and a[0] == 'k' else 0)) & M)
            elif fn != 37 and a and b and a[0] == 'k' and b[0] != 'c':
                res = a
            elif fn != 37 and a and b and b[0] == 'k' and a[0] != 'c':
                res = b
            elif fn != 37:
                if a and a[0] == 'a' and not (b and b[0] in ('c', 't', 'a')): res = ('t', a[1])
                elif b and b[0] == 'a' and not (a and a[0] in ('c', 't', 'a')): res = ('t', b[1])
        elif op == 35:
            dst = rt
            a = g(rs)
            imm = simm(i) & M
            if a and a[0] == 'c':
                res = TYPED.get((a[1] + imm) & M)
            elif a and a[0] in ('t', 'a'):
                p = STRUCTS.get(a[1], {}).get('ptrs', {}).get(imm)
                if p: res = ('t', p)
        else:
            try:
                _u, d = uses_defs(i)
            except Exception:
                return
            for r in d:
                st.pop(r, None)
            return
        if dst:
            if res is None: st.pop(dst, None)
            else: st[dst] = res

    # the callee-saved registers hold the caller's values at entry and the code only saves them before it writes them: a path
    # that reaches a use with the entry value is not one the compiled code takes, so the entry value joins as 'nothing yet'
    # (('u', 0)) and a path that did assign the register decides its type. Helpers that take their inputs in those registers
    # (EXTRA_IN) keep the plain rule.
    entry_in = {}
    if not BOTTOM_ENTRY_OFF:
        for r in (16, 17, 18, 19, 20, 21, 22, 23, 30):
            if r not in EXTRA_IN_REGS:
                entry_in[r] = ('u', 0)
    inn = {f.entry: entry_in}
    work = [f.entry]
    guard = 0
    while work and guard < 400000:
        guard += 1
        a = work.pop()
        b = f.blocks[a]
        st = dict(inn[a])
        ta = getattr(b, 'term_at', None)
        for (ia, i) in b.insns:
            if ta == ia:
                continue
            ev(st, i)
        if ta is not None:
            ev(st, W(ta + 4))
            if b.term[0] in ('call', 'callind'):
                # what a call leaves in the registers is known only as the code that returns says: no constants survive
                st = {r: v for r, v in st.items() if v[0] != 'c' and r not in CLOBBERED | {2, 3}}
        for sct in succ.get(a, []):
            if sct not in f.blocks:
                continue
            if sct not in inn:
                inn[sct] = dict(st)
                work.append(sct)
            else:
                cur = inn[sct]
                new = {}
                for r, v in cur.items():
                    o = st.get(r)
                    if o == v:
                        new[r] = v
                    elif v[0] == 'u' and o is not None and o[0] != 'c':
                        new[r] = o
                    elif o is not None and o[0] == 'u' and v[0] != 'c':
                        new[r] = v
                if new != cur:
                    inn[sct] = new
                    work.append(sct)
    return {b: {r: v for r, v in d.items() if v[0] != 'u'} for b, d in inn.items()}
