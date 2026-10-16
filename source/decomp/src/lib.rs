// The game's code as Rust source: functions decompiled from the program (tools/decomp/decomp.py) with their registers as
// locals and structured control flow. They run on G: the machine's memory, a few registers that are shared across calls
// (the callee-saved ones and the stack pointer) and a handful of operations for what is not plain memory.

use xgblocks::gte::Gte;

include!("modules.rs");

/// functions at table positions outside [lo, hi) are left to the translated code (for bisecting a fault)
pub static RANGE: [std::sync::atomic::AtomicUsize; 2] = [std::sync::atomic::AtomicUsize::new(0), std::sync::atomic::AtomicUsize::new(usize::MAX)];

/// Whether the program is told the relocated structures' addresses (XG_ENT=0 keeps them in the console image, for comparing).
pub static RELOC_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// XG_ENT_WATCH: report a call into the allocator's address range (0x80031000-0x80034000) that is given a relocated pointer
pub static WATCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// addresses of functions left to the translated code (XG_DEC_SKIP=hex,hex,...: for finding which function a difference comes from)
pub static SKIP: [std::sync::atomic::AtomicU32; 512] = [const { std::sync::atomic::AtomicU32::new(0) }; 512];

pub static SAVED_CHECK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub static HEAP_LOG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub type DecFn = unsafe fn(&mut G, u32, u32, u32, u32, u32) -> u32;

/// What the decompiled code asks of the machine around it.
pub struct Ops {
    pub read32: unsafe fn(*mut u8, u32) -> u32,
    pub read16: unsafe fn(*mut u8, u32) -> u16,
    pub read8: unsafe fn(*mut u8, u32) -> u8,
    pub write32: unsafe fn(*mut u8, u32, u32),
    pub write16: unsafe fn(*mut u8, u32, u16),
    pub write8: unsafe fn(*mut u8, u32, u8),
    /// code that is not decompiled: (machine, address, return address, arguments, stack pointer) -> v0
    pub call: unsafe fn(*mut u8, u32, u32, [u32; 4], u32) -> u32,
    pub brk: unsafe fn(*mut u8, u32),
    /// a pending interrupt is delivered
    pub poll: unsafe fn(*mut u8),
    /// a relocated structure: (machine, kind, old pointer) -> the pointer the program is told (kind 0: the entity table is
    /// being made, 1: an object is)
    pub reloc: unsafe fn(*mut u8, u32, u32) -> u32,
    /// block-entry register trace (differential debugging)
    pub trace: unsafe fn(*mut u8, u32, u32, &[u32; 32]),
    /// differential testing: runs the decompiled function and the translated code on the same state and compares them
    pub checked: Option<unsafe fn(*mut u8, u32, DecFn, u32, [u32; 4], u32) -> u32>,
}

/// the machine's register file (the callee-saved registers and v1 are shared with the code that called)
pub struct Regs(pub *mut u32);
impl std::ops::Index<usize> for Regs {
    type Output = u32;
    #[inline(always)] fn index(&self, i: usize) -> &u32 { unsafe { &*self.0.add(i) } }
}
impl std::ops::IndexMut<usize> for Regs {
    #[inline(always)] fn index_mut(&mut self, i: usize) -> &mut u32 { unsafe { &mut *self.0.add(i) } }
}

pub struct G {
    pub r: Regs,
    pub mem: *mut u32,
    pub flag: *const u64,
    pub gte: *mut Gte,
    pub sp: u32,
    pub m: *mut u8,
    pub irq: *const bool,
    pub fuel: *mut i32,
    /// the machine's hi and lo (a function that multiplies or divides hands its result back in them)
    pub hi: *mut u32,
    pub lo: *mut u32,
    pub ops: &'static Ops,
}

const RAM: u32 = 0x200000;
/// the data and bss of the main executable live outside the image (src/mem.rs): loads from the old addresses go to the machine
pub const STATE_LO: u32 = 0x4F000;
pub const STATE_LEN: u32 = 0x20000;

