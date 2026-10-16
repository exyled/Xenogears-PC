// A tiny binary writer and reader for saving the whole machine at one moment and coming back to it.

pub struct W {
    pub b: Vec<u8>,
}

impl W {
    pub fn new() -> W { W { b: Vec::new() } }
    pub fn u8(&mut self, v: u8) { self.b.push(v); }
    pub fn bool(&mut self, v: bool) { self.b.push(v as u8); }
    pub fn u16(&mut self, v: u16) { self.b.extend_from_slice(&v.to_le_bytes()); }
    pub fn u32(&mut self, v: u32) { self.b.extend_from_slice(&v.to_le_bytes()); }
    pub fn i32(&mut self, v: i32) { self.b.extend_from_slice(&v.to_le_bytes()); }
    pub fn u64(&mut self, v: u64) { self.b.extend_from_slice(&v.to_le_bytes()); }
    pub fn i64(&mut self, v: i64) { self.b.extend_from_slice(&v.to_le_bytes()); }
    pub fn f32(&mut self, v: f32) { self.b.extend_from_slice(&v.to_le_bytes()); }
    pub fn f64(&mut self, v: f64) { self.b.extend_from_slice(&v.to_le_bytes()); }
    pub fn bytes(&mut self, v: &[u8]) { self.u32(v.len() as u32); self.b.extend_from_slice(v); }
    pub fn u32s(&mut self, v: &[u32]) { self.u32(v.len() as u32); for x in v { self.u32(*x); } }
    pub fn u16s(&mut self, v: &[u16]) { self.u32(v.len() as u32); for x in v { self.u16(*x); } }
    pub fn i16s(&mut self, v: &[i16]) { self.u32(v.len() as u32); for x in v { self.u16(*x as u16); } }
    pub fn i32s(&mut self, v: &[i32]) { self.u32(v.len() as u32); for x in v { self.i32(*x); } }
    pub fn string(&mut self, s: &str) { self.bytes(s.as_bytes()); }
}

pub struct R<'a> {
    b: &'a [u8],
    p: usize,
    pub ok: bool,
}

impl<'a> R<'a> {
    pub fn new(b: &'a [u8]) -> R<'a> { R { b, p: 0, ok: true } }
    fn take(&mut self, n: usize) -> &'a [u8] {
        if self.p + n > self.b.len() { self.ok = false; return &[]; }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        s
    }
    pub fn u8(&mut self) -> u8 { self.take(1).first().copied().unwrap_or(0) }
    pub fn bool(&mut self) -> bool { self.u8() != 0 }
    pub fn u16(&mut self) -> u16 { let s = self.take(2); if s.len() < 2 { 0 } else { u16::from_le_bytes([s[0], s[1]]) } }
    pub fn u32(&mut self) -> u32 { let s = self.take(4); if s.len() < 4 { 0 } else { u32::from_le_bytes([s[0], s[1], s[2], s[3]]) } }
    pub fn i32(&mut self) -> i32 { self.u32() as i32 }
    pub fn u64(&mut self) -> u64 { let s = self.take(8); if s.len() < 8 { 0 } else { u64::from_le_bytes(s.try_into().unwrap()) } }
    pub fn i64(&mut self) -> i64 { self.u64() as i64 }
    pub fn f32(&mut self) -> f32 { f32::from_bits(self.u32()) }
    pub fn f64(&mut self) -> f64 { f64::from_bits(self.u64()) }
    pub fn bytes(&mut self) -> Vec<u8> { let n = self.u32() as usize; self.take(n).to_vec() }
    pub fn u32s(&mut self) -> Vec<u32> { let n = self.u32() as usize; (0..n).map(|_| self.u32()).collect() }
    pub fn u16s(&mut self) -> Vec<u16> { let n = self.u32() as usize; (0..n).map(|_| self.u16()).collect() }
    pub fn i16s(&mut self) -> Vec<i16> { let n = self.u32() as usize; (0..n).map(|_| self.u16() as i16).collect() }
    pub fn i32s(&mut self) -> Vec<i32> { let n = self.u32() as usize; (0..n).map(|_| self.i32()).collect() }
    pub fn string(&mut self) -> String { String::from_utf8_lossy(&self.bytes()).into_owned() }
}
