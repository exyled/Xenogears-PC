// The geometry coprocessor (coprocessor 2): 32 data and 32 control registers and the vector and colour commands.

pub struct Gte {
    pub d: [i32; 32],
    pub c: [i32; 32],
    flag: u32,
    mat: [i32; 9],
    unr: [u8; 0x101],
    /// Where each transformed vertex landed on the screen and how deep it was (frame stamp << 16 | depth), indexed by
    /// y & 1023, x & 1023; the large picture reads it to texture in perspective. Depth 0 marks two different depths at one spot.
    pub zrec: Vec<u32>,
    /// the sixteenth-of-a-pixel remainders (x in the high four bits, y in the low four) of those same positions; 0xFF when
    /// two vertices with different remainders landed on one pixel this frame
    pub zfrac: Vec<u8>,
    pub stamp: u32,
    /// 0: the console's projection; otherwise the share of the horizontal projected offset that is kept, in 256ths (192 for
    /// a 16:9 picture from a 4:3 one): the picture is shown stretched back, so the 3D scene shows more at the same shape
    pub wide: i64,
    /// writes to zrec since the picture's worker threads were last told (key, value pairs), kept only when `log_z`
    pub zlog: Vec<u32>,
    pub log_z: bool,
}

#[inline]
fn wrap44(v: i64) -> i64 { (v << 20) >> 20 }

impl Gte {
    pub fn new() -> Gte {
        let mut unr = [0u8; 0x101];
        for i in 0..=0x100i32 {
            unr[i as usize] = (((0x40000 / (i + 0x100) + 1) / 2) - 0x101).max(0) as u8;
        }
        Gte { d: [0; 32], c: [0; 32], flag: 0, mat: [0; 9], unr, zrec: vec![0u32; 1 << 20], zfrac: vec![0u8; 1 << 20], stamp: 0, wide: 0, zlog: Vec::new(), log_z: false }
    }

    #[inline] fn vx(&self, n: usize) -> i64 { self.d[n * 2] as i16 as i64 }
    #[inline] fn vy(&self, n: usize) -> i64 { (self.d[n * 2] >> 16) as i16 as i64 }
    #[inline] fn vz(&self, n: usize) -> i64 { self.d[n * 2 + 1] as i16 as i64 }
    #[inline] fn ir0(&self) -> i64 { self.d[8] as i16 as i64 }
    #[inline] fn ir(&self, n: usize) -> i64 { self.d[8 + n] as i16 as i64 }
    #[inline] fn mac(&self, n: usize) -> i64 { self.d[24 + n] as i64 }

    pub fn save_to(&self, w: &mut crate::snap::W) {
        w.i32s(&self.d);
        w.i32s(&self.c);
        w.u32(self.flag);
    }

    pub fn load_from(&mut self, r: &mut crate::snap::R) {
        let d = r.i32s();
        let c = r.i32s();
        if d.len() == 32 { self.d.copy_from_slice(&d); }
        if c.len() == 32 { self.c.copy_from_slice(&c); }
        self.flag = r.u32();
    }

    pub fn read_data(&self, r: usize) -> u32 {
        match r {
            1 | 3 | 5 | 8 | 9 | 10 | 11 => self.d[r] as i16 as i32 as u32,
            7 | 16 | 17 | 18 | 19 => self.d[r] as u16 as u32,
            15 => self.d[14] as u32,
            28 | 29 => self.irgb() as u32,
            31 => self.d[31] as u32,
            _ => self.d[r] as u32,
        }
    }

    fn irgb(&self) -> i32 {
        let cl = |v: i64| (v / 0x80).clamp(0, 0x1F) as i32;
        cl(self.ir(1)) | cl(self.ir(2)) << 5 | cl(self.ir(3)) << 10
    }

