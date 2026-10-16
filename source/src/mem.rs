// Main memory (2 MB at 0x80000000, mirrored at 0x00000000 and 0xA0000000) and the 1 KB scratchpad at 0x1F800000.
// Other addresses read as zero and ignore writes here; the machine puts the device ports on top (see machine.rs).

pub const RAM_SIZE: u32 = 0x200000;
pub const RAM_WORDS: usize = 0x80000;

pub const FLAGS: usize = RAM_WORDS / 64;

/// The field engine's entity table lives outside the console image: guest address 0x90000000 (physical 0x10000000) onwards
/// is `ENT_RECORDS` records of 0x5C bytes each. The program is told this address as the table's base when it allocates the
/// table, and every access to it comes through here.
pub const ENT_BASE: u32 = 0x1000_0000;
pub const ENT_RECORDS: usize = 4096;
pub const ENT_WORDS: usize = 23;
pub const ENT_BYTES: u32 = (ENT_RECORDS * ENT_WORDS * 4) as u32;
pub const ENT_GUEST: u32 = 0x9000_0000;

/// The objects the entity records point to (0x138 bytes each, made one per entity when the program creates it): guest
/// address 0x91000000 (physical 0x11000000) onwards, a slot per object, handed out in order and given back all at once when
/// the next entity table is made.
pub const OBJ_BASE: u32 = 0x1100_0000;
pub const OBJ_SLOTS: usize = 2048;
/// a slot is the object (0x138 bytes) behind an 8-byte gap: the allocator's block header sits there in the image, and code
/// that locks or unlocks a block writes the word before the object
pub const OBJ_WORDS: usize = 80;
pub const OBJ_BYTES: u32 = (OBJ_SLOTS * OBJ_WORDS * 4) as u32;
pub const OBJ_GUEST: u32 = 0x9100_0000;

/// One object (0x138 bytes): what an entity is in the world and the script it runs.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ObjRec {
    pub w: [u32; OBJ_WORDS],
}

impl ObjRec {
    /// position in 16.16 field units (+0x20, +0x24, +0x28)
    pub fn x(&self) -> i32 { self.w[2 + 0x20 / 4] as i32 }
    pub fn y(&self) -> i32 { self.w[2 + 0x24 / 4] as i32 }
    pub fn z(&self) -> i32 { self.w[2 + 0x28 / 4] as i32 }
    /// the script's program counter (+0xCC, a halfword)
    pub fn pc(&self) -> u16 { (self.w[2 + 0xCC / 4] & 0xFFFF) as u16 }
}

/// The room's script section (the entity descriptors and the bytecode they run: the decompressed section the scene loader
/// puts in a heap block and points SCRIPT_CODE into) lives outside the image too, as bytes at guest address 0x93000000
/// (physical 0x13000000). The loader's allocation is replaced by this buffer; its decompressor writes it byte by byte.
pub const SCRIPT_BASE: u32 = 0x1300_0000;
pub const SCRIPT_BYTES: u32 = 0x10_0000;
pub const SCRIPT_GUEST: u32 = 0x9300_0000;

/// The game's state block (the party, its Gears, the story variables, the room the player is in...: 0x2358 bytes at
/// 0x8006D634 in the console image) lives outside the image as `state`, at guest address 0x92000000 (physical 0x12000000).
/// The decompiled code is generated with the block's addresses already moved; the image words that held it are filled with
/// STATE_POISON so that any access that missed is seen. Addresses of the old place that reach `Mem` (the host's own reads and
/// writes, the probes) are answered from `state` too. It is only in use while the decompiled program runs (`state_on`).
pub const STATE_OLD: u32 = xgblocks::STATE_LO;
pub const STATE_BYTES: u32 = xgblocks::STATE_LEN;
const _: () = assert!(xgdec::STATE_LO == STATE_OLD && xgdec::STATE_LEN == STATE_BYTES);
pub const STATE_BASE: u32 = 0x1200_0000;
pub const STATE_POISON: u32 = 0xA5A5_A5A5;