impl G {
    /// the address a relocated structure's pointer holds: `new` when the structure lives outside the image
    #[inline(always)]
    pub unsafe fn reloc(&mut self, kind: u32, old: u32) -> u32 {
        if RELOC_ON.load(std::sync::atomic::Ordering::Relaxed) { (self.ops.reloc)(self.m, kind, old) } else { old }
    }
    #[inline(always)]
    pub unsafe fn lw(&mut self, a: u32) -> u32 {
        let p = a & 0x1FFF_FFFF;
        if p < RAM && p.wrapping_sub(STATE_LO) >= STATE_LEN { *self.mem.add((p >> 2) as usize) } else { (self.ops.read32)(self.m, a) }
    }
    #[inline(always)]
    pub unsafe fn lhu(&mut self, a: u32) -> u32 {
        let p = a & 0x1FFF_FFFF;
        if p < RAM && p.wrapping_sub(STATE_LO) >= STATE_LEN { (*self.mem.add((p >> 2) as usize) >> ((p & 2) * 8)) & 0xFFFF } else { (self.ops.read16)(self.m, a) as u32 }
    }
    #[inline(always)]
    pub unsafe fn lh(&mut self, a: u32) -> u32 { self.lhu(a) as u16 as i16 as i32 as u32 }
    #[inline(always)]
    pub unsafe fn lbu(&mut self, a: u32) -> u32 {
        let p = a & 0x1FFF_FFFF;
        if p < RAM && p.wrapping_sub(STATE_LO) >= STATE_LEN { (*self.mem.add((p >> 2) as usize) >> ((p & 3) * 8)) & 0xFF } else { (self.ops.read8)(self.m, a) as u32 }
    }
    #[inline(always)]
    pub unsafe fn lb(&mut self, a: u32) -> u32 { self.lbu(a) as u8 as i8 as i32 as u32 }
    #[inline(always)]
    pub unsafe fn lwl(&mut self, a: u32, old: u32) -> u32 {
        let w = self.lw(a & !3);
        let s = a & 3;
        let keep = if s == 3 { 0 } else { 0xFFFF_FFFFu32 >> ((s + 1) * 8) };
        (old & keep) | (w << ((3 - s) * 8))
    }
    #[inline(always)]
    pub unsafe fn lwr(&mut self, a: u32, old: u32) -> u32 {
        let w = self.lw(a & !3);
        let s = a & 3;
        let keep = if s == 0 { 0 } else { 0xFFFF_FFFFu32 << ((4 - s) * 8) };
        (old & keep) | (w >> (s * 8))
    }
    /// is this word one that translated blocks stand for (a store there goes through the machine)?
    #[inline(always)]
    unsafe fn flagged(&self, wi: usize) -> bool { (*self.flag.add(wi >> 6) >> (wi & 63)) & 1 != 0 }
    #[inline(always)]
    pub unsafe fn sw(&mut self, a: u32, v: u32) {
        let p = a & 0x1FFF_FFFF;
        if p < RAM && a & 3 == 0 && !self.flagged((p >> 2) as usize) { *self.mem.add((p >> 2) as usize) = v; } else { (self.ops.write32)(self.m, a, v) }
    }
    #[inline(always)]
    pub unsafe fn sh(&mut self, a: u32, v: u32) {
        let p = a & 0x1FFF_FFFF;
        if p < RAM && a & 1 == 0 && !self.flagged((p >> 2) as usize) {
            let w = self.mem.add((p >> 2) as usize);
            let s = (p & 2) * 8;
            *w = (*w & !(0xFFFFu32 << s)) | ((v & 0xFFFF) << s);
        } else { (self.ops.write16)(self.m, a, v as u16) }
    }
    #[inline(always)]
    pub unsafe fn sb(&mut self, a: u32, v: u32) {
        let p = a & 0x1FFF_FFFF;
        if p < RAM && !self.flagged((p >> 2) as usize) {
            let w = self.mem.add((p >> 2) as usize);
            let s = (p & 3) * 8;
            *w = (*w & !(0xFFu32 << s)) | ((v & 0xFF) << s);
        } else { (self.ops.write8)(self.m, a, v as u8) }
    }
    #[inline(always)]
    pub unsafe fn swl(&mut self, a: u32, v: u32) {
        let w = self.lw(a & !3);
        let s = a & 3;
        let keep = if s == 3 { 0 } else { 0xFFFF_FFFFu32 << ((s + 1) * 8) };
        self.sw(a & !3, (w & keep) | (v >> ((3 - s) * 8)));
    }
    #[inline(always)]
    pub unsafe fn swr(&mut self, a: u32, v: u32) {
        let w = self.lw(a & !3);
        let s = a & 3;
        let keep = if s == 0 { 0 } else { 0xFFFF_FFFFu32 >> ((4 - s) * 8) };
        self.sw(a & !3, (w & keep) | (v << (s * 8)));
    }
    #[inline(always)] pub unsafe fn gte_data(&self, n: usize) -> u32 { (*self.gte).read_data(n) }
    #[inline(always)] pub unsafe fn gte_ctrl(&self, n: usize) -> u32 { (*self.gte).read_control(n) }
    #[inline(always)] pub unsafe fn gte_set_data(&mut self, n: usize, v: u32) { (*self.gte).write_data(n, v) }
    #[inline(always)] pub unsafe fn gte_set_ctrl(&mut self, n: usize, v: u32) { (*self.gte).write_control(n, v) }
    #[inline(always)] pub unsafe fn gte_cmd(&mut self, c: u32) { (*self.gte).command(c) }
    pub unsafe fn brk(&mut self, code: u32) { (self.ops.brk)(self.m, code) }
    pub unsafe fn tb(&mut self, addr: u32, mask: u32, regs: [u32; 32]) { (self.ops.trace)(self.m, addr, mask, &regs) }
    /// at the top of every loop: a program that waits for an interrupt's doing has to see it come
    #[inline(always)]
    pub unsafe fn tick(&mut self) {
        if *self.irq || *self.fuel < 0 { (self.ops.poll)(self.m) }
    }
    /// the instructions a block stands for (the program's own count of time: a frame is so many of them)
    #[inline(always)]
    pub unsafe fn cost(&mut self, n: i32) { *self.fuel -= n; }