    pub fn write_data(&mut self, r: usize, v: u32) {
        match r {
            15 => {
                self.d[12] = self.d[13];
                self.d[13] = self.d[14];
                self.d[14] = v as i32;
                self.d[15] = v as i32;
            }
            28 => {
                self.d[28] = (v & 0x7FFF) as i32;
                self.d[9] = (v & 0x1F) as i32 * 0x80;
                self.d[10] = ((v >> 5) & 0x1F) as i32 * 0x80;
                self.d[11] = ((v >> 10) & 0x1F) as i32 * 0x80;
            }
            29 => {}
            30 => {
                self.d[30] = v as i32;
                let x = if (v as i32) < 0 { !v } else { v };
                self.d[31] = x.leading_zeros() as i32;
            }
            31 => {}
            _ => self.d[r] = v as i32,
        }
    }

    pub fn read_control(&self, r: usize) -> u32 {
        match r {
            4 | 12 | 20 | 26 | 27 | 29 | 30 => self.c[r] as i16 as i32 as u32,
            31 => {
                let mut f = self.c[31] as u32 & 0x7FFFF000;
                if f & 0x7F87E000 != 0 { f |= 0x80000000; }
                f
            }
            _ => self.c[r] as u32,
        }
    }

    pub fn write_control(&mut self, r: usize, v: u32) {
        if r == 31 { self.c[31] = (v & 0x7FFFF000) as i32; } else { self.c[r] = v as i32; }
    }

    fn a1(&mut self, v: i64) { if v > 0x7FFFFFFFFFF { self.flag |= 1 << 30; } else if v < -0x80000000000 { self.flag |= 1 << 27; } }
    fn a2(&mut self, v: i64) { if v > 0x7FFFFFFFFFF { self.flag |= 1 << 29; } else if v < -0x80000000000 { self.flag |= 1 << 26; } }
    fn a3(&mut self, v: i64) { if v > 0x7FFFFFFFFFF { self.flag |= 1 << 28; } else if v < -0x80000000000 { self.flag |= 1 << 25; } }
    fn check_mac(&mut self, n: usize, v: i64) {
        match n { 1 => self.a1(v), 2 => self.a2(v), _ => self.a3(v) }
    }

    fn sat_ir(&mut self, n: usize, v: i64, lm: bool) -> i32 {
        let lo: i64 = if lm { 0 } else { -0x8000 };
        let hi: i64 = 0x7FFF;
        let bit = if n == 1 { 1u32 << 24 } else if n == 2 { 1u32 << 23 } else { 1u32 << 22 };
        if v < lo { self.flag |= bit; return lo as i32; }
        if v > hi { self.flag |= bit; return hi as i32; }
        v as i32
    }

    fn sat_colour(&mut self, n: usize, v: i64) -> i32 {
        let bit = if n == 0 { 1u32 << 21 } else if n == 1 { 1u32 << 20 } else { 1u32 << 19 };
        if v < 0 { self.flag |= bit; return 0; }
        if v > 0xFF { self.flag |= bit; return 0xFF; }
        v as i32
    }

    pub fn command(&mut self, cmd: u32) {
        self.flag = 0;
        let sf = (((cmd >> 19) & 1) * 12) as i32;
        let mx = ((cmd >> 17) & 3) as usize;
        let v = ((cmd >> 15) & 3) as usize;
        let cv = ((cmd >> 13) & 3) as usize;
        let lm = (cmd >> 10) & 1 != 0;
        match cmd & 0x3F {
            0x01 => self.rtps(0, sf, lm),
            0x06 => self.nclip(),
            0x0C => self.op(sf, lm),
            0x10 => self.dpcs(sf, lm, false),
            0x11 => self.intpl(sf, lm),
            0x12 => self.mvmva(sf, mx, v, cv, lm),
            0x13 => self.ncds(0, sf, lm),
            0x14 => self.cdp(sf, lm),
            0x16 => { self.ncds(0, sf, lm); self.ncds(1, sf, lm); self.ncds(2, sf, lm); }
            0x1B => self.nccs(0, sf, lm),
            0x1C => self.cc(sf, lm),
            0x1E => self.ncs(0, sf, lm),
            0x20 => { self.ncs(0, sf, lm); self.ncs(1, sf, lm); self.ncs(2, sf, lm); }
            0x28 => self.sqr(sf, lm),
            0x29 => self.dcpl(sf, lm),
            0x2A => { self.dpcs(sf, lm, true); self.dpcs(sf, lm, true); self.dpcs(sf, lm, true); }
            0x2D => self.avsz3(),
            0x2E => self.avsz4(),
            0x30 => { self.rtps(0, sf, lm); self.rtps(1, sf, lm); self.rtps(2, sf, lm); }
            0x3D => self.gpf(sf, lm),
            0x3E => self.gpl(sf, lm),
            0x3F => { self.nccs(0, sf, lm); self.nccs(1, sf, lm); self.nccs(2, sf, lm); }
            _ => {}
        }
        self.c[31] = self.flag as i32;
    }