/// XG_WATCH=lo:hi (hex, physical): a 32-bit store into the range prints its value and the Rust stack that made it.
pub static WATCH_LO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
pub static WATCH_HI: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// One entity record (0x5C bytes). Named fields are the ones the findings pin down.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EntityRec {
    pub w: [u32; ENT_WORDS],
}

impl EntityRec {
    /// the object the entity stands for (its position is at +0x20/+0x24/+0x28 of that object)
    pub fn obj(&self) -> u32 { self.w[0x4C / 4] }
    /// the record's flag halfword (+0x58)
    pub fn flags(&self) -> u16 { (self.w[0x58 / 4] & 0xFFFF) as u16 }
}

pub struct Mem {
    pub w: Box<[u32; RAM_WORDS]>,
    // flag: one bit per word, set for the words the translated blocks stand for; gen: raised by every write to a flagged
    // word (a block is valid while gen is what it was when its words were compared); hit: a write reached a flagged word
    pub flag: Box<[u64; FLAGS]>,
    pub gen: u32,
    pub hit: bool,
    pub scratch: [u8; 0x400],
    pub ents: Vec<EntityRec>,
    pub ent_over: bool,
    pub objs: Vec<ObjRec>,
    pub obj_next: usize,
    pub obj_over: bool,
    pub state: Vec<u32>,
    pub state_on: bool,
    pub script: Vec<u8>,
    pub script_len: usize,
    pub script_over: bool,
}

impl Mem {
    pub fn new() -> Mem {
        Mem {
            w: vec![0u32; RAM_WORDS].into_boxed_slice().try_into().unwrap(),
            flag: vec![0u64; FLAGS].into_boxed_slice().try_into().unwrap(),
            gen: 0,
            hit: false,
            scratch: [0; 0x400],
            ents: vec![EntityRec { w: [0; ENT_WORDS] }; ENT_RECORDS],
            ent_over: false,
            objs: vec![ObjRec { w: [0; OBJ_WORDS] }; OBJ_SLOTS],
            obj_next: 0,
            obj_over: false,
            state: vec![0u32; (STATE_BYTES / 4) as usize],
            state_on: false,
            script: vec![0u8; SCRIPT_BYTES as usize],
            script_len: 0,
            script_over: false,
        }
    }

    #[inline]
    pub fn touch(&mut self, a: u32) {
        let wi = ((a & 0x1FFFFF) >> 2) as usize;
        if (self.flag[wi >> 6] >> (wi & 63)) & 1 != 0 { self.gen = self.gen.wrapping_add(1); self.hit = true; }
    }

    pub fn touch_all(&mut self) {
        self.gen = self.gen.wrapping_add(1);
    }

    pub fn load(&mut self, address: u32, data: &[u8]) {
        if !data.is_empty() {
            let mut a = address & 0x1FFFFF;
            let end = (a as usize + data.len()).min(RAM_SIZE as usize) as u32;
            while a < end { self.touch(a); a = (a | 3) + 1; }
        }
        let mut a = (address & 0x1FFFFF) as usize;
        for &b in data {
            if a >= RAM_SIZE as usize { break; }
            if self.state_on && (a as u32).wrapping_sub(STATE_OLD) < STATE_BYTES { self.st_write8_old(a as u32, b); a += 1; continue; }
            let s = (a & 3) * 8;
            let i = a >> 2;
            self.w[i] = (self.w[i] & !(0xFFu32 << s)) | ((b as u32) << s);
            a += 1;
        }
    }

    #[inline]
    fn ent_get(&self, wi: usize) -> u32 {
        self.ents[wi / ENT_WORDS].w[wi % ENT_WORDS]
    }

    #[inline]
    fn ent_set(&mut self, wi: usize, v: u32) {
        self.ents[wi / ENT_WORDS].w[wi % ENT_WORDS] = v;
    }

