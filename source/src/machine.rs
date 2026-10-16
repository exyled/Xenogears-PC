// The whole console as the game program sees it: the R3000 interpreter with its memory and devices, and the host's answers
// for what the program asks of the kernel (events, the memory card, the file calls of its host-file mode, the frame clock,
// the pad, the picture chip and the sound chip). The program's own code runs as written, from its entry, mode after mode.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::archive::{Archive, Store, ENTRY_SIZE, HEADER_BYTES};
use crate::card::Card;
use crate::gpu::Gpu;
use crate::gte::Gte;
use crate::mdec::Mdec;
use crate::movie::Movies;
use std::sync::Arc;
use crate::mem::{Mem, RAM_SIZE, RAM_WORDS};
use crate::spu::{Spu, RATE};
use crate::xa;

mod native;
mod services;

pub const SENTINEL: u32 = 0xFFFFFFF0;
pub const GUEST_FUEL: i32 = FRAME_STEPS as i32;
const GP: u32 = 0x80059170;
const SELECT_MODE: u32 = 0x8001996C;
const MODE_NUMBER: u32 = 0x80018088;
const LOADED_MODE: u32 = 0x800592C0;
const MAIN_LOOP: u32 = 0x80000100;
const VBLANK_HANDLER: u32 = 0x8003634C;
const VBLANK_STACK: u32 = 0x8000CFF0;
const PAD_PORT0: u32 = 0x800625FC;
const PAD_PORT1: u32 = 0x8006261E;
const VBLANKS: u32 = 0x80058960;
const LAST_VSYNC: u32 = 0x80057848;
const HOST_NAMES: u32 = 0x40000000;
const SW_CARD: u32 = 0xF4000001;
/// A vertical blank's time on the console (33.9 MHz over 60 blanks, at about two cycles an instruction).
const FRAME_STEPS: i64 = 280000;
pub const SOUND_RATE: usize = RATE as usize;
pub const SAMPLES_PER_FRAME: usize = 735;

#[derive(Clone, Copy, PartialEq)]
enum H {
    None,
    Zero,
    EnterCritical,
    ExitCritical,
    Ret(u32),
    GpuReg,
    GpuExec,
    ClearOtagR,
    GpuQueue,
    DiscStart,
    SelectMode,
    PadQueue,
    HostOpen,
    HostClose,
    HostSeek,
    HostRead,
    DmaCallback,
    VSync,
    OpenEvent,
    TestEvent,
    Deliver,
    UnDeliver,
    CardInfo,
    CardOpen,
    CardRead,
    CardWrite,
    CardClose,
    CardFormat,
    FirstFile,
    NextFile,
    Rename,
    Erase,
    ObsModeNumber { tramp: u32, addr: u32 },
    ObsSoundStart { tramp: u32, addr: u32 },
    Native(u16),
}

struct HostFile {
    entry: usize,
    data: Vec<u8>,
    position: i64,
    audio_done: i64,
    stream: bool,
    whole: bool,
}

struct CardFile {
    first: i32,
    position: usize,
    slot: u8,
}

pub struct Machine {
    // processor
    pub r: [u32; 32],
    pub pc: u32,
    pub npc: u32,
    pub hi: u32,
    pub lo: u32,
    cur_pc: u32,
    pub mem: Mem,
    pub gte: Gte,
    steps: i64,
    pub stop_reason: Option<String>,
    yield_: bool,
    hook: Vec<H>,
    slow: Vec<bool>,
    return_default: bool,
    slow_state: i32,
    irq_pending: bool,
    irq_delay: i32,
    critical: bool,
    in_irq: bool,
    /// a differential test is running: no test inside a test, and while the translated code is the reference no decompiled function runs
    pub(super) in_check: bool,
    pub(super) ref_run: bool,
    /// the program runs on a thread of its own (decompiled code keeps its place in the Rust call stack)
    guest_mode: bool,
    dec_check: u32,
    dec_seen: std::collections::HashMap<usize, u32>,
    dec_seen_addr: std::collections::HashMap<u32, u32>,
    dec_by_addr: std::collections::HashMap<u32, Vec<usize>>,
    pub dec_trace: Option<Vec<(u32, u32, [u32; 32])>>,
    xtrace_set: Option<std::collections::HashSet<u32>>,
    xtrace_out: Vec<(u32, [u32; 32])>,
    run_limit: i64,
    run_max: i64,
    /// loop turns left in this frame (the decompiled code has no instruction count: a frame's time is up when this runs out)
    guest_fuel: i32,
    guest_fuel_seen: i32,
    guest_go: Option<std::sync::mpsc::Receiver<()>>,
    guest_back: Option<std::sync::mpsc::Sender<u8>>,
    guest_link: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<u8>, std::thread::JoinHandle<()>)>,
    // devices
    pub gpu: Gpu,
    pub spu: Spu,
    mdec: Mdec,
    pub card: Card,
    pub card2: Option<Card>,
    // the program
    archive: Archive,
    pub pad: i32,
    pub finished: bool,
    pub frames: i64,
    pub error: Option<String>,
    started: bool,
    pub mode: i32,
    /// the picture is asked for in 16:9 for the 3D scenes (battle, world map, arena): the geometry engine squeezes what it
    /// projects so that the view takes in more
    pub wide_opt: bool,
    pub running: i32,
    pub modes: Vec<String>,
    next_trampoline: u32,
    vsync_target: u32,
    sound_ready: bool,
    ticks_due: i32,
    tick: i32,
    dma_callback: [u32; 8],
    dma_pending: i32,
    // host files
    open: HashMap<u32, HostFile>,
    next_handle: u32,
    cached_entry: i64,
    cached: Vec<u8>,
    stream_entry: i64,
    /// upscaled movies (tools/movies.py) and the movie frame the program read last: entry, frame number, machine frame
    movies: Movies,
    movie_at: (i64, u32, i64),
    pub movie_stats: [u64; 4],
    /// the last picture composed from a movie frame: (frame address, width, first row, rows) and its pixels
    movie_cache: ((usize, usize, usize, usize), Vec<u32>),
    stream_whole: bool,
    stream: Vec<u8>,
    // movie audio into the chip's CD input
    xa: Vec<i16>,
    xa_at: Vec<i64>,
    cd: Vec<i16>,
    cd_read: usize,
    cd_count: usize,
    // kernel events and the card
    event_class: Vec<u32>,
    event_spec: Vec<u32>,
    delivered: HashSet<u64>,
    card_files: HashMap<u32, CardFile>,
    found: Option<Vec<String>>,
    found_next: usize,
    found_slot: u8,
    // sound out
    sound: Vec<i16>,
    sound_write: usize,
    sound_read: usize,
    mix: Vec<i16>,
    // the translated blocks of the program (see native.rs)
    native_on: bool,
    native_dirty: bool,
    native_idx: Vec<u32>,
    native_fns: Vec<native::BlockFn>,
    native_len: Vec<u32>,
    native_woff: Vec<u32>,
    native_alt: Vec<u32>,
    ctx: Box<xgblocks::Ctx>,
    native_stamp: Vec<u32>,
    native_okv: Vec<u8>,
    exe_words: Vec<u32>,
    tracing: bool,
    trace_bits: Vec<u64>,
    trace_entries: Vec<u64>,
    trace_word: Vec<u32>,
    trace_events: Vec<(u8, u32, u32)>,
    trace_marked: usize,
    pub native_steps: i64,
    pub native_why: [u64; 6],
    pub native_hist: Vec<u32>,
    pub io_stat: std::collections::HashMap<(u32, u32, u8), u32>,
    in_service: bool,
    pub native_words_hist: std::collections::HashMap<u32, u32>,
    counting: bool,
    pub total_steps: i64,
}

fn event_key(cls: u32, spec: u32) -> u64 { (cls as u64) << 32 | spec as u64 }

impl Machine {
    pub fn new(archive: Archive, executable: &[u8], card_path: Option<PathBuf>, card2_path: Option<PathBuf>) -> Result<Machine, String> {
        let Store::Dir(game_dir) = &archive.store;
        let movie_dir = Some(game_dir.join("movies"));
        let card2 = match card2_path {
            Some(p) if p.exists() => Some(Card::new(Some(p))),
            _ => None,
        };
        let mut m = Machine {
            r: [0; 32], pc: 0, npc: 0, hi: 0, lo: 0, cur_pc: 0,
            mem: Mem::new(), gte: Gte::new(), steps: 0, stop_reason: None, yield_: false,
            hook: vec![H::None; RAM_WORDS], slow: vec![false; RAM_WORDS],
            return_default: false, slow_state: 0, irq_pending: false, irq_delay: 0, critical: false, in_irq: false, in_check: false, ref_run: false, guest_mode: false, wide_opt: false, dec_check: std::env::var("XG_DEC_CHECK").ok().and_then(|v| v.parse().ok()).unwrap_or(0), dec_seen: std::collections::HashMap::new(), dec_seen_addr: std::collections::HashMap::new(), dec_by_addr: std::collections::HashMap::new(), dec_trace: None, xtrace_set: None, xtrace_out: Vec::new(), run_limit: 0, run_max: 0, guest_fuel: GUEST_FUEL, guest_fuel_seen: GUEST_FUEL, guest_go: None, guest_back: None, guest_link: None,
            gpu: Gpu::new(), spu: Spu::new(), mdec: Mdec::new(), card: Card::new(card_path), card2,
            archive, pad: 0, finished: false, frames: 0, error: None, started: false, mode: 0, running: -1, modes: Vec::new(),
            next_trampoline: 0x80000180, vsync_target: 0, sound_ready: false, ticks_due: 0, tick: 0,
            dma_callback: [0; 8], dma_pending: 0,
            open: HashMap::new(), next_handle: 3, cached_entry: -1, cached: Vec::new(), stream_entry: -1, movies: Movies::new(movie_dir), movie_at: (-1, 0, 0), movie_stats: [0; 4], movie_cache: ((0, 0, 0, 0), Vec::new()), stream_whole: false, stream: Vec::new(),
            xa: Vec::new(), xa_at: Vec::new(), cd: vec![0; SOUND_RATE * 2 * 4], cd_read: 0, cd_count: 0,
            event_class: Vec::new(), event_spec: Vec::new(), delivered: HashSet::new(),
            card_files: HashMap::new(), found: None, found_next: 0, found_slot: 0,
            sound: vec![0; SOUND_RATE * 2], sound_write: 0, sound_read: 0, mix: Vec::new(),
            native_on: false, native_dirty: true, native_idx: Vec::new(), native_fns: Vec::new(), native_len: Vec::new(), native_woff: Vec::new(), native_alt: Vec::new(), ctx: native::new_ctx(), native_stamp: Vec::new(), native_okv: Vec::new(),
            exe_words: Vec::new(),
            tracing: std::env::var("XG_TRACE").is_ok(), trace_bits: vec![0u64; RAM_WORDS / 64 + 1], trace_entries: vec![0u64; RAM_WORDS / 64 + 1], trace_word: Vec::new(), trace_events: Vec::new(), trace_marked: 0, native_steps: 0, native_why: [0; 6], native_hist: vec![0u32; 0x2000], io_stat: std::collections::HashMap::new(), in_service: false, native_words_hist: std::collections::HashMap::new(), total_steps: 0, counting: std::env::var("XG_NATIVE_INFO").is_ok(),
        };
        m.gpu.zt = m.gte.zrec.as_ptr();
        m.gpu.zf = m.gte.zfrac.as_ptr();
        m.slow[0] = true;
        m.slow[0xA0 >> 2] = true;
        m.slow[0xB0 >> 2] = true;
        m.slow[0xC0 >> 2] = true;
        m.load_program(executable)?;
        m.install_program();
        if m.tracing { m.trace_word = vec![0u32; RAM_WORDS]; }
        m.native_init();
        Ok(m)
    }

    // ---- hooks ----

    fn set_hook(&mut self, address: u32, h: H) {
        let i = ((address & 0x1FFFFF) >> 2) as usize;
        self.hook[i] = h;
        self.slow[i] = h != H::None || i == 0 || i == (0xA0 >> 2) || i == (0xB0 >> 2) || i == (0xC0 >> 2);
        self.native_dirty = true;
    }

    // ---- memory with the device ports on top ----

    #[inline]
    fn io_read32(&mut self, a: u32) -> u32 {
        if self.counting && !self.in_service { *self.io_stat.entry((self.cur_pc, a & 0xFFFF, 0)).or_insert(0) += 1; }
        if Mdec::owns(a) { self.mdec.read(a) } else if Spu::owns(a) { self.spu.read(a, 4) } else { 0 }
    }

    fn read32(&mut self, a: u32) -> u32 {
        let p = a & 0x1FFFFFFF;
        if p >= 0x1F801000 && p < 0x1F803000 {
            return self.io_read32(p);
        }
        self.mem.read32(a)
    }

    fn read16(&mut self, a: u32) -> u16 {
        let p = a & 0x1FFFFFFF;
        if p >= 0x1F801000 && p < 0x1F803000 {
            if Spu::owns(p) { return self.spu.read(p, 2) as u16; }
            return (self.io_read32(p & !3) >> ((p & 2) * 8)) as u16;
        }
        self.mem.read16(a)
    }

