// The program's own code, translated ahead of time (tools/recomp.py -> native/gen.rs): each basic block is a Rust
// function that does what the interpreter would do for the same instructions, in the same order, with the same memory
// paths, and counts the same steps. The interpreter stays underneath for everything that is not translated (and for every
// address the host has a hook on), so the two always agree; XG_NATIVE=0 turns the translated blocks off.

use super::{Machine, H};
use crate::mem::RAM_WORDS;
use xgblocks::Ctx;

pub(super) use xgblocks::BlockFn;

pub(super) fn words() -> &'static [u32] { &xgblocks::WORDS }

// The blocks reach the machine through a Ctx: the scalars are copied across around every slow call, and exit says
// whether the block has to be left afterwards.
unsafe fn sync_in(m: &mut Machine, c: &Ctx) {
    m.pc = c.pc; m.npc = c.npc; m.cur_pc = c.cur_pc; m.hi = c.hi; m.lo = c.lo; m.critical = c.critical;
}

unsafe fn sync_out(m: &mut Machine, c: &mut Ctx) {
    c.pc = m.pc; c.npc = m.npc; c.cur_pc = m.cur_pc; c.hi = m.hi; c.lo = m.lo; c.critical = m.critical;
    c.exit = m.irq_pending || m.stop_reason.is_some();
}

unsafe fn c_read32(c: &mut Ctx, a: u32) -> u32 { let m = &mut *(c.m as *mut Machine); sync_in(m, c); let v = m.read32(a); sync_out(m, c); v }
unsafe fn c_read16(c: &mut Ctx, a: u32) -> u16 { let m = &mut *(c.m as *mut Machine); sync_in(m, c); let v = m.read16(a); sync_out(m, c); v }
unsafe fn c_read8(c: &mut Ctx, a: u32) -> u8 { let m = &mut *(c.m as *mut Machine); sync_in(m, c); let v = m.read8(a); sync_out(m, c); v }
unsafe fn c_write32(c: &mut Ctx, a: u32, v: u32) { let m = &mut *(c.m as *mut Machine); sync_in(m, c); m.write32(a, v); sync_out(m, c); }
unsafe fn c_write16(c: &mut Ctx, a: u32, v: u16) { let m = &mut *(c.m as *mut Machine); sync_in(m, c); m.write16(a, v); sync_out(m, c); }
unsafe fn c_write8(c: &mut Ctx, a: u32, v: u8) { let m = &mut *(c.m as *mut Machine); sync_in(m, c); m.write8(a, v); sync_out(m, c); }

pub(super) fn new_ctx() -> Box<Ctx> {
    Box::new(Ctx {
        r: std::ptr::null_mut(), w: std::ptr::null_mut(), flag: std::ptr::null(), gte: std::ptr::null_mut(),
        pc: 0, npc: 0, cur_pc: 0, hi: 0, lo: 0, critical: false, exit: false, m: std::ptr::null_mut(),
        f_read32: c_read32, f_read16: c_read16, f_read8: c_read8, f_write32: c_write32, f_write16: c_write16, f_write8: c_write8,
    })
}

impl Machine {
    /// Points the blocks' context at this machine (its memory does not move while it runs).
    pub(super) fn native_bind(&mut self) {
        let me = self as *mut Machine as *mut u8;
        self.ctx.r = self.r.as_mut_ptr();
        self.ctx.w = self.mem.w.as_mut_ptr();
        self.ctx.flag = self.mem.flag.as_ptr();
        self.ctx.gte = &mut self.gte;
        self.ctx.m = me;
    }
}