    #[inline]
    fn obj_get(&self, wi: usize) -> u32 {
        self.objs[wi / OBJ_WORDS].w[wi % OBJ_WORDS]
    }

    #[inline]
    fn obj_set(&mut self, wi: usize, v: u32) {
        self.objs[wi / OBJ_WORDS].w[wi % OBJ_WORDS] = v;
    }

    /// a fresh script buffer for the scene being loaded (the program is told its address)
    pub fn script_alloc(&mut self) -> u32 {
        for b in self.script[..self.script_len].iter_mut() { *b = 0; }
        self.script_len = 0;
        SCRIPT_GUEST + 8
    }

    /// the state block moves out of the image: its words are copied over and the image words poisoned
    pub fn state_take(&mut self) {
        let lo = (STATE_OLD >> 2) as usize;
        for i in 0..self.state.len() { self.state[i] = self.w[lo + i]; self.w[lo + i] = STATE_POISON; }
        self.state_on = true;
        self.state_flags();
        self.touch_all();
    }

    /// The words of the block are marked like the words translated code stands for: every store to them takes the slow path,
    /// where it is written to the state and (as the shadow translated code reads) to the image.
    pub fn state_flags(&mut self) {
        let lo = (STATE_OLD >> 2) as usize;
        for at in lo..lo + self.state.len() { self.flag[at >> 6] |= 1u64 << (at & 63); }
    }

    /// ... and back (the console image as the translated code sees it)
    pub fn state_give(&mut self) {
        let lo = (STATE_OLD >> 2) as usize;
        for i in 0..self.state.len() { self.w[lo + i] = self.state[i]; }
        self.state_on = false;
        self.touch_all();
    }

    fn st_read8_old(&self, a: u32) -> u8 {
        match self.state.get(((a.wrapping_sub(STATE_OLD)) >> 2) as usize) {
            Some(w) if a >= STATE_OLD => (*w >> ((a & 3) * 8)) as u8,
            _ => (self.w[((a & 0x1FFFFF) >> 2) as usize] >> ((a & 3) * 8)) as u8,
        }
    }

    fn st_write8_old(&mut self, a: u32, v: u8) {
        let wi = ((a - STATE_OLD) >> 2) as usize;
        let s = (a & 3) * 8;
        self.state[wi] = (self.state[wi] & !(0xFFu32 << s)) | ((v as u32) << s);
    }

    fn st_read8(&self, a: u32) -> u8 {
        (self.state[((a - STATE_BASE) >> 2) as usize] >> ((a & 3) * 8)) as u8
    }

    fn st_write8(&mut self, a: u32, v: u8) {
        let wi = ((a - STATE_BASE) >> 2) as usize;
        let s = (a & 3) * 8;
        self.state[wi] = (self.state[wi] & !(0xFFu32 << s)) | ((v as u32) << s);
    }

    fn obj_read8(&self, a: u32) -> u8 {
        (self.obj_get(((a - OBJ_BASE) >> 2) as usize) >> ((a & 3) * 8)) as u8
    }

    fn obj_write8(&mut self, a: u32, v: u8) {
        let wi = ((a - OBJ_BASE) >> 2) as usize;
        let s = (a & 3) * 8;
        let w = (self.obj_get(wi) & !(0xFFu32 << s)) | ((v as u32) << s);
        self.obj_set(wi, w);
    }

    /// a slot for a new object (the program is told its address); None when they are used up
    pub fn obj_alloc(&mut self) -> Option<u32> {
        if self.obj_next >= OBJ_SLOTS { self.obj_over = true; return None; }
        let n = self.obj_next;
        self.obj_next += 1;
        self.objs[n].w = [0; OBJ_WORDS];
        Some(OBJ_GUEST + (n * OBJ_WORDS * 4) as u32 + 8)
    }

    pub fn obj_reset(&mut self) {
        self.obj_next = 0;
    }

