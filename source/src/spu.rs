// The sound chip as the game's own sound driver drives it: ports at 0x1F801C00-0x1F801DFF (24 voices) and DMA channel 4.
// render() plays the voices at 44100 Hz: ADPCM blocks from the chip's memory at the voice's pitch, through its envelope
// and its two volumes; voices with reverb on also feed the reverb unit.

use crate::mem::Mem;

pub const RATE: i32 = 44100;
pub const VOICES: usize = 24;
pub const RAM_BYTES: u32 = 0x80000;

#[derive(Clone)]
struct Voice {
    addr: u32,
    repeat: u32,
    repeat_set: bool,
    counter: i32,
    block: [i16; 28],
    block_pos: usize,
    s1: i32,
    s2: i32,
    prev: i32,
    cur: i32,
    level: i32,
    phase: i32, // 0 off, 1 attack, 2 decay, 3 sustain, 4 release
    tick: i32,
}

impl Voice {
    fn new() -> Voice {
        Voice { addr: 0, repeat: 0, repeat_set: false, counter: 0, block: [0; 28], block_pos: 28, s1: 0, s2: 0, prev: 0, cur: 0, level: 0, phase: 0, tick: 0 }
    }
}

pub struct Spu {
    pub ram: Vec<u8>,
    reg: [u16; 0x200],
    transfer: u32,
    madr: u32,
    bcr: u32,
    chcr: u32,
    endx: i32,
    v: Vec<Voice>,
    reverb: Option<Reverb>,
    reverb_base: i32,
    reverb_regs: [i16; 32],
    /// set when a DMA 4 transfer has finished; the machine takes it and raises the interrupt
    pub dma_done: bool,
}

static F0: [i32; 5] = [0, 60, 115, 98, 122];
static F1: [i32; 5] = [0, 0, -52, -55, -60];

impl Spu {
    pub fn new() -> Spu {
        Spu {
            ram: vec![0u8; RAM_BYTES as usize],
            reg: [0; 0x200],
            transfer: 0, madr: 0, bcr: 0, chcr: 0, endx: 0,
            v: (0..VOICES).map(|_| Voice::new()).collect(),
            reverb: None,
            reverb_base: -1,
            reverb_regs: [0; 32],
            dma_done: false,
        }
    }

    pub fn save_to(&self, w: &mut crate::snap::W) {
        w.bytes(&self.ram);
        w.u16s(&self.reg);
        w.u32(self.transfer);
        w.u32(self.madr);
        w.u32(self.bcr);
        w.u32(self.chcr);
        w.i32(self.endx);
        for v in &self.v {
            w.u32(v.addr); w.u32(v.repeat); w.bool(v.repeat_set); w.i32(v.counter);
            w.i16s(&v.block); w.u32(v.block_pos as u32);
            w.i32(v.s1); w.i32(v.s2); w.i32(v.prev); w.i32(v.cur); w.i32(v.level); w.i32(v.phase); w.i32(v.tick);
        }
        w.i32(self.reverb_base);
        w.i16s(&self.reverb_regs);
        match &self.reverb {
            None => w.bool(false),
            Some(rv) => {
                w.bool(true);
                w.i16s(&rv.r);
                w.i16s(&rv.buf);
                w.i32(rv.size); w.i32(rv.pos); w.f64(rv.phase); w.f64(rv.per);
                w.f32(rv.in_l); w.f32(rv.in_r); w.i32(rv.in_n);
                w.f32(rv.prev_l); w.f32(rv.prev_r); w.f32(rv.cur_l); w.f32(rv.cur_r);
            }
        }
    }