impl Machine {
    /// Chooses the translated blocks that may run: the ones with no host hook inside them. Blocks made from different words
    /// at the same address are chained (native_alt); which one runs is decided on entry, by comparing the words in memory.
    pub(super) fn native_init(&mut self) {
        self.native_dirty = false;
        self.native_idx.clear();
        self.native_idx.resize(RAM_WORDS, 0);
        self.native_fns.clear();
        self.native_len.clear();
        self.native_woff.clear();
        self.native_alt.clear();
        self.native_stamp.clear();
        self.native_okv.clear();
        for f in self.mem.flag.iter_mut() { *f = 0; }
        if self.mem.state_on { self.mem.state_flags(); }
        self.mem.gen = self.mem.gen.wrapping_add(1);
        if std::env::var("XG_NATIVE").map(|v| v == "0").unwrap_or(false) || self.tracing || self.xtrace_set.is_some() { self.native_on = false; return; }
        // XG_NATIVE_RANGE=lo:hi: only the blocks with an index in that range (to find a block that disagrees)
        let (lo, hi) = std::env::var("XG_NATIVE_RANGE").ok().and_then(|v| {
            let q: Vec<&str> = v.split(':').collect();
            if q.len() == 2 { Some((q[0].parse::<usize>().ok()?, q[1].parse::<usize>().ok()?)) } else { None }
        }).unwrap_or((0, usize::MAX));
        let mut used = 0usize;
        let mut dropped = 0usize;
        for (bi, &(addr, n, off, f)) in xgblocks::BLOCKS.iter().enumerate() {
            if bi < lo || bi >= hi { continue; }
            let first = ((addr & 0x1FFFFF) >> 2) as usize;
            let n = n as usize;
            if first + n > RAM_WORDS || (first..first + n).any(|at| self.slow[at] || self.hook[at] != H::None) { dropped += 1; continue; }
            self.native_fns.push(f);
            self.native_len.push(n as u32);
            self.native_woff.push(off);
            self.native_alt.push(0);
            self.native_stamp.push(0);
            self.native_okv.push(0);
            for at in first..first + n { self.mem.flag[at >> 6] |= 1u64 << (at & 63); }
            let k = self.native_fns.len() as u32;
            if self.native_idx[first] == 0 {
                self.native_idx[first] = k;
            } else {
                let mut c = self.native_idx[first];
                while self.native_alt[(c - 1) as usize] != 0 { c = self.native_alt[(c - 1) as usize]; }
                self.native_alt[(c - 1) as usize] = k;
            }
            used += n;
        }
        self.native_on = !self.native_fns.is_empty();
        if std::env::var("XG_NATIVE_INFO").is_ok() {
            eprintln!("native: {} blocks ({} instructions) selected, {} left out (host hooks inside)", self.native_fns.len(), used, dropped);
        }
    }

    /// Called once a frame: the blocks are chosen again only when the host's hooks changed.
    pub(super) fn native_check(&mut self) {
        if self.native_dirty { self.native_init(); }
    }

    /// A hash of the processor's registers and all of memory, for comparing runs.
    pub fn digest(&self) -> u64 {
        let mut h: u64 = 0xCBF29CE484222325;
        let mut mix = |v: u32| { h = (h ^ v as u64).wrapping_mul(0x100000001B3); };
        // XG_DIGEST_RAM: memory only (the decompiled code keeps its registers in locals)
        if std::env::var("XG_DIGEST_RAM").is_err() {
            for v in self.r { mix(v); }
            mix(self.pc); mix(self.npc); mix(self.hi); mix(self.lo);
        }
        // XG_DIGEST_SKIP=lo:hi (hex byte offsets into memory): left out (stack residue differs in harmless ways)
        let skip: Option<(usize, usize)> = std::env::var("XG_DIGEST_SKIP").ok().and_then(|v| {
            let q: Vec<&str> = v.split(':').collect();
            if q.len() == 2 { Some((usize::from_str_radix(q[0], 16).ok()? / 4, usize::from_str_radix(q[1], 16).ok()? / 4)) } else { None }
        });
        // XG_DIGEST_VRAM: the picture memory (what the program shows), not the console image
        if std::env::var("XG_DIGEST_VRAM").is_ok() {
            for &v in self.gpu.vram.iter() { mix(v as u32); }
            return h;
        }
        for (i, &v) in self.mem.w.iter().enumerate() {
            if let Some((lo, hi)) = skip { if i >= lo && i < hi { continue; } }
            mix(v);
        }
        for &v in self.mem.state.iter() { mix(v); }
        for &b in self.mem.script[..self.mem.script_len].iter() { mix(b as u32); }
        for e in self.mem.ents.iter() { for &v in e.w.iter() { mix(v); } }
        for o in self.mem.objs.iter().take(self.mem.obj_next) { for &v in o.w.iter() { mix(v); } }
        h
    }

    /// Marks a boundary in the trace (the host is about to put new data in memory, which may be code): the generator
    /// cuts blocks from the image as it stands at each mark.
    pub(super) fn trace_flush(&mut self) {
        if self.trace_events.len() > self.trace_marked {
            self.trace_events.push((b'E', 0, 0));
            self.trace_marked = self.trace_events.len();
        }
    }

    /// Writes the trace: the words as they ran (with the address), the addresses entered other than by running on, and
    /// the marks.
    pub fn write_trace(&mut self, path: &str) {
        if !self.tracing { return; }
        self.trace_flush();
        let mut s = String::with_capacity(self.trace_events.len() * 20);
        for &(k, a, w) in &self.trace_events {
            match k {
                b'W' => s.push_str(&format!("W {:08X} {:08X}\n", a, w)),
                b'N' => s.push_str(&format!("N {:08X}\n", a)),
                _ => s.push_str("E\n"),
            }
        }
        let _ = std::fs::write(path, s);
    }
}