    fn read8(&mut self, a: u32) -> u8 {
        let p = a & 0x1FFFFFFF;
        if p >= 0x1F801000 && p < 0x1F803000 {
            return (self.io_read32(p & !3) >> ((p & 3) * 8)) as u8;
        }
        self.mem.read8(a)
    }

    fn io_write(&mut self, a: u32, v: u32, size: i32) {
        if self.counting && !self.in_service { *self.io_stat.entry((self.cur_pc, a & 0xFFFF, 1)).or_insert(0) += 1; }
        if Mdec::owns(a) {
            self.mdec.write(&mut self.mem, a, v);
            for ch in 0..2 {
                if self.mdec.dma_done[ch] {
                    self.mdec.dma_done[ch] = false;
                    if self.dma_callback[ch] != 0 { self.raise_dma(ch); }
                }
            }
        } else if Spu::owns(a) {
            self.spu.write(&mut self.mem, a, v, size);
            if self.spu.dma_done {
                self.spu.dma_done = false;
                if self.dma_callback[4] != 0 { self.raise_dma(4); }
            }
        }
    }

    fn raise_dma(&mut self, ch: usize) {
        self.dma_pending |= 1 << ch;
        self.irq_pending = true;
        self.irq_delay = 2000;
    }

    fn write8(&mut self, a: u32, v: u8) {
        let p = a & 0x1FFFFFFF;
        if p >= 0x1F801000 && p < 0x1F803000 { self.io_write(p, v as u32, 1); } else { self.mem.write8(a, v); }
    }

    fn write16(&mut self, a: u32, v: u16) {
        let p = a & 0x1FFFFFFF;
        if p >= 0x1F801000 && p < 0x1F803000 { self.io_write(p, v as u32, 2); } else { self.mem.write16(a, v); }
    }

    fn write32(&mut self, a: u32, v: u32) {
        let p = a & 0x1FFFFFFF;
        if p >= 0x1F801000 && p < 0x1F803000 { self.io_write(p, v, 4); } else { self.mem.write32(a, v); }
    }

    // ---- running ----

    pub fn prepare(&mut self, entry: u32, a0: u32, a1: u32, a2: u32, a3: u32) {
        self.r[4] = a0;
        self.r[5] = a1;
        self.r[6] = a2;
        self.r[7] = a3;
        self.r[31] = SENTINEL;
        self.pc = entry;
        self.npc = entry.wrapping_add(4);
        self.stop_reason = None;
    }

    /// Calls a function and runs until it returns; false when it stopped early.
    pub fn call(&mut self, entry: u32, a0: u32, a1: u32, a2: u32, a3: u32) -> bool {
        self.prepare(entry, a0, a1, a2, a3);
        self.run(50_000_000) && self.pc == SENTINEL
    }

    fn fail(&mut self, pc: u32, npc: u32, steps: i64, what: &str, cur: u32) -> bool {
        self.pc = pc;
        self.npc = npc;
        self.steps = steps;
        self.cur_pc = cur;
        self.stop_reason = Some(format!("{}{:08X}", what, cur));
        false
    }

    /// Runs from the current state until the call returns, a hook asks to yield, or something goes wrong.
    pub fn run(&mut self, max_steps: i64) -> bool {
        let saved = (self.run_limit, self.run_max);
        self.run_limit = self.steps + max_steps;
        self.run_max = max_steps;
        let r = self.run_inner(max_steps);
        self.run_limit = saved.0;
        self.run_max = saved.1;
        r
    }