    pub fn load_from(&mut self, r: &mut crate::snap::R) {
        let ram = r.bytes();
        if ram.len() == self.ram.len() { self.ram.copy_from_slice(&ram); }
        let reg = r.u16s();
        if reg.len() == self.reg.len() { self.reg.copy_from_slice(&reg); }
        self.transfer = r.u32(); self.madr = r.u32(); self.bcr = r.u32(); self.chcr = r.u32(); self.endx = r.i32();
        for v in self.v.iter_mut() {
            v.addr = r.u32(); v.repeat = r.u32(); v.repeat_set = r.bool(); v.counter = r.i32();
            let b = r.i16s();
            if b.len() == 28 { v.block.copy_from_slice(&b); }
            v.block_pos = r.u32() as usize;
            v.s1 = r.i32(); v.s2 = r.i32(); v.prev = r.i32(); v.cur = r.i32(); v.level = r.i32(); v.phase = r.i32(); v.tick = r.i32();
        }
        self.reverb_base = r.i32();
        let rr = r.i16s();
        if rr.len() == 32 { self.reverb_regs.copy_from_slice(&rr); }
        if r.bool() {
            let regs = r.i16s();
            let buf = r.i16s();
            let size = r.i32();
            let pos = r.i32();
            let phase = r.f64();
            let per = r.f64();
            let (in_l, in_r, in_n) = (r.f32(), r.f32(), r.i32());
            let (prev_l, prev_r, cur_l, cur_r) = (r.f32(), r.f32(), r.f32(), r.f32());
            let mut rv = Reverb { r: [0; 32], buf, size, pos, phase, per, in_l, in_r, in_n, prev_l, prev_r, cur_l, cur_r };
            if regs.len() == 32 { rv.r.copy_from_slice(&regs); }
            self.reverb = Some(rv);
        } else {
            self.reverb = None;
        }
        self.dma_done = false;
    }

    pub fn owns(a: u32) -> bool {
        (a >= 0x1F801C00 && a < 0x1F802000) || (a >= 0x1F8010C0 && a < 0x1F8010D0)
    }

    pub fn control(&self) -> u16 { self.reg[0xD5] }

    pub fn read(&self, a: u32, size: i32) -> u32 {
        if a >= 0x1F8010C0 && a < 0x1F8010D0 {
            return if a == 0x1F8010C0 { self.madr } else if a == 0x1F8010C4 { self.bcr } else if a == 0x1F8010C8 { self.chcr } else { 0 };
        }
        let r = ((a - 0x1F801C00) >> 1) as usize;
        let mut v = self.reg_read(r) as u32;
        if size == 4 { v |= (self.reg_read(r + 1) as u32) << 16; }
        v
    }

    fn reg_read(&self, r: usize) -> u16 {
        if r >= 0x200 { return 0; }
        match r {
            0xCE => return (self.endx & 0xFFFF) as u16,
            0xCF => return (self.endx >> 16) as u16,
            0xD7 => return self.reg[0xD5] & 0x3F,
            _ => {}
        }
        if r < VOICES * 8 && (r & 7) == 6 { return self.v[r >> 3].level as u16; }
        self.reg[r]
    }

    pub fn write(&mut self, mem: &mut Mem, a: u32, v: u32, size: i32) {
        if a >= 0x1F8010C0 && a < 0x1F8010D0 {
            if a == 0x1F8010C0 { self.madr = v & 0xFFFFFF; }
            else if a == 0x1F8010C4 { self.bcr = v; }
            else if a == 0x1F8010C8 {
                self.chcr = v;
                if v & 0x01000000 != 0 { self.dma(mem); }
            }
            return;
        }
        let r = ((a - 0x1F801C00) >> 1) as usize;
        self.put(r, v as u16);
        if size == 4 { self.put(r + 1, (v >> 16) as u16); }
    }

    fn put(&mut self, r: usize, v: u16) {
        if r >= 0x200 { return; }
        self.reg[r] = v;
        match r {
            0xC4 => self.key_on(v as i32, 0),
            0xC5 => self.key_on(v as i32, 16),
            0xC6 => self.key_off(v as i32, 0),
            0xC7 => self.key_off(v as i32, 16),
            0xD3 => self.transfer = (v as u32) << 3,
            0xD4 => {
                self.poke16(self.transfer, v);
                self.transfer = (self.transfer + 2) & (RAM_BYTES - 1);
            }
            0xD1 => self.reverb_base = -1,
            _ => {}
        }
        if r >= 0xE0 && r < 0x100 {
            self.reverb_regs[r - 0xE0] = v as i16;
            self.reverb_base = -1;
        }
        if r < VOICES * 8 && (r & 7) == 7 {
            self.v[r >> 3].repeat = (v as u32) << 3;
            self.v[r >> 3].repeat_set = true;
        }
    }

    fn poke16(&mut self, at: u32, v: u16) {
        let at = (at & (RAM_BYTES - 2)) as usize;
        self.ram[at] = v as u8;
        self.ram[at + 1] = (v >> 8) as u8;
    }