    fn load_mat(&mut self, b: usize) {
        let (c0, c1, c2, c3, c4) = (self.c[b], self.c[b + 1], self.c[b + 2], self.c[b + 3], self.c[b + 4]);
        let m = &mut self.mat;
        m[0] = c0 as i16 as i32; m[1] = c0 >> 16;
        m[2] = c1 as i16 as i32; m[3] = c1 >> 16;
        m[4] = c2 as i16 as i32; m[5] = c2 >> 16;
        m[6] = c3 as i16 as i32; m[7] = c3 >> 16;
        m[8] = c4 as i16 as i32;
    }

    // a 3x3 matrix element from the control registers (two 16-bit elements a word)
    fn mel(&self, base: usize, row: usize, col: usize) -> i64 {
        let idx = row * 3 + col;
        let word = self.c[base + idx / 2];
        (if idx % 2 == 0 { word as i16 } else { (word >> 16) as i16 }) as i64
    }

    fn tr(&self, cv: usize, i: usize) -> i64 {
        match cv { 0 => self.c[5 + i] as i64, 1 => self.c[13 + i] as i64, 2 => self.c[21 + i] as i64, _ => 0 }
    }

    fn mvmva(&mut self, sf: i32, mx: usize, v: usize, cv: usize, lm: bool) {
        let (vx, vy, vz) = if v == 3 { (self.ir(1), self.ir(2), self.ir(3)) } else { (self.vx(v), self.vy(v), self.vz(v)) };
        let (t0, t1, t2) = if cv == 3 { (0, 0, 0) } else { (self.tr(cv, 0), self.tr(cv, 1), self.tr(cv, 2)) };
        if mx == 3 {
            let r = self.d[6] & 0xFF;
            self.mat[0] = ((-r) << 4) as i16 as i32;
            self.mat[1] = (r << 4) as i16 as i32;
            self.mat[2] = self.ir0() as i16 as i32;
            let rt13 = self.mel(0, 0, 2) as i32;
            let rt22 = self.mel(0, 1, 1) as i32;
            self.mat[3] = rt13; self.mat[4] = rt13; self.mat[5] = rt13;
            self.mat[6] = rt22; self.mat[7] = rt22; self.mat[8] = rt22;
        } else {
            self.load_mat(if mx == 0 { 0 } else if mx == 1 { 8 } else { 16 });
        }
        for r in 0..3 {
            let mut acc = (if r == 0 { t0 } else if r == 1 { t1 } else { t2 }) << 12;
            let k = r * 3;
            let m = self.mat;
            if cv == 2 {
                // the far colour bug: the translation term is computed and saturated, then only the matrix product counts
                let p0 = acc + m[k] as i64 * vx;
                self.check_mac(r + 1, p0);
                self.sat_ir(r + 1, p0 >> sf, false);
                acc = m[k + 1] as i64 * vy + m[k + 2] as i64 * vz;
            } else {
                acc += m[k] as i64 * vx + m[k + 1] as i64 * vy + m[k + 2] as i64 * vz;
            }
            self.check_mac(r + 1, acc);
            let res = wrap44(acc) >> sf;
            self.d[25 + r] = res as i32;
            self.d[9 + r] = self.sat_ir(r + 1, res, lm);
        }
    }