    fn run_inner(&mut self, max_steps: i64) -> bool {
        let limit = self.steps + max_steps;
        let rp: *mut u32 = self.r.as_mut_ptr();
        let wp: *mut u32 = self.mem.w.as_mut_ptr();
        let sp: *const bool = self.slow.as_ptr();
        unsafe {
        self.yield_ = false;
        let mut fast_mask: u32 = if self.irq_pending { 0xFFFFFFFF } else { 0x1FE00003 };
        let mut pc = self.pc;
        let mut npc = self.npc;
        let mut cur = self.cur_pc;
        let mut budget = max_steps;
        if self.native_dirty { self.native_init(); }
        let native_on = self.native_on;
        self.native_bind();
        let genp: *mut u32 = &mut self.mem.gen;
        let flagp: *const u64 = self.mem.flag.as_ptr();
        let stamp: *mut u32 = self.native_stamp.as_mut_ptr();
        let okv: *mut u8 = self.native_okv.as_mut_ptr();
        let nat: *const u32 = self.native_idx.as_ptr();
        let tracing = self.tracing;
        let counting = self.counting;
        let tr: *mut u64 = self.trace_bits.as_mut_ptr();
        let te: *mut u64 = self.trace_entries.as_mut_ptr();
        'run: loop {
            // a pending interrupt does not keep the blocks from running: the interpreter delivers it after irq_delay fetches (and
            // not at all while one is being handled), so a block only has to be shorter than that and use up that many
            if native_on && (pc & 0x1FE00003) == 0 && npc == pc.wrapping_add(4) {
                let at = ((pc >> 2) & 0x7FFFF) as usize;
                let mut bi = *nat.add(at);
                while bi != 0 {
                    let b = (bi - 1) as usize;
                    let n = *self.native_len.get_unchecked(b) as usize;
                    // the block is valid while no word it stands for was written since its words were last compared
                    let same;
                    if *okv.add(b) != 0 && *stamp.add(b) == *genp {
                        same = *okv.add(b) == 1;
                    } else {
                        let want = native::words().as_ptr().add(*self.native_woff.get_unchecked(b) as usize);
                        let have = self.mem.w.as_ptr().add(at);
                        let mut ok = true;
                        for k in 0..n { if *want.add(k) != *have.add(k) { ok = false; break; } }
                        *stamp.add(b) = *genp;
                        *okv.add(b) = if ok { 1 } else { 2 };
                        same = ok;
                    }
                    if !same { bi = *self.native_alt.get_unchecked(b); continue; }
                    let counts_down = self.irq_pending && !self.in_irq;
                    if budget >= n as i64 && (!counts_down || self.irq_delay > n as i32) {
                        let f = *self.native_fns.get_unchecked(b);
                        let c: *mut xgblocks::Ctx = &mut *self.ctx;
                        (*c).pc = pc; (*c).npc = npc; (*c).hi = self.hi; (*c).lo = self.lo; (*c).critical = self.critical;
                        let done = f(&mut *c);
                        pc = (*c).pc; npc = (*c).npc; cur = (*c).cur_pc;
                        self.hi = (*c).hi; self.lo = (*c).lo; self.critical = (*c).critical;
                        self.pc = pc; self.npc = npc; self.cur_pc = cur;
                        budget -= done as i64;
                        self.native_steps += done as i64;
                        if counts_down { self.irq_delay -= done; }
                        self.mem.hit = false;
                        fast_mask = if self.irq_pending { 0xFFFFFFFF } else { 0x1FE00003 };
                        continue 'run;
                    }
                    break;
                }
            }
            if counting && self.xtrace_set.is_some() {
                if self.xtrace_set.as_ref().unwrap().contains(&pc) { let regs = self.r; self.xtrace_out.push((pc, regs)); }
            }
            if counting {
                // why this instruction is interpreted: 0 blocks off, 1 interrupt pending, 2 pc not in RAM or unaligned, 3 in a delay
                // slot, 4 no block starts here, 5 a block starts here but its words differ or the budget is short
                let why = if !native_on { 0 } else if (pc & fast_mask) != 0 { if self.irq_pending { 1 } else { 2 } }
                    else if npc != pc.wrapping_add(4) { 3 }
                    else if *nat.add(((pc >> 2) & 0x7FFFF) as usize) == 0 { 4 } else { 5 };
                self.native_why[why] += 1;
                let pg = ((pc >> 8) & 0x1FFF) as usize;
                self.native_hist[pg] += 1;
                *self.native_words_hist.entry(pc).or_insert(0) += 1;
            }
            let before = budget;
            budget -= 1;
            if before < 0 {
                self.cur_pc = cur;
                self.pc = pc;
                self.npc = npc;
                if pc == SENTINEL { self.steps = limit - budget - 1; return true; }
                self.steps = limit - budget;
                self.stop_reason = Some(format!("step budget at 0x{:08X}", pc));
                return false;
            }
            let idx = ((pc >> 2) & 0x7FFFF) as usize;
            let i: u32;
            if (pc & fast_mask) == 0 && !*sp.add(idx) {
                i = *wp.add(idx);
            } else {
                self.cur_pc = cur;
                self.pc = pc;
                self.npc = npc;
                if pc == SENTINEL { self.steps = limit - budget - 1; return true; }
                self.steps = limit - budget;
                i = self.slow_fetch();
                let st = self.slow_state;
                if st != 0 {
                    if st != 1 { return st == 2; }
                    pc = self.pc;
                    npc = self.npc;
                    budget = limit - self.steps;
                    fast_mask = if self.irq_pending { 0xFFFFFFFF } else { 0x1FE00003 };
                    continue;
                }
            }
            let prev_cur = cur;
            cur = pc;
            if tracing {
                let w = idx >> 6;
                let bit = 1u64 << (idx & 63);
                // a word is logged when it is first run at its address, and again when the address holds another one
                if *tr.add(w) & bit == 0 || *self.trace_word.get_unchecked(idx) != i {
                    *tr.add(w) |= bit;
                    *self.trace_word.get_unchecked_mut(idx) = i;
                    self.trace_events.push((b'W', pc, i));
                }
                if pc != prev_cur.wrapping_add(4) && *te.add(w) & bit == 0 { *te.add(w) |= bit; self.trace_events.push((b'N', pc, 0)); }
            }
            if counting { self.total_steps += 1; }
            pc = npc;
            npc = pc.wrapping_add(4);
            let rs = ((i >> 21) & 31) as usize;
            let rt = ((i >> 16) & 31) as usize;
            let rd = ((i >> 11) & 31) as usize;
            let op = i >> 26;
            let imm = i as i16 as i32 as u32;
            let key = if op != 0 { op } else { 64 + (i & 63) };
            match key {
                64 => (*rp.add(rd)) = (*rp.add(rt)) << ((i >> 6) & 31),
                66 => (*rp.add(rd)) = (*rp.add(rt)) >> ((i >> 6) & 31),
                67 => (*rp.add(rd)) = (((*rp.add(rt)) as i32) >> ((i >> 6) & 31)) as u32,
                68 => (*rp.add(rd)) = (*rp.add(rt)) << ((*rp.add(rs)) & 31),
                70 => (*rp.add(rd)) = (*rp.add(rt)) >> ((*rp.add(rs)) & 31),
                71 => (*rp.add(rd)) = (((*rp.add(rt)) as i32) >> ((*rp.add(rs)) & 31)) as u32,
                72 => npc = *rp.add(rs),
                73 => {
                    let t = *rp.add(rs);
                    (*rp.add(rd)) = cur.wrapping_add(8);
                    npc = t;
                }
                76 => {
                    // syscall: EnterCriticalSection (1) masks the interrupts, ExitCriticalSection (2) lets them in
                    if (*rp.add(4)) == 1 {
                        (*rp.add(2)) = if self.critical { 0 } else { 1 };
                        self.critical = true;
                    } else {
                        if (*rp.add(4)) == 2 { self.critical = false; }
                        (*rp.add(2)) = 1;
                    }
                }
                77 => return self.fail(pc, npc, limit - budget, "break at 0x", cur),
                80 => (*rp.add(rd)) = self.hi,
                81 => self.hi = *rp.add(rs),
                82 => (*rp.add(rd)) = self.lo,
                83 => self.lo = *rp.add(rs),
                88 => {
                    let p = ((*rp.add(rs)) as i32 as i64) * ((*rp.add(rt)) as i32 as i64);
                    self.lo = p as u32;
                    self.hi = (p >> 32) as u32;
                }
                89 => {
                    let p = ((*rp.add(rs)) as u64) * ((*rp.add(rt)) as u64);
                    self.lo = p as u32;
                    self.hi = (p >> 32) as u32;
                }
                90 => {
                    let a = (*rp.add(rs)) as i32;
                    let b = (*rp.add(rt)) as i32;
                    if b == 0 {
                        self.lo = if a >= 0 { 0xFFFFFFFF } else { 1 };
                        self.hi = a as u32;
                    } else if a == i32::MIN && b == -1 {
                        self.lo = 0x80000000;
                        self.hi = 0;
                    } else {
                        self.lo = (a / b) as u32;
                        self.hi = (a % b) as u32;
                    }
                }
                91 => {
                    let a = *rp.add(rs);
                    let b = *rp.add(rt);
                    if b == 0 {
                        self.lo = 0xFFFFFFFF;
                        self.hi = a;
                    } else {
                        self.lo = a / b;
                        self.hi = a % b;
                    }
                }
                96 | 97 => (*rp.add(rd)) = (*rp.add(rs)).wrapping_add(*rp.add(rt)),
                98 | 99 => (*rp.add(rd)) = (*rp.add(rs)).wrapping_sub(*rp.add(rt)),
                100 => (*rp.add(rd)) = (*rp.add(rs)) & (*rp.add(rt)),
                101 => (*rp.add(rd)) = (*rp.add(rs)) | (*rp.add(rt)),
                102 => (*rp.add(rd)) = (*rp.add(rs)) ^ (*rp.add(rt)),
                103 => (*rp.add(rd)) = !((*rp.add(rs)) | (*rp.add(rt))),
                106 => (*rp.add(rd)) = if ((*rp.add(rs)) as i32) < ((*rp.add(rt)) as i32) { 1 } else { 0 },
                107 => (*rp.add(rd)) = if (*rp.add(rs)) < (*rp.add(rt)) { 1 } else { 0 },
                1 => {
                    let ge = rt & 1 != 0;
                    let link = (rt & 0x1E) == 0x10;
                    let take = if ge { ((*rp.add(rs)) as i32) >= 0 } else { ((*rp.add(rs)) as i32) < 0 };
                    if link { (*rp.add(31)) = cur.wrapping_add(8); }
                    if take { npc = cur.wrapping_add(4).wrapping_add(imm << 2); }
                }
                2 => npc = (cur & 0xF0000000) | ((i & 0x3FFFFFF) << 2),
                3 => {
                    (*rp.add(31)) = cur.wrapping_add(8);
                    npc = (cur & 0xF0000000) | ((i & 0x3FFFFFF) << 2);
                }
                4 => { if (*rp.add(rs)) == (*rp.add(rt)) { npc = cur.wrapping_add(4).wrapping_add(imm << 2); } }
                5 => { if (*rp.add(rs)) != (*rp.add(rt)) { npc = cur.wrapping_add(4).wrapping_add(imm << 2); } }
                6 => { if ((*rp.add(rs)) as i32) <= 0 { npc = cur.wrapping_add(4).wrapping_add(imm << 2); } }
                7 => { if ((*rp.add(rs)) as i32) > 0 { npc = cur.wrapping_add(4).wrapping_add(imm << 2); } }
                8 | 9 => (*rp.add(rt)) = (*rp.add(rs)).wrapping_add(imm),
                10 => (*rp.add(rt)) = if ((*rp.add(rs)) as i32) < (imm as i32) { 1 } else { 0 },
                11 => (*rp.add(rt)) = if (*rp.add(rs)) < imm { 1 } else { 0 },
                12 => (*rp.add(rt)) = (*rp.add(rs)) & (i & 0xFFFF),
                13 => (*rp.add(rt)) = (*rp.add(rs)) | (i & 0xFFFF),
                14 => (*rp.add(rt)) = (*rp.add(rs)) ^ (i & 0xFFFF),
                15 => (*rp.add(rt)) = (i & 0xFFFF) << 16,
                16 => {
                    // coprocessor 0: reads give 0, writes are ignored, RFE does nothing
                    if rs == 0 || rs == 2 { (*rp.add(rt)) = 0; }
                }
                18 => {
                    if i & 0x02000000 != 0 { self.gte.command(i & 0x1FFFFFF); }
                    else {
                        match rs {
                            0 => (*rp.add(rt)) = self.gte.read_data(rd),
                            2 => (*rp.add(rt)) = self.gte.read_control(rd),
                            4 => self.gte.write_data(rd, *rp.add(rt)),
                            6 => self.gte.write_control(rd, *rp.add(rt)),
                            _ => {}
                        }
                    }
                }
                32 => {
                    let ea = (*rp.add(rs)).wrapping_add(imm);
                    let ea2 = ea & 0x1FFFFFFF;
                    if ea2 < RAM_SIZE && ea2.wrapping_sub(crate::mem::STATE_OLD) >= crate::mem::STATE_BYTES { (*rp.add(rt)) = (((*wp.add((ea2 >> 2) as usize)) >> ((ea2 & 3) * 8)) as i8) as i32 as u32; }
                    else {
                        self.cur_pc = cur; self.pc = pc; self.npc = npc;
                        (*rp.add(rt)) = (self.read8(ea) as i8) as i32 as u32;
                    }
                }
                33 => {
                    let ea = (*rp.add(rs)).wrapping_add(imm);
                    let ea2 = ea & 0x1FFFFFFF;
                    if ea2 < RAM_SIZE && ea2.wrapping_sub(crate::mem::STATE_OLD) >= crate::mem::STATE_BYTES { (*rp.add(rt)) = (((*wp.add((ea2 >> 2) as usize)) >> ((ea2 & 2) * 8)) as i16) as i32 as u32; }
                    else {
                        self.cur_pc = cur; self.pc = pc; self.npc = npc;
                        (*rp.add(rt)) = (self.read16(ea) as i16) as i32 as u32;
                    }
                }
                34 => {
                    // lwl
                    let ea = (*rp.add(rs)).wrapping_add(imm);
                    let wd = self.read32(ea & !3);
                    let s = ea & 3;
                    let keep = if s == 3 { 0 } else { 0xFFFFFFFFu32 >> ((s + 1) * 8) };
                    (*rp.add(rt)) = ((*rp.add(rt)) & keep) | (wd << ((3 - s) * 8));
                }
                35 => {
                    let ea = (*rp.add(rs)).wrapping_add(imm);
                    let ea2 = ea & 0x1FFFFFFF;
                    if ea2 < RAM_SIZE && ea2.wrapping_sub(crate::mem::STATE_OLD) >= crate::mem::STATE_BYTES && ea2 & 3 == 0 { (*rp.add(rt)) = *wp.add((ea2 >> 2) as usize); }
                    else {
                        self.cur_pc = cur; self.pc = pc; self.npc = npc;
                        (*rp.add(rt)) = self.read32(ea);
                    }
                }
                36 => {
                    let ea = (*rp.add(rs)).wrapping_add(imm);
                    let ea2 = ea & 0x1FFFFFFF;
                    if ea2 < RAM_SIZE && ea2.wrapping_sub(crate::mem::STATE_OLD) >= crate::mem::STATE_BYTES { (*rp.add(rt)) = ((*wp.add((ea2 >> 2) as usize)) >> ((ea2 & 3) * 8)) & 0xFF; }
                    else {
                        self.cur_pc = cur; self.pc = pc; self.npc = npc;
                        (*rp.add(rt)) = self.read8(ea) as u32;
                    }
                }
                37 => {
                    let ea = (*rp.add(rs)).wrapping_add(imm);
                    let ea2 = ea & 0x1FFFFFFF;
                    if ea2 < RAM_SIZE && ea2.wrapping_sub(crate::mem::STATE_OLD) >= crate::mem::STATE_BYTES { (*rp.add(rt)) = ((*wp.add((ea2 >> 2) as usize)) >> ((ea2 & 2) * 8)) & 0xFFFF; }
                    else {
                        self.cur_pc = cur; self.pc = pc; self.npc = npc;
                        (*rp.add(rt)) = self.read16(ea) as u32;
                    }
                }
                38 => {
                    // lwr
                    let ea = (*rp.add(rs)).wrapping_add(imm);
                    let wd = self.read32(ea & !3);
                    let s = ea & 3;
                    let keep = if s == 0 { 0 } else { 0xFFFFFFFFu32 << ((4 - s) * 8) };
                    (*rp.add(rt)) = ((*rp.add(rt)) & keep) | (wd >> (s * 8));
                }
                40 => {
                    let ea = (*rp.add(rs)).wrapping_add(imm);
                    let ea2 = ea & 0x1FFFFFFF;
                    if ea2 < RAM_SIZE && (*flagp.add((ea2 >> 8) as usize) >> ((ea2 >> 2) & 63)) & 1 == 0 {
                        let s = (ea2 & 3) * 8;
                        let wi = (ea2 >> 2) as usize;
                        (*wp.add(wi)) = ((*wp.add(wi)) & !(0xFFu32 << s)) | (((*rp.add(rt)) & 0xFF) << s);
                    } else {
                        self.cur_pc = cur; self.pc = pc; self.npc = npc;
                        self.write8(ea, (*rp.add(rt)) as u8);
                        fast_mask = if self.irq_pending { 0xFFFFFFFF } else { 0x1FE00003 };
                    }
                }
                41 => {
                    let ea = (*rp.add(rs)).wrapping_add(imm);
                    let ea2 = ea & 0x1FFFFFFF;
                    if ea2 < RAM_SIZE && ea2 & 1 == 0 && (*flagp.add((ea2 >> 8) as usize) >> ((ea2 >> 2) & 63)) & 1 == 0 {
                        let s = (ea2 & 2) * 8;
                        let wi = (ea2 >> 2) as usize;
                        (*wp.add(wi)) = ((*wp.add(wi)) & !(0xFFFFu32 << s)) | (((*rp.add(rt)) & 0xFFFF) << s);
                    } else {
                        self.cur_pc = cur; self.pc = pc; self.npc = npc;
                        self.write16(ea, (*rp.add(rt)) as u16);
                        fast_mask = if self.irq_pending { 0xFFFFFFFF } else { 0x1FE00003 };
                    }
                }
                42 => {
                    // swl
                    let ea = (*rp.add(rs)).wrapping_add(imm);
                    let wd = self.read32(ea & !3);
                    let s = ea & 3;
                    let keep = if s == 3 { 0 } else { 0xFFFFFFFFu32 << ((s + 1) * 8) };
                    self.cur_pc = cur; self.pc = pc; self.npc = npc;
                    let v = (wd & keep) | ((*rp.add(rt)) >> ((3 - s) * 8));
                    self.write32(ea & !3, v);
                    fast_mask = if self.irq_pending { 0xFFFFFFFF } else { 0x1FE00003 };
                }
                43 => {
                    let ea = (*rp.add(rs)).wrapping_add(imm);
                    let ea2 = ea & 0x1FFFFFFF;
                    if ea2 < RAM_SIZE && ea2 & 3 == 0 && (*flagp.add((ea2 >> 8) as usize) >> ((ea2 >> 2) & 63)) & 1 == 0 {
                        (*wp.add((ea2 >> 2) as usize)) = *rp.add(rt);
                    } else {
                        self.cur_pc = cur; self.pc = pc; self.npc = npc;
                        self.write32(ea, *rp.add(rt));
                        fast_mask = if self.irq_pending { 0xFFFFFFFF } else { 0x1FE00003 };
                    }
                }
                46 => {
                    // swr
                    let ea = (*rp.add(rs)).wrapping_add(imm);
                    let wd = self.read32(ea & !3);
                    let s = ea & 3;
                    let keep = if s == 0 { 0 } else { 0xFFFFFFFFu32 >> ((4 - s) * 8) };
                    self.cur_pc = cur; self.pc = pc; self.npc = npc;
                    let v = (wd & keep) | ((*rp.add(rt)) << (s * 8));
                    self.write32(ea & !3, v);
                    fast_mask = if self.irq_pending { 0xFFFFFFFF } else { 0x1FE00003 };
                }
                50 => {
                    let v = self.read32((*rp.add(rs)).wrapping_add(imm));
                    self.gte.write_data(rt, v);
                }
                58 => {
                    self.cur_pc = cur; self.pc = pc; self.npc = npc;
                    let v = self.gte.read_data(rt);
                    self.write32((*rp.add(rs)).wrapping_add(imm), v);
                    fast_mask = if self.irq_pending { 0xFFFFFFFF } else { 0x1FE00003 };
                }
                _ => {
                    if op == 0 { return self.fail(pc, npc, limit - budget, &format!("unknown special {:02X} at 0x", i & 63), cur); }
                    return self.fail(pc, npc, limit - budget, &format!("unknown opcode {} at 0x", op), cur);
                }
            }
            (*rp.add(0)) = 0;
        }
        }
    }

    // The rare side of an instruction fetch (pc, npc and steps are in the fields): an interrupt, a host hook, a jump to a
    // library entry point, or a fetch that is not an aligned word of memory. Leaves 0 in slow_state when the word returned
    // is to be executed, 1 when the machine state has changed and the loop should start over, 2 when run is to return true
    // and 3 when it is to return false.
    fn slow_fetch(&mut self) -> u32 {
        self.slow_state = 0;
        if self.irq_pending && !self.in_irq && {
            let go = self.irq_delay <= 0;
            self.irq_delay -= 1;
            go
        } && !self.critical {
            // an interrupt between two instructions: the handler runs with the state put back afterwards
            self.irq_pending = false;
            self.in_irq = true;
            let saved = self.r;
            let (spc, snext, scur, shi, slo) = (self.pc, self.npc, self.cur_pc, self.hi, self.lo);
            self.irq_handler();
            self.r = saved;
            self.pc = spc;
            self.npc = snext;
            self.cur_pc = scur;
            self.hi = shi;
            self.lo = slo;
            self.in_irq = false;
            self.slow_state = if self.stop_reason.is_some() { 3 } else { 1 };
            return 0;
        }
        let pc = self.pc;
        let idx = ((pc & 0x1FFFFF) >> 2) as usize;
        let h = self.hook[idx];
        let handled = match h {
            H::None => false,
            H::Native(id) => { self.in_service = true; let r = self.service(id); self.in_service = false; r }
            other => { self.run_hook(other); true }
        };
        if handled {
            if self.stop_reason.is_some() { self.slow_state = 3; return 0; }
            if self.return_default { self.return_default = false; }
            else {
                self.pc = self.r[31];
                self.npc = self.pc.wrapping_add(4);
            }
            self.slow_state = if self.yield_ { 2 } else { 1 };
            return 0;
        }
        if pc == 0xA0 || pc == 0xB0 || pc == 0xC0 {
            self.r[2] = 0;
            self.pc = self.r[31];
            self.npc = self.pc.wrapping_add(4);
            self.slow_state = 1;
            return 0;
        }
        self.read32(pc)
    }

    // ---- loading the program ----

    fn load_program(&mut self, exe: &[u8]) -> Result<(), String> {
        if exe.len() < 0x900 { return Err("the program file could not be read".into()); }
        let load = u32::from_le_bytes([exe[0x18], exe[0x19], exe[0x1A], exe[0x1B]]);
        let size = u32::from_le_bytes([exe[0x1C], exe[0x1D], exe[0x1E], exe[0x1F]]) as usize;
        let end = (0x800 + size).min(exe.len());
        self.mem.load(load, &exe[0x800..end]);
        self.exe_words = exe[0x800..end].chunks(4).map(|c| {
            let mut b = [0u8; 4];
            b[..c.len()].copy_from_slice(c);
            u32::from_le_bytes(b)
        }).collect();
        self.r[28] = GP;
        self.r[29] = 0x801FFF00;
        let mut table = vec![0u8; 0x8000];
        let mut i = 0;
        while i < self.archive.entries.len() && (i + 1) * ENTRY_SIZE <= table.len() {
            let e = self.archive.entries[i];
            let o = i * ENTRY_SIZE;
            table[o] = e.lba as u8;
            table[o + 1] = (e.lba >> 8) as u8;
            table[o + 2] = (e.lba >> 16) as u8;
            let sz = e.size as u32;
            table[o + 3] = sz as u8;
            table[o + 4] = (sz >> 8) as u8;
            table[o + 5] = (sz >> 16) as u8;
            table[o + 6] = (sz >> 24) as u8;
            i += 1;
        }
        let mut sections = vec![0u8; HEADER_BYTES];
        for (i, s) in self.archive.sections.iter().enumerate() {
            if i * 2 + 1 < sections.len() {
                sections[i * 2] = *s as u8;
                sections[i * 2 + 1] = (*s >> 8) as u8;
            }
        }
        self.mem.load(0x80010004, &table);
        self.mem.load(0x80018004, &sections);
        self.mem.write32(0x8004FDF0, 0x80010004);
        self.mem.write32(0x8004FDF4, 0x80018004);
        self.mem.write8(0x8004FE48, 0);
        self.mem.write32(0x8004FE14, 0);
        self.mem.write32(0x8004FE4C, 0xFFFFFFFF);
        // what the library's reset leaves (its drawing limits)
        self.mem.write8(0x800568D0, 0);
        self.mem.write8(0x800568D1, 1);
        let a = self.mem.read32(0x80056950) as u16;
        self.mem.write16(0x800568D4, a);
        let b = self.mem.read32(0x80056964) as u16;
        self.mem.write16(0x800568D6, b);
        let mut a = 0x800568E0u32;
        while a < 0x800568E0 + 0x5C { self.mem.write32(a, 0xFFFFFFFF); a += 4; }
        let mut a = 0x8005693Cu32;
        while a < 0x8005693C + 0x14 { self.mem.write32(a, 0xFFFFFFFF); a += 4; }
        self.mem.write32(0x8005698C, 0x00FFFFFF);
        Ok(())
    }

    fn install_program(&mut self) {
        // the kernel's calls (events, pad, exceptions) do nothing: the host is the kernel
        let bios = [0x80040454u32, 0x80040464, 0x80040474, 0x80040484, 0x80040494, 0x800404a4, 0x800404b4, 0x800404c4, 0x80040534, 0x80040544,
            0x80040554, 0x80040564, 0x80040574, 0x80040584, 0x80040594, 0x800405a4, 0x800405b4, 0x800405c4, 0x800405d4, 0x80040a8c, 0x80040a9c,
            0x80040aac, 0x80040abc, 0x80040acc, 0x80040adc, 0x80040e18, 0x800471a4, 0x8004b730, 0x8004bed8, 0x8004bef0, 0x8004bf00, 0x8004bf10,
            0x8004e564, 0x8004e784, 0x8004e850, 0x8004e860, 0x8004e870];
        for a in bios { self.set_hook(a, H::Zero); }
        self.set_hook(0x80028A60, H::Zero);
        self.set_hook(0x800404D4, H::EnterCritical);
        self.set_hook(0x800404E4, H::ExitCritical);
        // the sound driver, libspu and the movie module's decoder calls are native services (services.rs)
        for &(id, at, _) in services::SERVICES { self.set_hook(at, H::Native(id)); }
        // the decompiled functions of the program (XG_DEC=0 keeps the translated blocks)
        if std::env::var("XG_DEC").map(|v| v != "0").unwrap_or(true) {
            self.guest_mode = true;
            self.mem.state_take();
            if let Ok(w) = std::env::var("XG_WATCH") {
                if let Some((l, h)) = w.split_once(':') {
                    if let (Ok(l), Ok(h)) = (u32::from_str_radix(l, 16), u32::from_str_radix(h, 16)) {
                        crate::mem::WATCH_LO.store(l & 0x1FFFFFFF, std::sync::atomic::Ordering::Relaxed);
                        crate::mem::WATCH_HI.store(h & 0x1FFFFFFF, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            if std::env::var("XG_HEAP_LOG").is_ok() { xgdec::HEAP_LOG.store(true, std::sync::atomic::Ordering::Relaxed); }
            if std::env::var("XG_DEC_NESTED").is_ok() { services::NESTED.store(true, std::sync::atomic::Ordering::Relaxed); }
            if std::env::var("XG_DEC_SAVED").is_ok() { services::SAVED_CHECK.store(true, std::sync::atomic::Ordering::Relaxed); xgdec::SAVED_CHECK.store(true, std::sync::atomic::Ordering::Relaxed); }
            if std::env::var("XG_CALL_LOG").is_ok() { services::CALL_LOG.store(true, std::sync::atomic::Ordering::Relaxed); }
            if std::env::var("XG_ENT_WATCH").is_ok() { xgdec::WATCH.store(true, std::sync::atomic::Ordering::Relaxed); }
            if std::env::var("XG_ENT").map(|v| v == "0").unwrap_or(false) { xgdec::RELOC_ON.store(false, std::sync::atomic::Ordering::Relaxed); }
            let lo: usize = std::env::var("XG_DEC_LO").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
            let hi: usize = std::env::var("XG_DEC_HI").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
            xgdec::RANGE[0].store(lo, std::sync::atomic::Ordering::Relaxed);
            xgdec::RANGE[1].store(hi, std::sync::atomic::Ordering::Relaxed);
            let mut served: std::collections::HashSet<u32> = services::SERVICES.iter().map(|s| s.1).collect();
            if let Ok(v) = std::env::var("XG_DEC_SKIP") {
                for (k, t) in v.split(',').filter_map(|t| u32::from_str_radix(t.trim(), 16).ok()).enumerate().take(512) {
                    xgdec::SKIP[k].store(t, std::sync::atomic::Ordering::Relaxed);
                    served.insert(t);
                }
            }
            for i in 0..xgdec::count() {
                let e = match xgdec::entry(i) { Some(e) => e, None => break };
                if i < lo || i >= hi || served.contains(&e.0) { continue; }
                let list = self.dec_by_addr.entry(e.0).or_insert_with(Vec::new);
                if list.is_empty() { self.set_hook(e.0, H::Native(1000 + i as u16)); }
                self.dec_by_addr.get_mut(&e.0).unwrap().push(i);
            }
        }
        self.set_hook(0x8002954C, H::Ret(0xFFFFFFFF));
        // the graphics library: its function table's routines are the ones that poke the chip, so those answer here
        for a in [0x80046668u32, 0x80046588, 0x80045d44, 0x8004696c, 0x80046c58, 0x80046db4, 0x80046638] { self.set_hook(a, H::Zero); }
        self.set_hook(0x80046560, H::GpuReg);
        self.set_hook(0x8004659C, H::GpuExec);
        self.set_hook(0x80045D5C, H::ClearOtagR);
        self.set_hook(0x8004668C, H::GpuQueue);

        // two calls through pointers the console's kernel fills in: the host is the kernel
        self.set_hook(0x80040AEC, H::Zero);
        self.set_hook(0x80040B00, H::Zero);
        self.set_hook(0x80040B7C, H::Zero);
        self.set_hook(0x80028230, H::DiscStart);
        self.set_hook(SELECT_MODE, H::SelectMode);
        self.set_hook(0x80035CDC, H::PadQueue);
        self.observe(0x80019ACC, true);
        // the disc's busy check: the host's reads finish as they are asked for, so nothing is ever in flight
        self.set_hook(0x800286CC, H::Zero);
        // host-file mode: the program's own reader runs on the host's file calls
        self.mem.write32(0x8004FE48, HOST_NAMES);
        self.set_hook(0x8004C318, H::HostOpen);
        self.set_hook(0x8004C338, H::HostClose);
        self.set_hook(0x8004C348, H::HostSeek);
        self.set_hook(0x8004C458, H::HostRead);
        // the DMA ends' callbacks (FUN_8004B7A0 sets one per channel)
        self.set_hook(0x8004B7A0, H::DmaCallback);
        // the kernel's events and the memory card
        self.set_hook(0x80040474, H::OpenEvent);
        self.set_hook(0x80040484, H::Ret(1));
        self.set_hook(0x800404A4, H::Ret(1));
        self.set_hook(0x800404B4, H::Ret(1));
        self.set_hook(0x80040494, H::TestEvent);
        self.set_hook(0x80040E18, H::Deliver);
        self.set_hook(0x800404C4, H::UnDeliver);
        self.set_hook(0x8004E784, H::CardInfo);
        self.set_hook(0x80040534, H::CardOpen);
        self.set_hook(0x80040544, H::CardRead);
        self.set_hook(0x80040554, H::CardWrite);
        self.set_hook(0x80040564, H::CardClose);
        self.set_hook(0x80040574, H::CardFormat);
        self.set_hook(0x80040584, H::FirstFile);
        self.set_hook(0x80040594, H::NextFile);
        self.set_hook(0x800405A4, H::Rename);
        self.set_hook(0x800405C4, H::Ret(0xFFFFFFFF));
        self.set_hook(0x800405B4, H::Erase);
        self.set_hook(0x8004B54C, H::VSync);
        // the sound library runs as written, on the sound chip; the driver's start sets up its 240 Hz timer
        self.observe(0x80037B88, false);
        for a in [0x80040FE4u32, 0x8004111C, 0x80041248] { self.set_hook(a, H::Ret(1)); }
        // a three-instruction loop the run could start in, and the program's own start
        self.mem.write32(MAIN_LOOP, 0x0C000000 | ((0x80019ACC >> 2) & 0x3FFFFFF));
        self.mem.write32(MAIN_LOOP + 4, 0x24040000);
        self.mem.write32(MAIN_LOOP + 8, 0x08000000 | ((MAIN_LOOP >> 2) & 0x3FFFFFF));
        self.mem.write32(MAIN_LOOP + 12, 0);
        // the program's own start: the entry (FUN_80019524) clears the statics and goes on into main (FUN_80019578),
        // which sets everything up and selects the first mode (6, the logo movies) itself
        self.r[29] = 0x801FFF00;
        self.prepare(0x80019524, 0, 0, 0, 0);
    }

    /// Runs a look as the function at `address` starts, then the function itself (its first two instructions from a
    /// trampoline that jumps back past them).
    fn observe(&mut self, address: u32, mode_number: bool) {
        let t = self.next_trampoline;
        self.next_trampoline += 16;
        self.set_hook(address, if mode_number { H::ObsModeNumber { tramp: t, addr: address } } else { H::ObsSoundStart { tramp: t, addr: address } });
    }

    // ---- the hooks ----

    fn run_hook(&mut self, h: H) {
        match h {
            H::None | H::Native(_) => {}
            H::Zero => self.r[2] = 0,
            // the two syscall stubs (EnterCriticalSection / ExitCriticalSection): what the syscall instruction does
            H::EnterCritical => { self.r[2] = if self.critical { 0 } else { 1 }; self.critical = true; }
            H::ExitCritical => { self.critical = false; self.r[2] = 1; }
            H::Ret(v) => self.r[2] = v,
            H::GpuReg => {
                let v = self.r[4];
                let cmd = v >> 24;
                if cmd == 5 {
                    self.gpu.disp_x = (v & 0x3FF) as i32;
                    self.gpu.disp_y = ((v >> 10) & 0x1FF) as i32;
                } else if cmd == 7 {
                    self.gpu.line_top = (v & 0x3FF) as i32;
                    self.gpu.line_bottom = ((v >> 10) & 0x3FF) as i32;
                } else if cmd == 8 {
                    let w = [256, 320, 512, 640];
                    self.gpu.disp_w = if v & 0x40 != 0 { 368 } else { w[(v & 3) as usize] };
                    self.gpu.disp_h = if v & 4 != 0 { 480 } else { 240 };
                    self.gpu.display24 = v & 0x10 != 0;
                }
            }
            H::GpuExec => {
                let (a, n) = (self.r[4], self.r[5] as i32);
                if self.gpu.threaded { self.gpu.queue_z(&mut self.gte.zlog, self.gte.stamp); }
                self.gpu.execute(&self.mem, a, n);
                self.r[2] = 0;
            }
            H::ClearOtagR => {
                // every entry links to the one below it
                let p = self.r[4];
                let n = self.r[5] as i32;
                let mut i = n - 1;
                while i >= 1 {
                    self.mem.write32(p.wrapping_add((i * 4) as u32), p.wrapping_add(((i - 1) * 4) as u32) & 0xFFFFFF);
                    i -= 1;
                }
                if n > 0 { self.mem.write32(p, 0xFFFFFF); }
                self.r[2] = 0;
            }
            H::GpuQueue => {
                // the command queue: a0 names the routine, a1 its packet
                let (f, a1, a3) = (self.r[4], self.r[5], self.r[7]);
                match f {
                    0x800465EC => {
                        if self.gpu.threaded { self.gpu.queue_z(&mut self.gte.zlog, self.gte.stamp); }
                        self.gpu.draw_list(&self.mem, a1)
                    }
                    0x800460A0 => {
                        let (x, y, w, hh) = (self.mem.read16(a1) as i16 as i32, self.mem.read16(a1 + 2) as i16 as i32,
                            self.mem.read16(a1 + 4) as i32, self.mem.read16(a1 + 6) as i32);
                        self.gpu.load(&self.mem, x, y, w, hh, a3);
                    }
                    0x800462DC => {
                        let (x, y, w, hh) = (self.mem.read16(a1) as i16 as i32, self.mem.read16(a1 + 2) as i16 as i32,
                            self.mem.read16(a1 + 4) as i32, self.mem.read16(a1 + 6) as i32);
                        self.gpu.store(&mut self.mem, x, y, w, hh, a3);
                    }
                    0x80045E44 => {
                        let (x, y, w, hh) = (self.mem.read16(a1) as i16 as i32, self.mem.read16(a1 + 2) as i16 as i32,
                            self.mem.read16(a1 + 4) as i32, self.mem.read16(a1 + 6) as i32);
                        self.gpu.fill(x, y, w, hh, (a3 & 0xFF) as i32, ((a3 >> 8) & 0xFF) as i32, ((a3 >> 16) & 0xFF) as i32);
                    }
                    _ => {}
                }
                self.r[2] = 0;
            }
            H::DiscStart => {
                // the disc system's start: its resets, with the host-file mode in place of starting the drive
                for a in [0x8005A488u32, 0x8005A48C, 0x8005A490, 0x8005A494, 0x8005A498, 0x8005A49C, 0x8004FE14, 0x8004FDFC, 0x8004FDF8, 0x8004FE1C] {
                    self.mem.write32(a, 0);
                }
                self.mem.write32(0x8004FE4C, 0xFFFFFFFF);
                let (a4, a5) = (self.r[4], self.r[5]);
                self.mem.write32(0x8004FDF0, a4);
                self.mem.write32(0x8004FDF4, a5);
                self.mem.write32(0x8004FE48, HOST_NAMES);
            }
            H::SelectMode => {
                let mode = self.r[4] as i32;
                self.mode = mode;
                if self.modes.len() < 4096 { self.modes.push(format!("frame {}: mode {}", self.frames, mode)); }
                self.mem.write32(MODE_NUMBER, mode as u32);
                if mode != self.mem.read32(LOADED_MODE) as i32 {
                    if self.mem.read32(0x800592BC) != 0 { self.mem.write32(0x800592BC, 0); }
                    self.mem.write32(LOADED_MODE, 0xFFFFFFFF);
                }
            }
            H::PadQueue => {
                // the pad queue's reader: an empty poll in a loop that waits for a button ends the frame
                let count = self.mem.read32(0x8005937C);
                if count == 0 { self.r[2] = 0; return; }
                self.mem.write32(0x8005937C, count - 1);
                let at = self.mem.read32(0x80059384);
                let k = (at & 15) * 2;
                self.mem.write32(0x80059384, at.wrapping_add(1));
                let pairs = [(0x80059570u32, 0x8005A0FCu32), (0x80059574, 0x8005A11C), (0x8005948C, 0x8005A13C), (0x80059490, 0x8005A15C),
                    (0x800594A4, 0x8005A17C), (0x800594A8, 0x8005A19C)];
                for (dst, src) in pairs {
                    let v = self.mem.read16(src + k);
                    self.mem.write16(dst, v);
                }
                self.r[2] = count;
            }
            H::ObsModeNumber { tramp, addr } => {
                self.running = self.mem.read32(MODE_NUMBER) as i32;
                self.trampoline(tramp, addr);
            }
            H::ObsSoundStart { tramp, addr } => {
                self.sound_ready = true;
                self.trampoline(tramp, addr);
            }
            H::HostOpen => {
                let entry = (self.r[4].wrapping_sub(HOST_NAMES) / 0x40) as i32;
                if entry < 0 || entry as usize >= self.archive.entries.len() { self.r[2] = 0xFFFFFFFF; return; }
                let entry = entry as usize;
                let stream = self.archive.is_stream(entry);
                if !stream && !self.archive.can_read(entry) { self.r[2] = 0xFFFFFFFF; return; }
                let whole = self.mem.read8(0x80059F18) & 8 != 0;
                let data;
                if stream {
                    data = self.stream_file(entry, whole);
                } else {
                    if self.cached_entry != entry as i64 {
                        self.cached = self.archive.read(entry);
                        self.cached_entry = entry as i64;
                    }
                    data = self.cached.clone();
                }
                let hnd = self.next_handle;
                self.next_handle += 1;
                if stream { self.movies.want(entry as i64, 3, 2); }
                self.open.insert(hnd, HostFile { entry, data, position: 0, audio_done: 0, stream, whole });
                self.r[2] = hnd;
            }
            H::HostClose => {
                let h = self.r[4];
                self.open.remove(&h);
                self.r[2] = 0;
            }
            H::HostSeek => {
                let (hnd, off, whence) = (self.r[4], self.r[5] as i32 as i64, self.r[6] as i32);
                match self.open.get_mut(&hnd) {
                    None => self.r[2] = 0xFFFFFFFF,
                    Some(f) => {
                        f.position = if whence == 2 { f.data.len() as i64 + off } else if whence == 1 { f.position + off } else { off };
                        if f.position / 2336 < f.audio_done { f.audio_done = f.position / 2336; }
                        self.r[2] = f.position as u32;
                    }
                }
            }
            H::HostRead => {
                if self.tracing { self.trace_flush(); }
                let (hnd, len, buf) = (self.r[5], self.r[6] as i32, self.r[7]);
                let mut f = match self.open.remove(&hnd) {
                    None => { self.r[2] = 0xFFFFFFFF; return; }
                    Some(f) => f,
                };
                let avail = f.data.len() as i64 - f.position;
                let n = (len as i64).min(avail).max(0) as usize;
                if n > 0 {
                    let from = f.position as usize;
                    self.mem.load(buf, &f.data[from..from + n]);
                    if f.stream { self.movie_read(&mut f, from, n); self.movie_note(&f, from, n); }
                }
                f.position += n as i64;
                self.r[2] = n as u32;
                self.open.insert(hnd, f);
            }
            H::DmaCallback => {
                let ch = (self.r[4] & 7) as usize;
                self.r[2] = self.dma_callback[ch];
                self.dma_callback[ch] = self.r[5];
            }
            H::VSync => {
                let n = self.r[4] as i32;
                let now = self.mem.read32(VBLANKS);
                if n < 0 { self.r[2] = now; return; }
                if n == 1 { self.r[2] = 0; return; }
                if self.vsync_target == 0 {
                    let t = if n > 1 { self.mem.read32(LAST_VSYNC).wrapping_add(n as u32) } else { now.wrapping_add(1) };
                    self.vsync_target = (now.wrapping_add(1)).max(t);
                }
                if now < self.vsync_target && self.guest_mode {
                    // the program's own thread waits here for the next frame; the call goes on when it comes
                    loop {
                        self.frames += 1;
                        self.guest_wait_frame();
                        if self.finished { return; }
                        if self.mem.read32(VBLANKS) >= self.vsync_target { break; }
                    }
                    self.deliver_interrupts();
                } else if now < self.vsync_target {
                    // the frame ends here; the call goes on next frame
                    self.frames += 1;
                    self.yield_ = true;
                    self.return_default = true;
                    return;
                }
                let now = self.mem.read32(VBLANKS);
                self.vsync_target = 0;
                self.mem.write32(LAST_VSYNC, now);
                self.r[2] = 0;
            }
            H::OpenEvent => {
                self.event_class.push(self.r[4]);
                self.event_spec.push(self.r[5]);
                self.r[2] = 0xF1000000 | (self.event_class.len() as u32 - 1);
            }
            H::TestEvent => {
                let h = self.r[4];
                let slot = if (h & 0xFFFF0000) == 0xF1000000 && ((h & 0xFFFF) as usize) < self.event_class.len() { (h & 0xFFFF) as i32 } else { -1 };
                self.r[2] = if slot >= 0 && self.delivered.remove(&event_key(self.event_class[slot as usize], self.event_spec[slot as usize])) { 1 } else { 0 };
            }
            H::Deliver => {
                self.delivered.insert(event_key(self.r[4], self.r[5]));
                self.r[2] = 1;
            }
            H::UnDeliver => {
                self.delivered.remove(&event_key(self.r[4], self.r[5]));
                self.r[2] = 1;
            }
            H::CardInfo => {
                for spec in [4u32, 0x100, 0x2000, 0x8000] { self.delivered.remove(&event_key(SW_CARD, spec)); }
                let spec = if self.r[4] & 0x10 == 0 || self.card2.is_some() { 4u32 } else { 0x100 };
                self.delivered.insert(event_key(SW_CARD, spec));
                self.r[2] = 1;
            }
            H::CardOpen => {
                let text = self.text(self.r[4]);
                let mode = self.r[5];
                self.r[2] = 0xFFFFFFFF;
                let (slot, name) = match self.card_name(&text) { Some(x) => x, None => return };
                let first = {
                    let card = if slot == 0 { &mut self.card } else { self.card2.as_mut().unwrap() };
                    if mode & 0x200 != 0 { card.create(&name, (mode >> 16) as i32) } else { card.find(&name) }
                };
                if first < 0 { return; }
                let mut fd = 2u32;
                while self.card_files.contains_key(&fd) { fd += 1; }
                self.card_files.insert(fd, CardFile { first, position: 0, slot });
                self.r[2] = fd;
            }
            H::CardRead | H::CardWrite => {
                let write = h == H::CardWrite;
                let (fd, buf, n) = (self.r[4], self.r[5], self.r[6] as i32);
                let (first, position, slot) = match self.card_files.get(&fd) {
                    Some(f) => (f.first, f.position, f.slot),
                    None => { self.r[2] = 0xFFFFFFFF; return; }
                };
                let n = n.max(0) as usize;
                let mut data = vec![0u8; n];
                if write { for i in 0..n { data[i] = self.mem.read8(buf.wrapping_add(i as u32)); } }
                let done = {
                    let card = if slot == 0 { &mut self.card } else { self.card2.as_mut().unwrap() };
                    card.transfer(first, position, &mut data, n, write)
                };
                if !write { for i in 0..done { self.mem.write8(buf.wrapping_add(i as u32), data[i]); } }
                if let Some(f) = self.card_files.get_mut(&fd) { f.position += done; }
                self.r[2] = if done == n { n as u32 } else { 0xFFFFFFFF };
            }
            H::CardClose => {
                let fd = self.r[4];
                self.r[2] = if self.card_files.remove(&fd).is_some() { fd } else { 0xFFFFFFFF };
            }
            H::CardFormat => {
                let text = self.text(self.r[4]);
                match self.card_name(&text) {
                    None => self.r[2] = 0,
                    Some((slot, _)) => {
                        let card = if slot == 0 { &mut self.card } else { self.card2.as_mut().unwrap() };
                        card.format();
                        self.r[2] = 1;
                    }
                }
            }
            H::FirstFile => {
                let text = self.text(self.r[4]);
                match self.card_name(&text) {
                    None => { self.found = None; }
                    Some((slot, name)) => {
                        let card = if slot == 0 { &self.card } else { self.card2.as_ref().unwrap() };
                        self.found = Some(card.list(&name));
                        self.found_slot = slot;
                    }
                }
                self.found_next = 0;
                let at = self.r[5];
                self.r[2] = self.next_entry(at);
            }
            H::NextFile => {
                let at = self.r[4];
                self.r[2] = self.next_entry(at);
            }
            H::Rename => {
                let (a, b) = (self.text(self.r[4]), self.text(self.r[5]));
                let from = self.card_name(&a);
                let to = self.card_name(&b);
                self.r[2] = match (from, to) {
                    (Some((s1, f)), Some((s2, t))) if s1 == s2 => {
                        let card = if s1 == 0 { &mut self.card } else { self.card2.as_mut().unwrap() };
                        if card.rename(&f, &t) { 1 } else { 0 }
                    }
                    _ => 0,
                };
            }
            H::Erase => {
                let text = self.text(self.r[4]);
                self.r[2] = match self.card_name(&text) {
                    None => 0,
                    Some((slot, name)) => {
                        let card = if slot == 0 { &mut self.card } else { self.card2.as_mut().unwrap() };
                        if card.erase(&name) { 1 } else { 0 }
                    }
                };
            }
        }
    }

    // a hook that lets the original function run: its first two instructions from a trampoline that jumps back past them
    fn trampoline(&mut self, t: u32, address: u32) {
        let w0 = self.mem.read32(address);
        let w1 = self.mem.read32(address + 4);
        self.mem.write32(t, w0);
        self.mem.write32(t + 4, w1);
        self.mem.write32(t + 8, 0x08000000 | (((address + 8) >> 2) & 0x3FFFFFF));
        self.mem.write32(t + 12, 0);
        self.pc = t;
        self.npc = t + 4;
        self.return_default = true;
    }

    fn text(&self, at: u32) -> String {
        let mut s = String::new();
        for i in 0..64u32 {
            let b = self.mem.read8(at.wrapping_add(i));
            if b == 0 { break; }
            s.push(b as char);
        }
        s
    }

    /// A card name "bu00:FILE" or "bu10:FILE": the slot it names when that card is in, and the file part.
    fn card_name(&self, name: &str) -> Option<(u8, String)> {
        if let Some(rest) = name.strip_prefix("bu00:") { return Some((0, rest.to_string())); }
        if let Some(rest) = name.strip_prefix("bu10:") {
            if self.card2.is_some() { return Some((1, rest.to_string())); }
        }
        None
    }

    // the next found file into the DIRENTRY at the address, or 0
    fn next_entry(&mut self, at: u32) -> u32 {
        let name = match &self.found {
            Some(list) if self.found_next < list.len() => list[self.found_next].clone(),
            _ => return 0,
        };
        self.found_next += 1;
        let (first, size) = {
            let card = if self.found_slot == 0 { &self.card } else { self.card2.as_ref().unwrap() };
            let first = card.find(&name);
            (first, card.file_size(first))
        };
        let nb = name.as_bytes();
        for i in 0..20u32 {
            let b = if (i as usize) < nb.len() { nb[i as usize] } else { 0 };
            self.mem.write8(at + i, b);
        }
        self.mem.write32(at + 20, 0x50);
        self.mem.write32(at + 24, size as u32);
        self.mem.write32(at + 28, 0);
        self.mem.write32(at + 32, first as u32);
        self.mem.write32(at + 36, 0);
        at
    }

    // ---- movies ----

    // A movie as the development setup kept it: with the stream mode's bit 3, 2336-byte sectors with an 8-byte header whose
    // first byte is 1 for an audio sector, the 2048 bytes of user data and the last 280 of the sector; without it the video
    // sectors' 2048 bytes only. (The audio is the host's to play.)
    fn stream_file(&mut self, entry: usize, whole: bool) -> Vec<u8> {
        if self.stream_entry == entry as i64 && self.stream_whole == whole { return self.stream.clone(); }
        if let Some(o) = self.stream_from_video(entry, whole) {
            self.stream = o;
            self.stream_entry = entry as i64;
            self.stream_whole = whole;
            return self.stream.clone();
        }
        let raw = match self.archive.read_raw_sectors(entry) { Some(r) => r, None => return Vec::new() };
        let count = raw.len() / 2352;
        let mut o: Vec<u8> = Vec::new();
        for i in 0..count {
            let s = i * 2352;
            let audio = raw[s + 18] & 0x04 != 0;
            let video = !audio && u16::from_le_bytes([raw[s + 24 + 2], raw[s + 24 + 3]]) == 0x8001;
            if whole {
                let mut head = [0u8; 8];
                head[0] = if audio { 1 } else { 0 };
                o.extend_from_slice(&head);
                o.extend_from_slice(&raw[s + 24..s + 24 + 2328]);
            } else if video {
                o.extend_from_slice(&raw[s + 24..s + 24 + 2048]);
            }
        }
        self.stream = o;
        self.stream_entry = entry as i64;
        self.stream_whole = whole;
        if whole { self.movie_audio(&raw, count); }
        self.stream.clone()
    }

    // A movie kept as an ordinary video file (Movies/NNNN.avi) and its Movies/NNNN.sync (the order of the program's own sectors):
    // the program is given the sectors it would have read, with the same rhythm and the same frame numbers, holding a picture
    // that is blank; the pictures shown are the video file's, and the sound is its sound.
    fn stream_from_video(&mut self, entry: usize, whole: bool) -> Option<Vec<u8>> {
        if std::env::var("XG_NO_VIDEO").is_ok() { return None; }
        let Store::Dir(game_dir) = &self.archive.store;
        let dir = game_dir.join("movies");
        let sync = std::fs::read(dir.join(format!("{:04}.sync", entry))).ok()?;
        if sync.len() < 12 || &sync[0..4] != b"XGS1" { return None; }
        let n = u32::from_le_bytes([sync[4], sync[5], sync[6], sync[7]]) as usize;
        if sync.len() < 12 + n { return None; }
        let types = &sync[12..12 + n];
        let vid = std::fs::read(dir.join(format!("{:04}.vid", entry))).ok()?;
        let mut o: Vec<u8> = Vec::new();
        let mut v = 0usize;
        for i in 0..n {
            let t = types[i];
            let payload: &[u8] = if t == 1 { let p = &vid[v * 2048..(v + 1) * 2048]; v += 1; p } else { &[] };
            if whole {
                let mut head = [0u8; 8];
                head[0] = if t == 0 { 1 } else { 0 };
                o.extend_from_slice(&head);
                o.extend_from_slice(payload);
                o.resize(o.len() + 2328 - payload.len(), 0);
            } else if t == 1 {
                o.extend_from_slice(payload);
            }
        }
        if whole {
            // the sound: the video file's, to 44100 Hz
            let (pcm, rate, ch) = crate::movie::avi_audio(&dir.join(format!("{:04}.avi", entry)))?;
            let mut at = vec![-1i64; n + 1];
            let mut k = 0i64;
            for i in 0..n { if types[i] == 0 { at[i] = k; k += 1; } }
            self.movie_audio_pcm(&pcm, rate as i64, ch as usize, &at, k as usize);
        }
        Some(o)
    }

    fn movie_audio(&mut self, raw: &[u8], count: usize) {
        let mut sectors: Vec<Vec<u8>> = Vec::new();
        let mut at = vec![-1i64; count + 1];
        let mut coding = 0u8;
        for i in 0..count {
            let s = i * 2352;
            if raw[s + 18] & 0x04 == 0 { continue; }
            if sectors.is_empty() { coding = raw[s + 19]; }
            at[i] = sectors.len() as i64;
            sectors.push(raw[s + 24..s + 24 + 2324].to_vec());
        }
        if sectors.is_empty() {
            self.xa.clear();
            self.xa_at.clear();
            return;
        }
        let pcm = xa::decode(&sectors, coding);
        let ch = if xa::is_stereo(coding) { 2 } else { 1 };
        let rate = xa::sample_rate(coding) as i64;
        self.movie_audio_pcm(&pcm, rate, ch, &at, sectors.len());
    }

    // The movie's sound (interleaved samples at its own rate) put on the 44100 Hz timeline: `at` is each sector's number among the
    // sound sectors (-1 for the others), `sectors` how many there are.
    fn movie_audio_pcm(&mut self, pcm: &[i16], rate: i64, ch: usize, at: &[i64], sectors: usize) {
        let count = at.len() - 1;
        let stereo = ch == 2;
        let frames = pcm.len() / ch;
        if frames == 0 || sectors == 0 { self.xa.clear(); self.xa_at.clear(); return; }
        let per_sector = (frames / sectors) as i64;
        // to 44100 Hz, linear between the source's samples
        let sr = SOUND_RATE as i64;
        let out_frames = (frames as i64 * sr / rate) as usize;
        let mut x = vec![0i16; out_frames * 2];
        for i in 0..out_frames {
            let p = i as f64 * rate as f64 / SOUND_RATE as f64;
            let k = p as usize;
            let t = (p - k as f64) as f32;
            let k1 = (k + 1).min(frames - 1);
            let l = pcm[k * ch] as f32 + (pcm[k1 * ch] as f32 - pcm[k * ch] as f32) * t;
            let r = if stereo { pcm[k * 2 + 1] as f32 + (pcm[k1 * 2 + 1] as f32 - pcm[k * 2 + 1] as f32) * t } else { l };
            x[i * 2] = l as i16;
            x[i * 2 + 1] = r as i16;
        }
        let mut xa_at = vec![-1i64; count + 1];
        for i in 0..count {
            xa_at[i] = if at[i] < 0 { -1 } else { at[i] * per_sector * sr / rate };
        }
        xa_at[count] = -1;
        if std::env::var("XG_MOVIE_TRACE").is_ok() { eprintln!("movie sound: {} frames at 44100", x.len() / 2); }
        self.xa = x;
        self.xa_at = xa_at;
        self.cd_read = 0;
        self.cd_count = 0;
    }

    // Which frame of the movie the program has read up to (the frame number in the video sectors' own headers).
    fn movie_note(&mut self, f: &HostFile, from: usize, n: usize) {
        if !self.movies.active() { return; }
        let (stride, hdr) = if f.whole { (2336usize, 8usize) } else { (2048usize, 0usize) };
        let last = (from + n - 1) / stride;
        for s in from / stride..=last {
            let b = s * stride + hdr;
            if b + 12 > f.data.len() { break; }
            if u16::from_le_bytes([f.data[b + 2], f.data[b + 3]]) != 0x8001 { continue; }
            let number = u32::from_le_bytes([f.data[b + 8], f.data[b + 9], f.data[b + 10], f.data[b + 11]]);
            if std::env::var("XG_MOVIE_DEBUG").is_ok() && self.movie_at.0 != f.entry as i64 { eprintln!("movie entry {} frame {} at machine frame {}", f.entry, number, self.frames); }
            if std::env::var("XG_MOVIE_TRACE").is_ok() && number % 10 == 0 && self.movie_at.1 != number { eprintln!("trace entry {} number {} machine frame {}", f.entry, number, self.frames); }
            self.movie_at = (f.entry as i64, number, self.frames);
            self.movies.want(f.entry as i64, number, 2);
        }
    }

    /// For the pacing test (XG_MOVIE_BENCH): asks for the frame that would be shown now and counts whether it was ready
    /// (stats: not a movie, exact frame, an earlier one, nothing).
    pub fn movie_probe(&mut self) {
        let (entry, number, at) = self.movie_at;
        if !self.movies.active() || entry < 0 || self.frames.saturating_sub(at) > 30 { self.movie_stats[0] += 1; return; }
        let want = number.saturating_sub(2);
        let got = self.movies.frame(entry, want);
        let k = if self.movies.ready(entry, want) { 1 } else if got.is_some() { 2 } else { 3 };
        self.movie_stats[k] += 1;
    }

    /// Puts the upscaled picture of the movie frame that is on the display in place of the chip's own (the picture is w x h,
    /// the display's size times the picture factor).
    pub fn apply_movie(&mut self, px: &mut Vec<u32>, w: usize, h: usize) {
        if !self.movies.active() || !self.gpu.display24 { return; }
        let (entry, number, at) = self.movie_at;
        if entry < 0 || self.frames.saturating_sub(at) > 30 || self.movies.lacks(entry) { return; }
        let lag: u32 = std::env::var("XG_MOVIE_LAG").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
        let fr = match self.movies.frame(entry, number.saturating_sub(lag)) { Some(f) => f, None => return };
        let (dw, dh) = (self.gpu.disp_w.max(1) as usize, self.gpu.disp_h.max(1) as usize);
        if w == 0 || h == 0 || px.len() < w * h { return; }
        let s = (w / dw).max(1);
        let vs = if dh > 240 { 2 } else { 1 };
        let first = ((self.gpu.line_top - 16) * vs).max(0) as usize;
        let last = (((self.gpu.line_bottom - 16) * vs).max(0) as usize).min(dh);
        if std::env::var("XG_MOVIE_DEBUG").is_ok() && self.frames % 200 == 0 { eprintln!("movie {} frame {}: picture {}x{}, display {}x{} lines {}..{}, frame {}x{}, rows {}..{}", entry, number, w, h, dw, dh, self.gpu.line_top, self.gpu.line_bottom, fr.w, fr.h, first, last); }
        if last <= first { return; }
        let (y0, y1) = (first * s, (last * s).min(h));
        let rh = y1 - y0;
        // the display shows the movie's own rows from the top; the upscaled frame is `up` times the chip's picture
        let up = fr.w as f32 / dw as f32;
        let src_h = ((last - first) as f32 * up).min(fr.h as f32);
        // (a freed frame's address can be handed out again, so a few of its bytes are part of the key)
        let sig = fr.rgb.iter().step_by(1999).fold(0usize, |a, &b| a.wrapping_mul(31).wrapping_add(b as usize));
        let key = (Arc::as_ptr(&fr) as usize ^ sig.rotate_left(17), w, y0, rh);
        if self.movie_cache.0 != key {
            let t_compose = std::time::Instant::now();
            // columns once: the two source columns and the weight (8 bits) of each picture column
            let xs: Vec<(usize, usize, u32)> = (0..w).map(|x| {
                let fx = ((x as f32 + 0.5) * fr.w as f32 / w as f32 - 0.5).clamp(0.0, fr.w as f32 - 1.0);
                let i0 = fx.floor() as usize;
                (i0, (i0 + 1).min(fr.w - 1), ((fx - i0 as f32) * 256.0) as u32)
            }).collect();
            let mut out = vec![0u32; w * rh];
            for y in 0..rh {
                let fy = ((y as f32 + 0.5) * src_h / rh as f32 - 0.5).clamp(0.0, fr.h as f32 - 1.0);
                let j0 = fy.floor() as usize;
                let j1 = (j0 + 1).min(fr.h - 1);
                let ty = ((fy - j0 as f32) * 256.0) as u32;
                let (r0, r1) = (&fr.rgb[j0 * fr.w * 3..(j0 + 1) * fr.w * 3], &fr.rgb[j1 * fr.w * 3..(j1 + 1) * fr.w * 3]);
                let row = &mut out[y * w..(y + 1) * w];
                for (x, &(i0, i1, tx)) in xs.iter().enumerate() {
                    let mut c = 0u32;
                    for k in 0..3 {
                        let top = r0[i0 * 3 + k] as u32 * (256 - tx) + r0[i1 * 3 + k] as u32 * tx;
                        let bot = r1[i0 * 3 + k] as u32 * (256 - tx) + r1[i1 * 3 + k] as u32 * tx;
                        let v = (top * (256 - ty) + bot * ty + 32768) >> 16;
                        c = c << 8 | v.min(255);
                    }
                    row[x] = c;
                }
            }
            self.movie_cache = (key, out);
            if std::env::var("XG_MOVIE_DEBUG").is_ok() && number % 5 == 0 { eprintln!("composed movie {} frame {} into {}x{} in {:?}", entry, number, w, rh, t_compose.elapsed()); }
        }
        px[y0 * w..(y0 + rh) * w].copy_from_slice(&self.movie_cache.1);
    }

    // The program read [from, from + length) of a movie: the audio sectors passed go to the CD input.
    fn movie_read(&mut self, f: &mut HostFile, from: usize, length: usize) {
        const SECTOR: usize = 2336;
        if self.xa.is_empty() || self.xa_at.is_empty() || !f.whole { return; }
        let first = (from / SECTOR).max(f.audio_done as usize);
        let last = (from + length - 1) / SECTOR;
        let cap = self.cd.len() / 2;
        let mut s = first;
        while s <= last && s + 1 < self.xa_at.len() {
            if self.xa_at[s] >= 0 {
                let mut end = (self.xa.len() / 2) as i64;
                for n in s + 1..self.xa_at.len() - 1 {
                    if self.xa_at[n] >= 0 { end = self.xa_at[n]; break; }
                }
                let mut i = self.xa_at[s];
                while i < end {
                    let w = (self.cd_read + self.cd_count) % cap;
                    self.cd[w * 2] = self.xa[i as usize * 2];
                    self.cd[w * 2 + 1] = self.xa[i as usize * 2 + 1];
                    if self.cd_count < cap { self.cd_count += 1; } else { self.cd_read = (self.cd_read + 1) % cap; }
                    i += 1;
                }
            }
            s += 1;
        }
        f.audio_done = f.audio_done.max(last as i64 + 1);
    }

    fn cd_input(&mut self, frames: usize) -> Vec<i16> {
        let mut out = vec![0i16; frames * 2];
        let cap = self.cd.len() / 2;
        for i in 0..frames {
            if self.cd_count == 0 { continue; }
            out[i * 2] = self.cd[self.cd_read * 2];
            out[i * 2 + 1] = self.cd[self.cd_read * 2 + 1];
            self.cd_read = (self.cd_read + 1) % cap;
            self.cd_count -= 1;
        }
        out
    }

    // ---- the interrupt, the sound, the frame ----

    fn irq_handler(&mut self) {
        self.r[29] = 0x8000EFF0; // the kernel's own stack (the low 64 KB the program never loads into)
        for ch in 0..8 {
            if self.dma_pending & (1 << ch) == 0 { continue; }
            self.dma_pending &= !(1 << ch);
            let cb = self.dma_callback[ch];
            if cb != 0 && !self.call(cb, 0, 0, 0, 0) {
                self.error = Some(format!("DMA {} callback: {}", ch, self.stop_reason.clone().unwrap_or_default()));
            }
        }
        // the sound timer: four ticks a vertical blank, a quarter of the frame's sound played after each
        while self.ticks_due > 0 {
            self.ticks_due -= 1;
            if self.sound_ready && !self.call(0x8003C020, 0, 0, 0, 0) {
                self.error = Some(format!("sound timer: {} frame {}", self.stop_reason.clone().unwrap_or_default(), self.frames));
                self.sound_ready = false;
            }
            let q = (self.tick & 3) as usize;
            self.tick += 1;
            let n = SAMPLES_PER_FRAME * (q + 1) / 4 - SAMPLES_PER_FRAME * q / 4;
            self.play_sound(n);
        }
        self.critical = false;
    }

    fn play_sound(&mut self, frames: usize) {
        if self.mix.len() < frames * 2 { self.mix.resize(frames * 2, 0); }
        let cd = if self.spu.control() & 1 != 0 { Some(self.cd_input(frames)) } else { None };
        let mut mix = std::mem::take(&mut self.mix);
        self.spu.render(&mut mix, 0, frames, cd.as_deref());
        let cap = self.sound.len() / 2;
        for i in 0..frames {
            let at = (self.sound_write + i) % cap;
            self.sound[at * 2] = mix[i * 2];
            self.sound[at * 2 + 1] = mix[i * 2 + 1];
        }
        self.mix = mix;
        self.sound_write = (self.sound_write + frames) % cap;
        if (self.sound_write + cap - self.sound_read) % cap < frames { self.sound_read = (self.sound_write + 1) % cap; }
    }

    /// Copies the sound made since the last call into `to` (interleaved stereo); the stereo frames it gave.
    pub fn take_sound(&mut self, to: &mut [i16]) -> usize {
        let cap = self.sound.len() / 2;
        let mut n = 0;
        while self.sound_read != self.sound_write && n * 2 + 1 < to.len() {
            to[n * 2] = self.sound[self.sound_read * 2];
            to[n * 2 + 1] = self.sound[self.sound_read * 2 + 1];
            self.sound_read = (self.sound_read + 1) % cap;
            n += 1;
        }
        n
    }

    fn vblank(&mut self) {
        // the pad's two port buffers as the pad library leaves them: status 0, the id byte, then the buttons inverted
        let p = !self.pad & 0xFFFF;
        self.mem.write8(PAD_PORT0, 0);
        self.mem.write8(PAD_PORT0 + 1, 0x41);
        self.mem.write8(PAD_PORT0 + 2, p as u8);
        self.mem.write8(PAD_PORT0 + 3, (p >> 8) as u8);
        self.mem.write8(PAD_PORT1, 0xFF);
        let saved = self.r;
        let (pc, next, hi, lo) = (self.pc, self.npc, self.hi, self.lo);
        self.r[29] = VBLANK_STACK;
        self.call(VBLANK_HANDLER, 0, 0, 0, 0);
        self.r = saved;
        self.pc = pc;
        self.npc = next;
        self.hi = hi;
        self.lo = lo;
    }

    /// Runs the program to the end of its next frame (the pad as set in `pad` is what the frame's vertical blank sees).
    /// False when the program is over or the machine stopped.
    /// True while a scene is on that is drawn in 3D all over the picture (battle 2, world map 3, arena 4) and the wide picture is
    /// asked for (XG_WIDE=1 asks for it at every scene, to look at what it does)
    pub fn wide_scene(&self) -> bool {
        self.wide_opt && (matches!(self.mode, 2 | 3 | 4) || std::env::var("XG_WIDE").map(|v| v == "1").unwrap_or(false))
    }

    pub fn run_frame(&mut self) -> bool {
        self.gte.wide = if self.wide_scene() { 192 } else { 0 };
        self.gpu.hud_squeeze = self.gte.wide != 0;
        self.gpu.wide_scene = self.wide_scene();
        if std::env::var("XG_WIDE_DEBUG").is_ok() && self.frames % 30 == 0 { eprintln!("frame {} mode {} wide {} area {:?} off {:?} disp {:?}", self.frames, self.mode, self.wide_scene(), self.gpu.debug_area(), self.gpu.debug_off(), self.gpu.debug_disp()); }
        // on the title menu the picture entries can take the buttons
        let raw = self.pad;
        self.pad = self.gpu.title_pad(raw);
        let ok = self.run_frame_inner();
        self.pad = raw;
        ok
    }

    fn run_frame_inner(&mut self) -> bool {
        if self.finished { return false; }
        self.native_check();
        self.gte.stamp = (self.frames & 0xFFFF) as u32;
        self.gpu.zstamp = self.gte.stamp;
        self.gte.log_z = self.gpu.threaded;
        // a frame is one vertical blank: the library's counter goes up as its interrupt would, before the handlers run
        let v = self.mem.read32(VBLANKS);
        self.mem.write32(VBLANKS, v.wrapping_add(1));
        self.ticks_due = (self.ticks_due + 4).min(8);
        self.irq_pending = true;
        if self.started { self.vblank(); }
        self.started = true;
        if self.guest_mode {
            return self.guest_frame();
        }
        let ok = self.run(FRAME_STEPS);
        if !ok {
            let budget = self.stop_reason.as_deref().map(|s| s.starts_with("step budget")).unwrap_or(false);
            if budget {
                // a frame's time is up: the vertical blank comes in the middle of whatever the program is doing
                self.stop_reason = None;
                self.frames += 1;
                return !self.finished;
            }
            self.error = Some(format!("stopped: {} at 0x{:08X}", self.stop_reason.clone().unwrap_or_default(), self.pc));
            self.finished = true;
            return false;
        }
        !self.finished
    }

    // ---- the program on a thread of its own ----

    /// The pending interrupt, if it is due (what the interpreter does between two instructions).
    pub(super) fn deliver_interrupts(&mut self) {
        if self.guest_fuel < 0 && self.guest_mode {
            // the frame's time is up: the vertical blank comes in the middle of whatever the program is doing
            self.frames += 1;
            self.guest_wait_frame();
        }
        // the delay of an interrupt is counted in instructions: here, the ones the blocks passed since the last look
        let used = (self.guest_fuel_seen - self.guest_fuel).max(0);
        self.guest_fuel_seen = self.guest_fuel;
        if self.irq_pending && !self.in_irq && {
            let go = self.irq_delay <= 0;
            self.irq_delay -= used.max(1);
            go
        } && !self.critical {
            self.irq_pending = false;
            self.in_irq = true;
            let saved = self.r;
            let (spc, snext, scur, shi, slo) = (self.pc, self.npc, self.cur_pc, self.hi, self.lo);
            self.irq_handler();
            self.r = saved;
            self.pc = spc;
            self.npc = snext;
            self.cur_pc = scur;
            self.hi = shi;
            self.lo = slo;
            self.in_irq = false;
        }
    }

    /// On the program's thread: tell the host the frame is over and wait for the next one.
    fn guest_wait_frame(&mut self) {
        self.guest_fuel = GUEST_FUEL;
        self.guest_fuel_seen = GUEST_FUEL;
        // the next frame gets a whole frame's steps again (what a fresh run() would have given)
        let refill = |m: &mut Machine| { m.steps = m.run_limit - m.run_max; };
        if let Some(tx) = &self.guest_back { let _ = tx.send(0); }
        let ok = match &self.guest_go { Some(rx) => rx.recv().is_ok(), None => false };
        if !ok {
            self.finished = true;
            self.stop_reason = Some("host closed".to_string());
        } else {
            refill(self);
        }
    }

    /// The last function the decompiled program handed to the machine, how many such calls and interrupt polls there have been
    /// (a freeze report prints them).
    pub fn debug_state() -> (u32, u64, u64) {
        (services::DBG_LAST.load(std::sync::atomic::Ordering::Relaxed), services::DBG_CALLS.load(std::sync::atomic::Ordering::Relaxed),
         services::DBG_POLLS.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// On the host's thread: runs the program until it waits for the next vertical blank again.
    fn guest_frame(&mut self) -> bool {
        if self.guest_link.is_none() {
            if std::env::var("XG_DEC_DEBUG").is_ok() {
                std::thread::spawn(|| loop {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    eprintln!("watchdog: last guest call 0x{:08X}, calls {}, polls {}", services::DBG_LAST.load(std::sync::atomic::Ordering::Relaxed), services::DBG_CALLS.load(std::sync::atomic::Ordering::Relaxed), services::DBG_POLLS.load(std::sync::atomic::Ordering::Relaxed));
                });
            }
            let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
            let (back_tx, back_rx) = std::sync::mpsc::channel::<u8>();
            self.guest_go = Some(go_rx);
            self.guest_back = Some(back_tx);
            struct P(*mut Machine);
            unsafe impl Send for P {}
            let p = P(self as *mut Machine);
            let h = std::thread::Builder::new().name("program".into()).stack_size(256 << 20).spawn(move || {
                let p = p;
                let m: &mut Machine = unsafe { &mut *p.0 };
                if m.guest_go.as_ref().map(|rx| rx.recv().is_err()).unwrap_or(true) { return; }
                loop {
                    let ok = m.run(FRAME_STEPS);
                    if std::env::var("XG_DEC_DEBUG").is_ok() { eprintln!("program thread: run returned {} pc {:08X} stop {:?} frames {}", ok, m.pc, m.stop_reason, m.frames); }
                    if ok {
                        m.finished = true;
                        break;
                    }
                    let budget = m.stop_reason.as_deref().map(|s| s.starts_with("step budget")).unwrap_or(false);
                    if budget && !m.finished {
                        m.stop_reason = None;
                        m.frames += 1;
                        m.guest_wait_frame();
                        if m.finished { break; }
                        continue;
                    }
                    m.error = Some(format!("stopped: {} at 0x{:08X}", m.stop_reason.clone().unwrap_or_default(), m.pc));
                    if std::env::var("XG_DEC_DEBUG").is_ok() {
                        let pc = m.pc;
                        eprintln!("stopped at pc {:08X}: words {:08X} {:08X} {:08X} {:08X}; ra {:08X} sp {:08X}; last guest call {:08X}", pc,
                            m.mem.read32(pc), m.mem.read32(pc + 4), m.mem.read32(pc + 8), m.mem.read32(pc + 12), m.r[31], m.r[29],
                            services::DBG_LAST.load(std::sync::atomic::Ordering::Relaxed));
                    }
                    m.finished = true;
                    break;
                }
                if let Some(tx) = &m.guest_back { let _ = tx.send(1); }
            }).expect("program thread");
            self.guest_link = Some((go_tx, back_rx, h));
        }
        let (go, back) = match &self.guest_link { Some((g, b, _)) => (g.clone(), b), None => return false };
        if go.send(()).is_err() {
            self.finished = true;
            return false;
        }
        if back.recv().is_err() {
            self.error = Some("the program's thread ended unexpectedly".to_string());
            self.finished = true;
        }
        !self.finished
    }

    // ---- the machine at one moment ----

    /// The last calls decompiled code made into code that is not decompiled, oldest first (for a report when the program ends).
    pub fn call_ring(&self) -> Vec<String> {
        let r = services::RING.lock().unwrap();
        let n = r.1.min(64);
        (0..n).map(|k| { let e = r.0[(r.1 - n + k) % 64]; format!("frame {} mode {}: call 0x{:08X} from 0x{:08X}", e.2, e.3, e.0, e.1) }).collect()
    }

    pub fn save_state(&self) -> Vec<u8> {
        use crate::snap::W;
        let mut w = W::new();
        w.u32(0x58475231);
        for v in self.r { w.u32(v); }
        w.u32(self.pc); w.u32(self.npc); w.u32(self.hi); w.u32(self.lo); w.u32(self.cur_pc);
        if self.mem.state_on {
            let mut ram = self.mem.w.to_vec();
            let lo = (crate::mem::STATE_OLD >> 2) as usize;
            for i in 0..self.mem.state.len() { ram[lo + i] = self.mem.state[i]; }
            w.u32s(&ram);
        } else {
            w.u32s(&self.mem.w[..]);
        }
        w.bytes(&self.mem.scratch);
        self.gte.save_to(&mut w);
        self.gpu.save_to(&mut w);
        self.mdec.save_to(&mut w);
        self.spu.save_to(&mut w);
        w.i64(self.steps);
        w.bool(self.irq_pending); w.i32(self.irq_delay); w.bool(self.critical);
        w.bool(self.finished); w.i64(self.frames); w.bool(self.started); w.i32(self.mode); w.i32(self.running);
        w.u32(self.vsync_target); w.bool(self.sound_ready); w.i32(self.ticks_due); w.i32(self.tick);
        for v in self.dma_callback { w.u32(v); }
        w.i32(self.dma_pending);
        w.u32(self.next_handle);
        w.u32(self.open.len() as u32);
        for (h, f) in &self.open {
            w.u32(*h); w.u32(f.entry as u32); w.i64(f.position); w.i64(f.audio_done); w.bool(f.stream); w.bool(f.whole);
        }
        w.u32(self.event_class.len() as u32);
        for i in 0..self.event_class.len() { w.u32(self.event_class[i]); w.u32(self.event_spec[i]); }
        w.u32(self.delivered.len() as u32);
        for k in &self.delivered { w.u64(*k); }
        w.u32(self.card_files.len() as u32);
        for (fd, f) in &self.card_files { w.u32(*fd); w.i32(f.first); w.u32(f.position as u32); w.u8(f.slot); }
        w.bool(self.found.is_some());
        if let Some(list) = &self.found {
            w.u32(list.len() as u32);
            for n in list { w.string(n); }
        }
        w.u32(self.found_next as u32);
        w.u8(self.found_slot);
        // the entity table (outside the image): words, then count and a tag so an older file without it still loads
        for e in self.mem.ents.iter() { for &v in e.w.iter() { w.u32(v); } }
        for o in self.mem.objs.iter() { for &v in o.w.iter() { w.u32(v); } }
        w.u32(self.mem.obj_next as u32);
        for &b in self.mem.script[..self.mem.script_len].iter() { w.u8(b); }
        w.u32(self.mem.script_len as u32);
        w.u32((self.mem.ents.len() * crate::mem::ENT_WORDS) as u32);
        w.u32((self.mem.objs.len() * crate::mem::OBJ_WORDS) as u32);
        w.u32(0x33544E45);
        w.b
    }

    pub fn load_state(&mut self, data: &[u8]) -> bool {
        use crate::snap::R;
        let mut r = R::new(data);
        if r.u32() != 0x58475231 { return false; }
        for i in 0..32 { self.r[i] = r.u32(); }
        self.pc = r.u32(); self.npc = r.u32(); self.hi = r.u32(); self.lo = r.u32(); self.cur_pc = r.u32();
        let words = r.u32s();
        let was_on = self.mem.state_on;
        if words.len() == RAM_WORDS { self.mem.w.copy_from_slice(&words); self.mem.touch_all(); }
        if was_on { self.mem.state_take(); }
        let sc = r.bytes();
        if sc.len() == 0x400 { self.mem.scratch.copy_from_slice(&sc); }
        self.gte.load_from(&mut r);
        self.gpu.load_from(&mut r);
        self.mdec.load_from(&mut r);
        self.spu.load_from(&mut r);
        self.steps = r.i64();
        self.irq_pending = r.bool(); self.irq_delay = r.i32(); self.critical = r.bool();
        self.finished = r.bool(); self.frames = r.i64(); self.started = r.bool(); self.mode = r.i32(); self.running = r.i32();
        self.vsync_target = r.u32(); self.sound_ready = r.bool(); self.ticks_due = r.i32(); self.tick = r.i32();
        for i in 0..8 { self.dma_callback[i] = r.u32(); }
        self.dma_pending = r.i32();
        self.next_handle = r.u32();
        self.native_dirty = true;
        self.cached_entry = -1;
        self.stream_entry = -1;
        self.open.clear();
        let n = r.u32();
        for _ in 0..n {
            let h = r.u32();
            let entry = r.u32() as usize;
            let position = r.i64();
            let audio_done = r.i64();
            let stream = r.bool();
            let whole = r.bool();
            let data = if entry >= self.archive.entries.len() { Vec::new() }
                else if stream { self.stream_file(entry, whole) } else { self.archive.read(entry) };
            self.open.insert(h, HostFile { entry, data, position, audio_done, stream, whole });
        }
        self.event_class.clear();
        self.event_spec.clear();
        let n = r.u32();
        for _ in 0..n { self.event_class.push(r.u32()); self.event_spec.push(r.u32()); }
        self.delivered.clear();
        let n = r.u32();
        for _ in 0..n { self.delivered.insert(r.u64()); }
        self.card_files.clear();
        let n = r.u32();
        for _ in 0..n {
            let fd = r.u32();
            let first = r.i32();
            let position = r.u32() as usize;
            let slot = r.u8();
            self.card_files.insert(fd, CardFile { first, position, slot });
        }
        self.found = if r.bool() {
            let n = r.u32();
            Some((0..n).map(|_| r.string()).collect())
        } else { None };
        self.found_next = r.u32() as usize;
        self.found_slot = r.u8();
        // the entity table's trailer (absent in older files: the table is left empty and the image holds whatever it held)
        let n = data.len();
        for e in self.mem.ents.iter_mut() { e.w = [0; crate::mem::ENT_WORDS]; }
        for o in self.mem.objs.iter_mut() { o.w = [0; crate::mem::OBJ_WORDS]; }
        self.mem.obj_next = 0;
        for b in self.mem.script[..self.mem.script_len].iter_mut() { *b = 0; }
        self.mem.script_len = 0;
        let word = |at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        if n >= 20 && word(n - 4) == 0x33544E45 {
            let (ce, co, sl) = (word(n - 12) as usize, word(n - 8) as usize, word(n - 16) as usize);
            let body = (ce + co + 1) * 4 + sl;
            if ce == self.mem.ents.len() * crate::mem::ENT_WORDS && co == self.mem.objs.len() * crate::mem::OBJ_WORDS
                && sl <= self.mem.script.len() && n >= 16 + body {
                let start = n - 16 - body;
                for i in 0..ce { self.mem.ents[i / crate::mem::ENT_WORDS].w[i % crate::mem::ENT_WORDS] = word(start + 4 * i); }
                for i in 0..co { self.mem.objs[i / crate::mem::OBJ_WORDS].w[i % crate::mem::OBJ_WORDS] = word(start + 4 * (ce + i)); }
                self.mem.obj_next = word(start + 4 * (ce + co)) as usize;
                let at = start + 4 * (ce + co + 1);
                self.mem.script[..sl].copy_from_slice(&data[at..at + sl]);
                self.mem.script_len = sl;
            }
        } else if n >= 16 && word(n - 4) == 0x32544E45 {
            let (ce, co) = (word(n - 12) as usize, word(n - 8) as usize);
            let body = (ce + co + 1) * 4;
            if ce == self.mem.ents.len() * crate::mem::ENT_WORDS && co == self.mem.objs.len() * crate::mem::OBJ_WORDS && n >= 12 + body {
                let start = n - 12 - body;
                for i in 0..ce { self.mem.ents[i / crate::mem::ENT_WORDS].w[i % crate::mem::ENT_WORDS] = word(start + 4 * i); }
                for i in 0..co { self.mem.objs[i / crate::mem::OBJ_WORDS].w[i % crate::mem::OBJ_WORDS] = word(start + 4 * (ce + i)); }
                self.mem.obj_next = word(start + 4 * (ce + co)) as usize;
            }
        } else if n >= 8 && u32::from_le_bytes([data[n - 4], data[n - 3], data[n - 2], data[n - 1]]) == 0x31544E45 {
            let count = u32::from_le_bytes([data[n - 8], data[n - 7], data[n - 6], data[n - 5]]) as usize;
            if count == self.mem.ents.len() * crate::mem::ENT_WORDS && n >= 8 + count * 4 {
                let start = n - 8 - count * 4;
                for (i, c) in data[start..start + count * 4].chunks(4).enumerate() {
                    self.mem.ents[i / crate::mem::ENT_WORDS].w[i % crate::mem::ENT_WORDS] = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                }
            }
        }
        self.in_irq = false;
        self.return_default = false;
        self.slow_state = 0;
        self.yield_ = false;
        self.stop_reason = None;
        self.error = None;
        self.sound_write = 0;
        self.sound_read = 0;
        r.ok
    }

    /// Probe helpers: write or read memory as the program would, and send the program counter somewhere (a tail call).
    pub fn poke(&mut self, a: u32, v: u32, size: i32) {
        match size { 1 => self.mem.write8(a, v as u8), 2 => self.mem.write16(a, v as u16), _ => self.mem.write32(a, v) }
    }

    pub fn peek(&mut self, a: u32, size: i32) -> u32 {
        match size { 1 => self.read8(a) as u32, 2 => self.read16(a) as u32, _ => self.read32(a) }
    }

    pub fn jump(&mut self, addr: u32, arg: u32) {
        self.r[4] = arg;
        self.pc = addr;
        self.npc = addr.wrapping_add(4);
    }

    pub fn display_size(&self) -> (usize, usize) {
        let s = self.gpu.scale.max(1) as usize;
        let w = if self.gpu.wide_shown { crate::gpu::WIDE_W as usize } else { self.gpu.disp_w as usize };
        (w * s, self.gpu.disp_h as usize * s)
    }
}