    fn ent_read8(&self, a: u32) -> u8 {
        (self.ent_get(((a - ENT_BASE) >> 2) as usize) >> ((a & 3) * 8)) as u8
    }

    fn ent_write8(&mut self, a: u32, v: u8) {
        let wi = ((a - ENT_BASE) >> 2) as usize;
        let s = (a & 3) * 8;
        let w = (self.ent_get(wi) & !(0xFFu32 << s)) | ((v as u32) << s);
        self.ent_set(wi, w);
    }

    #[inline]
    pub fn read32(&self, a: u32) -> u32 {
        let a = a & 0x1FFFFFFF;
        if a < RAM_SIZE {
            if self.state_on && a.wrapping_sub(STATE_OLD) < STATE_BYTES {
                return (0..4).fold(0u32, |v, k| v | (self.st_read8_old(a + k) as u32) << (8 * k));
            }
            if a & 3 == 0 { return self.w[(a >> 2) as usize]; }
            let lo = self.w[(a >> 2) as usize];
            let hi = if a + 4 < RAM_SIZE { self.w[((a >> 2) + 1) as usize] } else { 0 };
            let s = (a & 3) * 8;
            return (lo >> s) | (hi << (32 - s));
        }
        if a >= 0x1F800000 && a < 0x1F800400 {
            let o = (a - 0x1F800000) as usize;
            return self.scratch[o] as u32 | (self.scratch[o + 1] as u32) << 8 | (self.scratch[o + 2] as u32) << 16 | (self.scratch[o + 3] as u32) << 24;
        }
        if a >= SCRIPT_BASE && a < SCRIPT_BASE + SCRIPT_BYTES - 3 {
            let o = (a - SCRIPT_BASE) as usize;
            return u32::from_le_bytes([self.script[o], self.script[o + 1], self.script[o + 2], self.script[o + 3]]);
        }
        if a >= STATE_BASE && a < STATE_BASE + STATE_BYTES - 3 {
            if a & 3 == 0 { return self.state[((a - STATE_BASE) >> 2) as usize]; }
            return (0..4).fold(0u32, |v, k| v | (self.st_read8(a + k) as u32) << (8 * k));
        }
        if a >= ENT_BASE && a < ENT_BASE + ENT_BYTES - 3 {
            if a & 3 == 0 { return self.ent_get(((a - ENT_BASE) >> 2) as usize); }
            return (0..4).fold(0u32, |v, k| v | (self.ent_read8(a + k) as u32) << (8 * k));
        }
        if a >= OBJ_BASE && a < OBJ_BASE + OBJ_BYTES - 3 {
            if a & 3 == 0 { return self.obj_get(((a - OBJ_BASE) >> 2) as usize); }
            return (0..4).fold(0u32, |v, k| v | (self.obj_read8(a + k) as u32) << (8 * k));
        }
        0
    }

    #[inline]
    pub fn read16(&self, a: u32) -> u16 {
        let a = a & 0x1FFFFFFF;
        if a < RAM_SIZE {
            if self.state_on && a.wrapping_sub(STATE_OLD) < STATE_BYTES { return self.st_read8_old(a) as u16 | (self.st_read8_old(a + 1) as u16) << 8; }
            return (self.w[(a >> 2) as usize] >> ((a & 2) * 8)) as u16;
        }
        if a >= 0x1F800000 && a < 0x1F800400 {
            let o = (a - 0x1F800000) as usize;
            return self.scratch[o] as u16 | (self.scratch[o + 1] as u16) << 8;
        }
        if a >= SCRIPT_BASE && a < SCRIPT_BASE + SCRIPT_BYTES - 1 {
            let o = (a - SCRIPT_BASE) as usize;
            return u16::from_le_bytes([self.script[o], self.script[o + 1]]);
        }
        if a >= STATE_BASE && a < STATE_BASE + STATE_BYTES - 1 {
            return self.st_read8(a) as u16 | (self.st_read8(a + 1) as u16) << 8;
        }
        if a >= ENT_BASE && a < ENT_BASE + ENT_BYTES - 1 {
            return self.ent_read8(a) as u16 | (self.ent_read8(a + 1) as u16) << 8;
        }
        if a >= OBJ_BASE && a < OBJ_BASE + OBJ_BYTES - 1 {
            return self.obj_read8(a) as u16 | (self.obj_read8(a + 1) as u16) << 8;
        }
        0
    }