    fn dma(&mut self, mem: &mut Mem) {
        let size = (self.bcr & 0xFFFF) as i32;
        let blocks = (self.bcr >> 16) as i32;
        let words = (if size == 0 { 0x10000 } else { size }) * blocks.max(1);
        let to_chip = self.chcr & 1 != 0;
        let mut p = self.madr;
        for _ in 0..words {
            if to_chip {
                let w = mem.read32(0x80000000 | p);
                self.poke16(self.transfer, w as u16);
                self.poke16(self.transfer + 2, (w >> 16) as u16);
            } else {
                let at = (self.transfer & (RAM_BYTES - 4)) as usize;
                let w = self.ram[at] as u32 | (self.ram[at + 1] as u32) << 8 | (self.ram[at + 2] as u32) << 16 | (self.ram[at + 3] as u32) << 24;
                mem.write32(0x80000000 | p, w);
            }
            self.transfer = (self.transfer + 4) & (RAM_BYTES - 1);
            p = p.wrapping_add(4);
        }
        self.chcr &= !0x01000000;
        self.dma_done = true;
    }

    fn key_on(&mut self, bits: i32, first: usize) {
        for i in 0..16 {
            if bits & (1 << i) == 0 || first + i >= VOICES { continue; }
            let n = first + i;
            let addr = (self.reg[n * 8 + 3] as u32) << 3;
            let v = &mut self.v[n];
            v.addr = addr;
            if !v.repeat_set { v.repeat = v.addr; }
            v.repeat_set = false;
            v.counter = 0; v.block_pos = 28; v.s1 = 0; v.s2 = 0; v.prev = 0; v.cur = 0;
            v.level = 0; v.phase = 1; v.tick = 0;
            self.endx &= !(1 << n);
        }
    }

    fn key_off(&mut self, bits: i32, first: usize) {
        for i in 0..16 {
            if bits & (1 << i) != 0 && first + i < VOICES && self.v[first + i].phase != 0 {
                self.v[first + i].phase = 4;
                self.v[first + i].tick = 0;
            }
        }
    }

    fn next_block(ram: &[u8], endx: &mut i32, v: &mut Voice, n: usize) {
        let at = (v.addr & (RAM_BYTES - 16)) as usize;
        let mut shift = (ram[at] & 0xF) as i32;
        let mut filter = ((ram[at] >> 4) & 7) as usize;
        let flags = ram[at + 1];
        if filter > 4 { filter = 4; }
        if shift > 12 { shift = 9; }
        if flags & 4 != 0 && !v.repeat_set { v.repeat = at as u32; }
        for k in 0..28usize {
            let b = ram[at + 2 + k / 2] as i32;
            let nib = if k & 1 == 0 { b & 0xF } else { b >> 4 };
            let mut s = (((nib << 28) >> 28) << 12) >> shift;
            s += (v.s1 * F0[filter] + v.s2 * F1[filter] + 32) >> 6;
            s = s.clamp(-32768, 32767);
            v.s2 = v.s1;
            v.s1 = s;
            v.block[k] = s as i16;
        }
        v.block_pos = 0;
        v.addr = at as u32 + 16;
        if flags & 1 != 0 {
            // the block that ends the sample: a loop goes back to the repeat address, otherwise the voice stops
            *endx |= 1 << n;
            if flags & 2 != 0 { v.addr = v.repeat; }
            else { v.addr = v.repeat; v.phase = 4; v.level = 0; }
        }
    }

    fn sample(ram: &[u8], endx: &mut i32, reg: &[u16; 0x200], v: &mut Voice, n: usize) -> i32 {
        let pitch = (reg[n * 8 + 2] as i32).min(0x3FFF);
        v.counter += pitch;
        while v.counter >= 0x1000 {
            v.counter -= 0x1000;
            if v.block_pos >= 28 { Spu::next_block(ram, endx, v, n); }
            v.prev = v.cur;
            v.cur = v.block[v.block_pos] as i32;
            v.block_pos += 1;
        }
        v.prev + (((v.cur - v.prev) * v.counter) >> 12)
    }

    // the envelope: the chip's rate stepping (a step every 2^(shift-11) samples)
    fn step(v: &mut Voice, rate: i32, exponential: bool, decrease: bool) {
        let mut shift = rate >> 2;
        let mut step = if decrease { -8 + (rate & 3) } else { 7 - (rate & 3) };
        if exponential && !decrease && v.level > 0x6000 { shift += 2; }
        let cycles = 1 << (shift - 11).max(0);
        step <<= (11 - shift).max(0);
        if exponential && decrease { step = (step * v.level) >> 15; }
        v.tick += 1;
        if v.tick < cycles { return; }
        v.tick = 0;
        v.level = (v.level + step).clamp(0, 0x7FFF);
    }