    fn rtps(&mut self, vi: usize, sf: i32, lm: bool) {
        let (vx, vy, vz) = (self.vx(vi), self.vy(vi), self.vz(vi));
        let m1 = ((self.c[5] as i64) << 12) + self.mel(0, 0, 0) * vx + self.mel(0, 0, 1) * vy + self.mel(0, 0, 2) * vz;
        let m2 = ((self.c[6] as i64) << 12) + self.mel(0, 1, 0) * vx + self.mel(0, 1, 1) * vy + self.mel(0, 1, 2) * vz;
        let m3 = ((self.c[7] as i64) << 12) + self.mel(0, 2, 0) * vx + self.mel(0, 2, 1) * vy + self.mel(0, 2, 2) * vz;
        self.check_mac(1, m1);
        self.check_mac(2, m2);
        self.check_mac(3, m3);
        let r1 = wrap44(m1) >> sf;
        let r2 = wrap44(m2) >> sf;
        let r3 = wrap44(m3) >> sf;
        self.d[25] = r1 as i32;
        self.d[26] = r2 as i32;
        self.d[27] = r3 as i32;
        self.d[9] = self.sat_ir(1, r1, lm);
        self.d[10] = self.sat_ir(2, r2, lm);
        self.d[11] = self.sat_ir(3, r3, lm);
        // SZ FIFO (SZ3 is MAC3 >> 12 whatever sf is)
        let z = wrap44(m3) >> 12;
        self.d[16] = self.d[17];
        self.d[17] = self.d[18];
        self.d[18] = self.d[19];
        let sz: i32;
        if z < 0 { self.flag |= 1 << 18; sz = 0; }
        else if z > 0xFFFF { self.flag |= 1 << 18; sz = 0xFFFF; }
        else { sz = z as i32; }
        self.d[19] = sz;
        // perspective divide (the reciprocal by Newton-Raphson from a 257-entry table)
        let h = self.c[26] as u16 as i64;
        let div: i64;
        if h >= sz as i64 * 2 {
            self.flag |= 1 << 17;
            div = 0x1FFFF;
        } else {
            let mut zc = 0;
            let mut i = 15i32;
            while i >= 0 && ((sz >> i) & 1) == 0 { zc += 1; i -= 1; }
            let n = h << zc;
            let mut d = (sz as i64) << zc;
            let u = self.unr[((d - 0x7FC0) >> 7) as usize] as i64 + 0x101;
            d = (0x2000080 - (d * u)) >> 8;
            d = (0x0000080 + (d * u)) >> 8;
            div = 0x1FFFFi64.min(((n * d) + 0x8000) >> 16);
        }
        let sx = if self.wide != 0 { self.c[24] as i64 + ((self.ir(1) * div * self.wide) >> 8) } else { self.c[24] as i64 + self.ir(1) * div };
        let sy = self.c[25] as i64 + self.ir(2) * div;
        if sx > 0x7FFFFFFF || sx < -0x80000000 { self.flag |= 1 << 16; }
        if sy > 0x7FFFFFFF || sy < -0x80000000 { self.flag |= 1 << 15; }
        let mx = sx >> 16;
        let my = sy >> 16;
        let ox: i32;
        let oy: i32;
        if mx < -0x400 { self.flag |= 1 << 14; ox = -0x400; } else if mx > 0x3FF { self.flag |= 1 << 14; ox = 0x3FF; } else { ox = mx as i32; }
        if my < -0x400 { self.flag |= 1 << 13; oy = -0x400; } else if my > 0x3FF { self.flag |= 1 << 13; oy = 0x3FF; } else { oy = my as i32; }
        {
            let key = (((oy & 1023) as usize) << 10) | ((ox & 1023) as usize);
            let e = self.zrec[key];
            let st = self.stamp & 0xFFFF;
            let z = if (e >> 16) == st && (e & 0xFFFF) != sz as u32 { 0 } else { sz as u32 };
            let clamped = mx != ox as i64 || my != oy as i64;
            let fr = if clamped { 0u8 } else { ((((sx & 0xFFFF) >> 12) as u8) << 4) | (((sy & 0xFFFF) >> 12) as u8) };
            let fr = if (e >> 16) == st && self.zfrac[key] != fr { 0xFF } else { fr };
            self.zfrac[key] = fr;
            self.zrec[key] = (st << 16) | z;
            if self.log_z { self.zlog.push(key as u32 | (fr as u32) << 24); self.zlog.push((st << 16) | z); }
        }
        self.d[12] = self.d[13];
        self.d[13] = self.d[14];
        self.d[14] = (ox as u16 as i32) | (oy << 16);
        self.d[15] = self.d[14];
        // depth cue: MAC0 = DQB + DQA * div, IR0 = MAC0 >> 12 clamped to 0..0x1000
        let m0 = self.c[28] as i64 + (self.c[27] as i16 as i64) * div;
        self.d[24] = m0 as i32;
        if m0 > 0x7FFFFFFF { self.flag |= 1 << 16; } else if m0 < -0x80000000 { self.flag |= 1 << 15; }
        let mut ir0 = m0 >> 12;
        if ir0 < 0 { self.flag |= 1 << 12; ir0 = 0; } else if ir0 > 0x1000 { self.flag |= 1 << 12; ir0 = 0x1000; }
        self.d[8] = ir0 as i32;
    }