    #[inline]
    pub fn read8(&self, a: u32) -> u8 {
        let a = a & 0x1FFFFFFF;
        if a < RAM_SIZE {
            if self.state_on && a.wrapping_sub(STATE_OLD) < STATE_BYTES { return self.st_read8_old(a); }
            return (self.w[(a >> 2) as usize] >> ((a & 3) * 8)) as u8;
        }
        if a >= SCRIPT_BASE && a < SCRIPT_BASE + SCRIPT_BYTES { return self.script[(a - SCRIPT_BASE) as usize]; }
        if a >= STATE_BASE && a < STATE_BASE + STATE_BYTES { return self.st_read8(a); }
        if a >= 0x1F800000 && a < 0x1F800400 { return self.scratch[(a - 0x1F800000) as usize]; }
        if a >= ENT_BASE && a < ENT_BASE + ENT_BYTES { return self.ent_read8(a); }
        if a >= OBJ_BASE && a < OBJ_BASE + OBJ_BYTES { return self.obj_read8(a); }
        0
    }

    #[inline]
    pub fn write8(&mut self, a: u32, v: u8) {
        let a = a & 0x1FFFFFFF;
        if self.state_on && a.wrapping_sub(STATE_OLD) < STATE_BYTES { self.st_write8_old(a, v); return; }
        if a >= STATE_BASE && a < STATE_BASE + STATE_BYTES { self.st_write8(a, v); return; }
        if a >= SCRIPT_BASE && a < SCRIPT_BASE + SCRIPT_BYTES {
            let o = (a - SCRIPT_BASE) as usize;
            self.script[o] = v;
            if o >= self.script_len { self.script_len = o + 1; }
            return;
        }
        if a < RAM_SIZE {
            self.touch(a);
            let s = (a & 3) * 8;
            let i = (a >> 2) as usize;
            self.w[i] = (self.w[i] & !(0xFFu32 << s)) | ((v as u32) << s);
        } else if a >= 0x1F800000 && a < 0x1F800400 {
            self.scratch[(a - 0x1F800000) as usize] = v;
        } else if a >= ENT_BASE && a < ENT_BASE + ENT_BYTES {
            self.ent_write8(a, v);
        } else if a >= OBJ_BASE && a < OBJ_BASE + OBJ_BYTES {
            self.obj_write8(a, v);
        } else if a >= ENT_BASE {
            { if !self.ent_over && std::env::var("XG_OVER_LOG").is_ok() { eprintln!("OVERFLOW write at {:08X} (physical)", a); } self.ent_over = true; }
        }
    }

