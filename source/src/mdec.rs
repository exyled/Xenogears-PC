// The picture decompression chip, as the movie module drives it: ports 0x1F801820 (command and data) and 0x1F801824
// (status and control) and its two DMA channels (0 into the chip, 1 out of it). The module's own code unpacks each movie
// frame into run-length codes; the chip turns them into pixels: per 16x16 macroblock the Cr, Cb and four Y blocks, each a DC
// value with its quant scale and then run and level pairs up to 0xFE00, dequantised, an inverse DCT, and YCbCr to 15- or
// 24-bit colour. A DMA finishes as it starts.

use crate::mem::Mem;

static ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5,
    12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

pub struct Mdec {
    command: u32,
    luma_quant: [u8; 64],
    chroma_quant: [u8; 64],
    input: Vec<u16>,
    output: Vec<u32>,
    out_read: usize,
    madr0: u32,
    bcr0: u32,
    madr1: u32,
    bcr1: u32,
    chcr0: u32,
    chcr1: u32,
    cos: [f32; 64],
    cos_t: [[f32; 8]; 8],
    coef: [i32; 64],
    tmp: [f32; 64],
    blocks: [[f32; 64]; 6],
    /// set when a channel's transfer has finished: [channel 0, channel 1]
    pub dma_done: [bool; 2],
}

fn signed10(v: i32) -> i32 {
    let v = v & 0x3FF;
    if v >= 0x200 { v - 0x400 } else { v }
}

fn clamp8(v: f32) -> i32 {
    // round half to even, without a library call: adding and taking away 1.5 * 2^52 rounds to an integer in the current mode
    let m = 6755399441055744.0f64;
    let r = ((v as f64) + m) - m;
    (r as i32).clamp(0, 255)
}