    fn envelope(reg: &[u16; 0x200], v: &mut Voice, n: usize) {
        let lo = reg[n * 8 + 4] as i32;
        let hi = reg[n * 8 + 5] as i32;
        match v.phase {
            1 => {
                Spu::step(v, (lo >> 8) & 0x7F, lo & 0x8000 != 0, false);
                if v.level >= 0x7FFF { v.phase = 2; v.tick = 0; }
            }
            2 => {
                Spu::step(v, ((lo >> 4) & 0xF) << 2, true, true);
                if v.level <= ((lo & 0xF) + 1) * 0x800 { v.phase = 3; v.tick = 0; }
            }
            3 => {
                Spu::step(v, (hi >> 6) & 0x7F, hi & 0x8000 != 0, hi & 0x4000 != 0);
            }
            4 => {
                Spu::step(v, (hi & 0x1F) << 2, hi & 0x20 != 0, true);
                if v.level <= 0 { v.phase = 0; }
            }
            _ => {}
        }
    }

    #[inline]
    fn volume(raw: u16) -> f32 {
        if raw & 0x8000 != 0 { 0.5 } else { ((raw << 1) as i16) as f32 / 32768.0 }
    }

    /// Plays `frames` stereo frames into `out` (interleaved), starting at frame `at`. `cd` is the CD input (movie audio)
    /// for these frames when the control register asks for it.
    pub fn render(&mut self, out: &mut [i16], at: usize, frames: usize, cd: Option<&[i16]>) {
        let ctl = self.reg[0xD5];
        let on = ctl & 0x8000 != 0;
        let unmuted = ctl & 0x4000 != 0;
        let reverb_on = ctl & 0x80 != 0;
        let main_l = Spu::volume(self.reg[0xC0]);
        let main_r = Spu::volume(self.reg[0xC1]);
        let rev_l = (self.reg[0xC2] as i16) as f32 / 32768.0;
        let rev_r = (self.reg[0xC3] as i16) as f32 / 32768.0;
        let eon = self.reg[0xCC] as i32 | (self.reg[0xCD] as i32) << 16;
        if reverb_on && self.reverb_base != self.reg[0xD1] as i32 {
            self.reverb_base = self.reg[0xD1] as i32;
            self.reverb = Some(Reverb::new(self.reverb_regs, (RAM_BYTES as i32 - (self.reverb_base << 3)) as usize, RATE));
        }
        let cd_l = (self.reg[0xD8] as i16) as f32 / 32768.0;
        let cd_r = (self.reg[0xD9] as i16) as f32 / 32768.0;
        for f in 0..frames {
            let (mut l, mut r, mut el, mut er) = (0f32, 0f32, 0f32, 0f32);
            if on {
                for n in 0..VOICES {
                    if self.v[n].phase == 0 { continue; }
                    let s = (Spu::sample(&self.ram, &mut self.endx, &self.reg, &mut self.v[n], n) * self.v[n].level) >> 15;
                    Spu::envelope(&self.reg, &mut self.v[n], n);
                    let vl = s as f32 * Spu::volume(self.reg[n * 8]);
                    let vr = s as f32 * Spu::volume(self.reg[n * 8 + 1]);
                    l += vl;
                    r += vr;
                    if eon & (1 << n) != 0 { el += vl; er += vr; }
                }
            }
            if let Some(cd) = cd {
                if f * 2 + 1 < cd.len() {
                    l += cd[f * 2] as f32 * cd_l;
                    r += cd[f * 2 + 1] as f32 * cd_r;
                }
            }
            if reverb_on {
                if let Some(rv) = self.reverb.as_mut() {
                    let (ol, or) = rv.process(el, er);
                    l += ol * rev_l;
                    r += or * rev_r;
                }
            }
            if !unmuted { l = 0.0; r = 0.0; }
            l *= main_l;
            r *= main_r;
            out[(at + f) * 2] = l.clamp(-32768.0, 32767.0) as i16;
            out[(at + f) * 2 + 1] = r.clamp(-32768.0, 32767.0) as i16;
        }
    }
}

// The reverb unit (the registers' order is the library's: dAPF1, dAPF2, vIIR, vCOMB1-4, vWALL, vAPF1, vAPF2, mLSAME, mRSAME, ...).
struct Reverb {
    r: [i16; 32],
    buf: Vec<i16>,
    size: i32,
    pos: i32,
    phase: f64,
    per: f64,
    in_l: f32,
    in_r: f32,
    in_n: i32,
    prev_l: f32,
    prev_r: f32,
    cur_l: f32,
    cur_r: f32,
}