    #[inline]
    pub fn write16(&mut self, a: u32, v: u16) {
        let a = a & 0x1FFFFFFF;
        if (self.state_on && a.wrapping_sub(STATE_OLD) < STATE_BYTES) || (a >= STATE_BASE && a < STATE_BASE + STATE_BYTES) || (a >= SCRIPT_BASE && a < SCRIPT_BASE + SCRIPT_BYTES) {
            self.write8(a, v as u8);
            self.write8(a + 1, (v >> 8) as u8);
            return;
        }
        if a < RAM_SIZE {
            if a & 1 == 0 {
                self.touch(a);
                let s = (a & 2) * 8;
                let i = (a >> 2) as usize;
                self.w[i] = (self.w[i] & !(0xFFFFu32 << s)) | ((v as u32) << s);
            } else {
                self.write8(a, v as u8);
                self.write8(a + 1, (v >> 8) as u8);
            }
        } else if a >= 0x1F800000 && a < 0x1F800400 {
            let o = (a - 0x1F800000) as usize;
            self.scratch[o] = v as u8;
            self.scratch[o + 1] = (v >> 8) as u8;
        } else if a >= ENT_BASE && a < ENT_BASE + ENT_BYTES - 1 {
            self.ent_write8(a, v as u8);
            self.ent_write8(a + 1, (v >> 8) as u8);
        } else if a >= OBJ_BASE && a < OBJ_BASE + OBJ_BYTES - 1 {
            self.obj_write8(a, v as u8);
            self.obj_write8(a + 1, (v >> 8) as u8);
        } else if a >= ENT_BASE {
            { if !self.ent_over && std::env::var("XG_OVER_LOG").is_ok() { eprintln!("OVERFLOW write at {:08X} (physical)", a); } self.ent_over = true; }
        }
    }

    #[inline]
    pub fn write32(&mut self, a: u32, v: u32) {
        let a = a & 0x1FFFFFFF;
        if WATCH_HI.load(std::sync::atomic::Ordering::Relaxed) != 0 {
            let (lo, hi) = (WATCH_LO.load(std::sync::atomic::Ordering::Relaxed), WATCH_HI.load(std::sync::atomic::Ordering::Relaxed));
            if a >= lo && a < hi {
                eprintln!("WATCH write32 {:08X} <- {:08X}
{}", a | 0x8000_0000, v, std::backtrace::Backtrace::force_capture());
            }
        }
        if a >= SCRIPT_BASE && a < SCRIPT_BASE + SCRIPT_BYTES {
            for k in 0..4 { self.write8(a + k, (v >> (8 * k)) as u8); }
            return;
        }
        if (self.state_on && a.wrapping_sub(STATE_OLD) < STATE_BYTES) || (a >= STATE_BASE && a < STATE_BASE + STATE_BYTES) {
            if a >= STATE_BASE && a & 3 == 0 && a < STATE_BASE + STATE_BYTES - 3 { let wi = ((a - STATE_BASE) >> 2) as usize; self.state[wi] = v; return; }
            for k in 0..4 { self.write8(a + k, (v >> (8 * k)) as u8); }
            return;
        }
        if a < RAM_SIZE {
            if a & 3 == 0 { self.touch(a); self.w[(a >> 2) as usize] = v; }
            else {
                self.write8(a, v as u8);
                self.write8(a + 1, (v >> 8) as u8);
                self.write8(a + 2, (v >> 16) as u8);
                self.write8(a + 3, (v >> 24) as u8);
            }
        } else if a >= 0x1F800000 && a < 0x1F800400 {
            let o = (a - 0x1F800000) as usize;
            self.scratch[o] = v as u8;
            self.scratch[o + 1] = (v >> 8) as u8;
            self.scratch[o + 2] = (v >> 16) as u8;
            self.scratch[o + 3] = (v >> 24) as u8;
        } else if a >= ENT_BASE && a < ENT_BASE + ENT_BYTES - 3 {
            if a & 3 == 0 { self.ent_set(((a - ENT_BASE) >> 2) as usize, v); }
            else { for k in 0..4 { self.ent_write8(a + k, (v >> (8 * k)) as u8); } }
        } else if a >= OBJ_BASE && a < OBJ_BASE + OBJ_BYTES - 3 {
            if a & 3 == 0 { self.obj_set(((a - OBJ_BASE) >> 2) as usize, v); }
            else { for k in 0..4 { self.obj_write8(a + k, (v >> (8 * k)) as u8); } }
        } else if a >= ENT_BASE {
            { if !self.ent_over && std::env::var("XG_OVER_LOG").is_ok() { eprintln!("OVERFLOW write at {:08X} (physical)", a); } self.ent_over = true; }
        }
    }
}