    fn nclip(&mut self) {
        let sx0 = self.d[12] as i16 as i64;
        let sy0 = (self.d[12] >> 16) as i16 as i64;
        let sx1 = self.d[13] as i16 as i64;
        let sy1 = (self.d[13] >> 16) as i16 as i64;
        let sx2 = self.d[14] as i16 as i64;
        let sy2 = (self.d[14] >> 16) as i16 as i64;
        let m = sx0 * (sy1 - sy2) + sx1 * (sy2 - sy0) + sx2 * (sy0 - sy1);
        self.d[24] = m as i32;
        if m > 0x7FFFFFFF { self.flag |= 1 << 16; } else if m < -0x80000000 { self.flag |= 1 << 15; }
    }

    fn op(&mut self, sf: i32, lm: bool) {
        let d1 = self.c[0] as i16 as i64;
        let d2 = self.c[2] as i16 as i64;
        let d3 = self.c[4] as i16 as i64;
        let (i1, i2, i3) = (self.ir(1), self.ir(2), self.ir(3));
        let m1 = d2 * i3 - d3 * i2;
        let m2 = d3 * i1 - d1 * i3;
        let m3 = d1 * i2 - d2 * i1;
        self.check_mac(1, m1);
        self.check_mac(2, m2);
        self.check_mac(3, m3);
        self.d[25] = (m1 >> sf) as i32;
        self.d[26] = (m2 >> sf) as i32;
        self.d[27] = (m3 >> sf) as i32;
        self.d[9] = self.sat_ir(1, m1 >> sf, lm);
        self.d[10] = self.sat_ir(2, m2 >> sf, lm);
        self.d[11] = self.sat_ir(3, m3 >> sf, lm);
    }

    fn sqr(&mut self, sf: i32, lm: bool) {
        for n in 1..=3 {
            let i = self.ir(n);
            let v = (i * i) >> sf;
            self.d[24 + n] = v as i32;
            self.d[8 + n] = self.sat_ir(n, v, lm);
        }
    }