impl Mdec {
    pub fn new() -> Mdec {
        let mut cos = [0f32; 64];
        for x in 0..8 {
            for u in 0..8 {
                let k = if u == 0 { 0.5f64.sqrt() } else { 1.0 };
                cos[x * 8 + u] = (k * (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos()) as f32;
            }
        }
        let mut cos_t = [[0f32; 8]; 8];
        for u in 0..8 { for x in 0..8 { cos_t[u][x] = cos[x * 8 + u]; } }
        Mdec {
            cos_t,
            command: 0, luma_quant: [0; 64], chroma_quant: [0; 64], input: Vec::new(), output: Vec::new(), out_read: 0,
            madr0: 0, bcr0: 0, madr1: 0, bcr1: 0, chcr0: 0, chcr1: 0,
            cos, coef: [0; 64], tmp: [0.0; 64], blocks: [[0.0; 64]; 6], dma_done: [false; 2],
        }
    }

    pub fn save_to(&self, w: &mut crate::snap::W) {
        w.u32(self.command);
        w.bytes(&self.luma_quant);
        w.bytes(&self.chroma_quant);
        w.u16s(&self.input);
        w.u32s(&self.output);
        w.u32(self.out_read as u32);
        for v in [self.madr0, self.bcr0, self.madr1, self.bcr1, self.chcr0, self.chcr1] { w.u32(v); }
    }

    pub fn load_from(&mut self, r: &mut crate::snap::R) {
        self.command = r.u32();
        let l = r.bytes();
        let c = r.bytes();
        if l.len() == 64 { self.luma_quant.copy_from_slice(&l); }
        if c.len() == 64 { self.chroma_quant.copy_from_slice(&c); }
        self.input = r.u16s();
        self.output = r.u32s();
        self.out_read = r.u32() as usize;
        self.madr0 = r.u32(); self.bcr0 = r.u32(); self.madr1 = r.u32(); self.bcr1 = r.u32(); self.chcr0 = r.u32(); self.chcr1 = r.u32();
        self.dma_done = [false; 2];
    }

    pub fn owns(a: u32) -> bool {
        a == 0x1F801820 || a == 0x1F801824 || (a >= 0x1F801080 && a < 0x1F8010A0)
    }

    pub fn read(&self, a: u32) -> u32 {
        match a {
            0x1F801820 => 0,
            0x1F801824 => (if self.out_read >= self.output.len() { 0x80000000 } else { 0 }) | 0x00040000,
            0x1F801080 => self.madr0,
            0x1F801084 => self.bcr0,
            0x1F801088 => self.chcr0,
            0x1F801090 => self.madr1,
            0x1F801094 => self.bcr1,
            0x1F801098 => self.chcr1,
            _ => 0,
        }
    }

    pub fn write(&mut self, mem: &mut Mem, a: u32, v: u32) {
        match a {
            0x1F801820 => self.set_command(v),
            0x1F801824 => {
                if v & 0x80000000 != 0 {
                    self.input.clear();
                    self.output.clear();
                    self.out_read = 0;
                    self.command = 0;
                }
            }
            0x1F801080 => self.madr0 = v & 0xFFFFFF,
            0x1F801084 => self.bcr0 = v,
            0x1F801088 => {
                self.chcr0 = v;
                if v & 0x01000000 != 0 { self.dma_in(mem); }
            }
            0x1F801090 => self.madr1 = v & 0xFFFFFF,
            0x1F801094 => self.bcr1 = v,
            0x1F801098 => {
                self.chcr1 = v;
                if v & 0x01000000 != 0 { self.dma_out(mem); }
            }
            _ => {}
        }
    }

    fn set_command(&mut self, v: u32) {
        self.command = v;
        self.input.clear();
        if v >> 29 == 1 {
            self.output.clear();
            self.out_read = 0;
        }
    }

    fn words(bcr: u32) -> usize {
        let size = (bcr & 0xFFFF) as usize;
        let blocks = (bcr >> 16) as usize;
        (if size == 0 { 0x10000 } else { size }) * blocks.max(1)
    }

    fn dma_in(&mut self, mem: &mut Mem) {
        let n = Mdec::words(self.bcr0);
        let mut p = self.madr0;
        for _ in 0..n {
            let w = mem.read32(0x80000000 | p);
            self.input.push(w as u16);
            self.input.push((w >> 16) as u16);
            p = p.wrapping_add(4);
        }
        let op = self.command >> 29;
        if op == 1 { self.decode(); }
        else if op == 2 {
            for i in 0..64 { self.luma_quant[i] = self.byte(i); }
            if self.command & 1 != 0 { for i in 0..64 { self.chroma_quant[i] = self.byte(64 + i); } }
        }
        self.chcr0 &= !0x01000000;
        self.dma_done[0] = true;
    }

    fn byte(&self, i: usize) -> u8 {
        let h = i / 2;
        if h < self.input.len() { (self.input[h] >> ((i & 1) * 8)) as u8 } else { 0 }
    }

    fn dma_out(&mut self, mem: &mut Mem) {
        let n = Mdec::words(self.bcr1);
        let mut p = self.madr1;
        for _ in 0..n {
            let w = if self.out_read < self.output.len() { let w = self.output[self.out_read]; self.out_read += 1; w } else { 0 };
            mem.write32(0x80000000 | p, w);
            p = p.wrapping_add(4);
        }
        self.chcr1 &= !0x01000000;
        self.dma_done[1] = true;
    }

    // One block from the codes at pos; false at the end of the data.
    fn block(&mut self, pos: &mut usize, luma: bool, which: usize) -> bool {
        while *pos < self.input.len() && self.input[*pos] == 0xFE00 { *pos += 1; }
        if *pos >= self.input.len() { return false; }
        self.coef = [0; 64];
        let quant = if luma { self.luma_quant } else { self.chroma_quant };
        let mut n = self.input[*pos] as i32;
        *pos += 1;
        let q = n >> 10;
        let mut k: i32 = 0;
        let mut val = signed10(n) * quant[0] as i32;
        loop {
            if q == 0 { val = signed10(n) * 2; }
            val = val.clamp(-0x400, 0x3FF);
            if q > 0 { self.coef[ZIGZAG[k as usize]] = val; } else { self.coef[k as usize] = val; }
            if *pos >= self.input.len() { break; }
            n = self.input[*pos] as i32;
            *pos += 1;
            k += (n >> 10) + 1;
            if k > 63 { break; }
            val = (signed10(n) * quant[k as usize] as i32 * q + 4) / 8;
        }
        // the two passes sum the same terms in the same order as the plain form (each output adds its terms by ascending
        // index); terms that are zero are left out, and the eight outputs of a row are computed side by side
        let mut row_used = [false; 8];
        for yy in 0..8 {
            let mut acc = [0f32; 8];
            let mut any = false;
            for u in 0..8 {
                let c = self.coef[yy * 8 + u];
                if c == 0 { continue; }
                any = true;
                let cf = c as f32;
                let ct = &self.cos_t[u];
                for x in 0..8 { acc[x] += ct[x] * cf; }
            }
            row_used[yy] = any;
            self.tmp[yy * 8..yy * 8 + 8].copy_from_slice(&acc);
        }
        for yy in 0..8 {
            let mut acc = [0f32; 8];
            for v in 0..8 {
                if !row_used[v] { continue; }
                let cv = self.cos[yy * 8 + v];
                let t = &self.tmp[v * 8..v * 8 + 8];
                for x in 0..8 { acc[x] += cv * t[x]; }
            }
            for x in 0..8 { self.blocks[which][yy * 8 + x] = acc[x] / 4.0; }
        }
        true
    }

    fn decode(&mut self) {
        let depth = (self.command >> 27) & 3; // 2: 24-bit, 3: 15-bit
        let signed_out = self.command & 0x04000000 != 0;
        let bit15 = self.command & 0x02000000 != 0;
        let mut pos = 0usize;
        let mut bytes: Vec<u8> = Vec::new();
        loop {
            if !self.block(&mut pos, false, 0) || !self.block(&mut pos, false, 1) { break; }
            let mut whole = true;
            for b in 2..6 {
                if !self.block(&mut pos, true, b) { whole = false; break; }
            }
            if !whole { break; }
            for py in 0..16usize {
                for px in 0..16usize {
                    let lum = self.blocks[2 + (py / 8) * 2 + px / 8][(py & 7) * 8 + (px & 7)];
                    let ci = (py / 2) * 8 + px / 2;
                    let cr = self.blocks[0][ci];
                    let cb = self.blocks[1][ci];
                    let mut r = clamp8(lum + 1.402f32 * cr + 128.0);
                    let mut g = clamp8(lum - 0.3437f32 * cb - 0.7143f32 * cr + 128.0);
                    let mut bl = clamp8(lum + 1.772f32 * cb + 128.0);
                    if signed_out { r ^= 0x80; g ^= 0x80; bl ^= 0x80; }
                    if depth == 2 {
                        bytes.push(r as u8);
                        bytes.push(g as u8);
                        bytes.push(bl as u8);
                    } else {
                        let c = (r >> 3) | (g >> 3) << 5 | (bl >> 3) << 10 | if bit15 { 0x8000 } else { 0 };
                        bytes.push(c as u8);
                        bytes.push((c >> 8) as u8);
                    }
                }
            }
        }
        let mut i = 0;
        while i < bytes.len() {
            let mut w = 0u32;
            for j in 0..4 {
                if i + j < bytes.len() { w |= (bytes[i + j] as u32) << (j * 8); }
            }
            self.output.push(w);
            i += 4;
        }
    }
}