    /// A call by address: the decompiled function there if the code in memory is the one it was made from, else the
    /// translated code runs it.
    pub unsafe fn call_addr(&mut self, addr: u32, ra: u32, a0: u32, a1: u32, a2: u32, a3: u32) -> u32 {
        if HEAP_LOG.load(std::sync::atomic::Ordering::Relaxed) && (addr == 0x80031BDC || addr == 0x800320E8) {
            let v = self.call_addr_inner(addr, ra, a0, a1, a2, a3);
            if addr == 0x80031BDC { eprintln!("HEAP alloc size {} align {} from {:08X} -> {:08X}", a0, a1, ra, v); }
            else { eprintln!("HEAP free {:08X} from {:08X}", a0, ra); }
            return v;
        }
        self.call_addr_inner(addr, ra, a0, a1, a2, a3)
    }

    unsafe fn call_addr_inner(&mut self, addr: u32, ra: u32, a0: u32, a1: u32, a2: u32, a3: u32) -> u32 {
        if SAVED_CHECK.load(std::sync::atomic::Ordering::Relaxed) {
            let before: [u32; 10] = [16, 17, 18, 19, 20, 21, 22, 23, 28, 30].map(|r| self.r[r]);
            let v = self.call_addr_raw(addr, ra, a0, a1, a2, a3);
            let after: [u32; 10] = [16, 17, 18, 19, 20, 21, 22, 23, 28, 30].map(|r| self.r[r]);
            if before != after && ra != 0xFFFF_FFF0 {
                for k in 0..10 { if before[k] != after[k] { eprintln!("SAVED {} changed across call {:08X} (ra {:08X}): {:08X} -> {:08X}", [16, 17, 18, 19, 20, 21, 22, 23, 28, 30][k], addr, ra, before[k], after[k]); } }
            }
            return v;
        }
        self.call_addr_raw(addr, ra, a0, a1, a2, a3)
    }

    unsafe fn call_addr_raw(&mut self, addr: u32, ra: u32, a0: u32, a1: u32, a2: u32, a3: u32) -> u32 {
        if WATCH.load(std::sync::atomic::Ordering::Relaxed) && addr >= 0x80031000 && addr < 0x80034000 {
            for (k, a) in [a0].iter().enumerate() {
                if (0x90..=0x93).contains(&(*a >> 24)) { eprintln!("WATCH: allocator function {:08X} called with relocated pointer {:08X} as argument {}", addr, a, k); }
            }
        }
        if let Some(f) = lookup(self, addr) {
            if let Some(chk) = self.ops.checked { return chk(self.m, addr, f, ra, [a0, a1, a2, a3], self.sp); }
            return f(self, ra, a0, a1, a2, a3);
        }
        (self.ops.call)(self.m, addr, ra, [a0, a1, a2, a3], self.sp)
    }
}

/// the decompiled function at an address whose first words are in memory
pub unsafe fn lookup(g: &mut G, addr: u32) -> Option<DecFn> {
    if SKIP[0].load(std::sync::atomic::Ordering::Relaxed) != 0 {
        for s in SKIP.iter() { if s.load(std::sync::atomic::Ordering::Relaxed) == addr { return None; } }
    }
    let lo_ = RANGE[0].load(std::sync::atomic::Ordering::Relaxed);
    let hi_ = RANGE[1].load(std::sync::atomic::Ordering::Relaxed);
    let mut base_index = 0usize;
    for t in TABLES.iter() {
        let mut lo = 0usize;
        let mut hi = t.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if t[mid].0 < addr { lo = mid + 1 } else { hi = mid }
        }
        let mut i = lo;
        while i < t.len() && t[i].0 == addr {
            let idx = base_index + i;
            if idx >= lo_ && idx < hi_ {
                let sig = &t[i].1;
                let mut same = true;
                for k in 0..6 {
                    if g.lw(addr.wrapping_add(4 * k as u32)) != sig[k] { same = false; break; }
                }
                if same { return Some(t[i].2); }
            }
            i += 1;
        }
        base_index += t.len();
    }
    None
}

/// (table, position) of the n-th function over all modules
pub fn entry(n: usize) -> Option<(u32, [u32; 6], DecFn)> {
    let mut n = n;
    for t in TABLES.iter() {
        if n < t.len() { return Some((t[n].0, t[n].1, t[n].2)); }
        n -= t.len();
    }
    None
}

pub fn count() -> usize { TABLES.iter().map(|t| t.len()).sum() }