    fn avsz3(&mut self) {
        let m = (self.c[29] as i16 as i64) * (self.d[17] as u16 as i64 + self.d[18] as u16 as i64 + self.d[19] as u16 as i64);
        self.d[24] = m as i32;
        let mut o = m >> 12;
        if o < 0 { self.flag |= 1 << 18; o = 0; } else if o > 0xFFFF { self.flag |= 1 << 18; o = 0xFFFF; }
        self.d[7] = o as i32;
    }

    fn avsz4(&mut self) {
        let m = (self.c[30] as i16 as i64)
            * (self.d[16] as u16 as i64 + self.d[17] as u16 as i64 + self.d[18] as u16 as i64 + self.d[19] as u16 as i64);
        self.d[24] = m as i32;
        let mut o = m >> 12;
        if o < 0 { self.flag |= 1 << 18; o = 0; } else if o > 0xFFFF { self.flag |= 1 << 18; o = 0xFFFF; }
        self.d[7] = o as i32;
    }

    fn push_mac_colour(&mut self) {
        let (r, g, b) = (self.d[25] as i64, self.d[26] as i64, self.d[27] as i64);
        self.d[20] = self.d[21];
        self.d[21] = self.d[22];
        let cr = self.sat_colour(0, r >> 4);
        let cg = self.sat_colour(1, g >> 4);
        let cb = self.sat_colour(2, b >> 4);
        let code = (self.d[6] >> 24) & 0xFF;
        self.d[22] = cr | cg << 8 | cb << 16 | code << 24;
    }

    fn mac_to_ir(&mut self, lm: bool) {
        self.d[9] = self.sat_ir(1, self.mac(1), lm);
        self.d[10] = self.sat_ir(2, self.mac(2), lm);
        self.d[11] = self.sat_ir(3, self.mac(3), lm);
    }

    // Light and colour matrices applied to a normal: IR = LLM * V, then IR = BK + LCM * IR.
    fn light_colour(&mut self, v: usize, sf: i32, lm: bool) {
        let (vx, vy, vz) = (self.vx(v), self.vy(v), self.vz(v));
        self.load_mat(8);
        for r in 0..3 {
            let k = r * 3;
            let m = self.mat;
            let acc = m[k] as i64 * vx + m[k + 1] as i64 * vy + m[k + 2] as i64 * vz;
            self.check_mac(r + 1, acc);
            self.d[25 + r] = (wrap44(acc) >> sf) as i32;
        }
        self.mac_to_ir(lm);
        self.colour_matrix_on_ir(sf, lm);
    }

    fn colour_matrix_on_ir(&mut self, sf: i32, lm: bool) {
        let (i1, i2, i3) = (self.ir(1), self.ir(2), self.ir(3));
        self.load_mat(16);
        for r in 0..3 {
            let k = r * 3;
            let m = self.mat;
            let acc = ((self.c[13 + r] as i64) << 12) + m[k] as i64 * i1 + m[k + 1] as i64 * i2 + m[k + 2] as i64 * i3;
            self.check_mac(r + 1, acc);
            self.d[25 + r] = (wrap44(acc) >> sf) as i32;
        }
        self.mac_to_ir(lm);
    }

    fn rgb(&self) -> (i64, i64, i64) {
        ((self.d[6] & 0xFF) as i64, ((self.d[6] >> 8) & 0xFF) as i64, ((self.d[6] >> 16) & 0xFF) as i64)
    }

    // colour * IR, with the far colour interpolation (depth cue)
    fn depth(&mut self, rr: i64, gg: i64, bb: i64, sf: i32, lm: bool) {
        for n in 0..3 {
            let src = if n == 0 { rr } else if n == 1 { gg } else { bb };
            let fc = self.c[21 + n] as i64;
            let t = (fc << 12) - src;
            self.check_mac(n + 1, t);
            let m = wrap44(t) >> sf;
            let satv = self.sat_ir(n + 1, m, false);
            let acc = src + self.ir0() * satv as i64;
            self.check_mac(n + 1, acc);
            self.d[25 + n] = (wrap44(acc) >> sf) as i32;
        }
        self.mac_to_ir(lm);
    }