#[inline]
fn clip(v: f32) -> i16 { v.clamp(-32768.0, 32767.0) as i16 }

impl Reverb {
    fn new(regs: [i16; 32], work_area_bytes: usize, rate: i32) -> Reverb {
        let size = ((work_area_bytes / 2) as i32).max(8);
        Reverb {
            r: regs, buf: vec![0i16; size as usize], size, pos: 0, phase: 0.0, per: 22050.0 / rate as f64,
            in_l: 0.0, in_r: 0.0, in_n: 0, prev_l: 0.0, prev_r: 0.0, cur_l: 0.0, cur_r: 0.0,
        }
    }

    #[inline]
    fn at(&self, offset: i32) -> usize {
        let i = (self.pos + offset * 4) % self.size;
        (if i < 0 { i + self.size } else { i }) as usize
    }
    #[inline]
    fn prev(&self, index: usize) -> usize { if index == 0 { self.size as usize - 1 } else { index - 1 } }
    #[inline]
    fn vol(&self, reg: usize) -> f32 { self.r[reg] as f32 / 32768.0 }
    #[inline]
    fn off(&self, reg: usize) -> i32 { self.r[reg] as u16 as i32 }
    #[inline]
    fn b(&self, offset: i32) -> f32 { self.buf[self.at(offset)] as f32 }

    fn step(&mut self, in_l: f32, in_r: f32) {
        let lin = in_l * self.vol(30);
        let rin = in_r * self.vol(31);
        let wall = self.vol(7);
        let iir = self.vol(2);
        let regs: [(usize, usize, f32); 4] = [(10, 16, lin), (11, 17, rin), (18, 25, lin), (19, 24, rin)];
        for (dst, src, input) in regs {
            let i = self.at(self.off(dst));
            let p = self.prev(i);
            let s = self.buf[self.at(self.off(src))] as f32;
            self.buf[i] = clip((input + s * wall - self.buf[p] as f32) * iir + self.buf[p] as f32);
        }
        let (c1, c2, c3, c4) = (self.vol(3), self.vol(4), self.vol(5), self.vol(6));
        let mut l = c1 * self.b(self.off(12)) + c2 * self.b(self.off(14)) + c3 * self.b(self.off(20)) + c4 * self.b(self.off(22));
        let mut r = c1 * self.b(self.off(13)) + c2 * self.b(self.off(15)) + c3 * self.b(self.off(21)) + c4 * self.b(self.off(23));
        let a1 = self.vol(8);
        let a2 = self.vol(9);
        let d1 = self.off(0);
        let d2 = self.off(1);
        l -= a1 * self.b(self.off(26) - d1);
        let i = self.at(self.off(26)); self.buf[i] = clip(l);
        l = l * a1 + self.b(self.off(26) - d1);
        r -= a1 * self.b(self.off(27) - d1);
        let i = self.at(self.off(27)); self.buf[i] = clip(r);
        r = r * a1 + self.b(self.off(27) - d1);
        l -= a2 * self.b(self.off(28) - d2);
        let i = self.at(self.off(28)); self.buf[i] = clip(l);
        l = l * a2 + self.b(self.off(28) - d2);
        r -= a2 * self.b(self.off(29) - d2);
        let i = self.at(self.off(29)); self.buf[i] = clip(r);
        r = r * a2 + self.b(self.off(29) - d2);
        self.prev_l = self.cur_l;
        self.prev_r = self.cur_r;
        self.cur_l = l.clamp(-32768.0, 32767.0);
        self.cur_r = r.clamp(-32768.0, 32767.0);
        self.pos = (self.pos + 1) % self.size;
    }

    /// One output frame: the reverb-flagged voices' left and right sum goes in, the reverb's output (before the depth volume) comes out.
    fn process(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        self.in_l += in_l;
        self.in_r += in_r;
        self.in_n += 1;
        self.phase += self.per;
        while self.phase >= 1.0 {
            self.phase -= 1.0;
            let n = self.in_n as f32;
            let (a, b) = if self.in_n > 0 { (self.in_l / n, self.in_r / n) } else { (0.0, 0.0) };
            self.step(a, b);
            self.in_l = 0.0;
            self.in_r = 0.0;
            self.in_n = 0;
        }
        let t = self.phase as f32;
        (self.prev_l + (self.cur_l - self.prev_l) * t, self.prev_r + (self.cur_r - self.prev_r) * t)
    }
}