    fn ncs(&mut self, v: usize, sf: i32, lm: bool) {
        self.light_colour(v, sf, lm);
        self.push_mac_colour();
    }

    fn ncds(&mut self, v: usize, sf: i32, lm: bool) {
        self.light_colour(v, sf, lm);
        let (r, g, b) = self.rgb();
        let (i1, i2, i3) = (self.ir(1), self.ir(2), self.ir(3));
        self.depth((r * i1) << 4, (g * i2) << 4, (b * i3) << 4, sf, lm);
        self.push_mac_colour();
    }

    fn nccs(&mut self, v: usize, sf: i32, lm: bool) {
        self.light_colour(v, sf, lm);
        let (r, g, b) = self.rgb();
        let m1 = (r * self.ir(1)) << 4;
        let m2 = (g * self.ir(2)) << 4;
        let m3 = (b * self.ir(3)) << 4;
        self.d[25] = (m1 >> sf) as i32;
        self.d[26] = (m2 >> sf) as i32;
        self.d[27] = (m3 >> sf) as i32;
        self.mac_to_ir(lm);
        self.push_mac_colour();
    }

    fn cc(&mut self, sf: i32, lm: bool) {
        self.colour_matrix_on_ir(sf, lm);
        let (rr, gg, bb) = self.rgb();
        self.d[25] = (((rr * self.ir(1)) << 4) >> sf) as i32;
        self.d[26] = (((gg * self.ir(2)) << 4) >> sf) as i32;
        self.d[27] = (((bb * self.ir(3)) << 4) >> sf) as i32;
        self.mac_to_ir(lm);
        self.push_mac_colour();
    }

    fn cdp(&mut self, sf: i32, lm: bool) {
        self.colour_matrix_on_ir(sf, lm);
        let (rr, gg, bb) = self.rgb();
        let (i1, i2, i3) = (self.ir(1), self.ir(2), self.ir(3));
        self.depth((rr * i1) << 4, (gg * i2) << 4, (bb * i3) << 4, sf, lm);
        self.push_mac_colour();
    }

    fn dcpl(&mut self, sf: i32, lm: bool) {
        let (rr, gg, bb) = self.rgb();
        let (i1, i2, i3) = (self.ir(1), self.ir(2), self.ir(3));
        self.depth((rr * i1) << 4, (gg * i2) << 4, (bb * i3) << 4, sf, lm);
        self.push_mac_colour();
    }

    fn dpcs(&mut self, sf: i32, lm: bool, fifo: bool) {
        let (rr, gg, bb) = if fifo {
            ((self.d[20] & 0xFF) as i64, ((self.d[20] >> 8) & 0xFF) as i64, ((self.d[20] >> 16) & 0xFF) as i64)
        } else {
            self.rgb()
        };
        self.depth(rr << 16, gg << 16, bb << 16, sf, lm);
        self.push_mac_colour();
    }

    fn intpl(&mut self, sf: i32, lm: bool) {
        let (i1, i2, i3) = (self.ir(1), self.ir(2), self.ir(3));
        self.depth(i1 << 12, i2 << 12, i3 << 12, sf, lm);
        self.push_mac_colour();
    }

    fn gpf(&mut self, sf: i32, lm: bool) {
        for n in 0..3 {
            let v = self.ir0() * self.ir(n + 1);
            self.check_mac(n + 1, v);
            self.d[25 + n] = (wrap44(v) >> sf) as i32;
        }
        self.mac_to_ir(lm);
        self.push_mac_colour();
    }

    fn gpl(&mut self, sf: i32, lm: bool) {
        for n in 0..3 {
            let v = ((self.d[25 + n] as i64) << sf) + self.ir0() * self.ir(n + 1);
            self.check_mac(n + 1, v);
            self.d[25 + n] = (wrap44(v) >> sf) as i32;
        }
        self.mac_to_ir(lm);
        self.push_mac_colour();
    }
}
