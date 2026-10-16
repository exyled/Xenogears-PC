// Upscaled movies: tools/movies.py turns each movie into <entry>.xgv (the frames as baseline JPEG, numbered as in the movie's own
// sector headers). While the program plays a movie, the frame whose chunks it read last is looked up here and shown in place of
// the chip's 320 x 224 picture. The frames are decoded on a thread of their own, so a frame never makes the game wait.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// One decoded frame: RGB, 3 bytes a pixel.
pub struct Frame {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>,
}

struct Pack {
    file: std::fs::File,
    numbers: Vec<u32>,
    /// where each frame's JPEG is: (offset, length)
    spans: Vec<(u64, u64)>,
}

/// The chunks of an AVI file: (offset, length) of the video frames and of the sound blocks, in file order.
struct AviIndex {
    video: Vec<(u64, u64)>,
    audio: Vec<(u64, u64)>,
    rate: u32,
    channels: u16,
}

fn avi_index(f: &mut std::fs::File) -> Option<AviIndex> {
    let mut head = [0u8; 12];
    f.seek(SeekFrom::Start(0)).ok()?;
    f.read_exact(&mut head).ok()?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"AVI " { return None; }
    let end = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as u64 + 8;
    let (mut movi, mut idx1) = (0u64, (0u64, 0u64));
    let (mut rate, mut channels) = (0u32, 0u16);
    let mut at = 12u64;
    // the top level: LIST hdrl (the sound format is in it), LIST movi, idx1
    while at + 8 <= end {
        let mut h = [0u8; 12];
        f.seek(SeekFrom::Start(at)).ok()?;
        if f.read(&mut h[..8]).ok()? < 8 { break; }
        let size = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as u64;
        if &h[0..4] == b"LIST" {
            f.read_exact(&mut h[8..12]).ok()?;
            if &h[8..12] == b"movi" { movi = at + 8; }
            if &h[8..12] == b"hdrl" {
                let mut body = vec![0u8; size as usize - 4];
                f.read_exact(&mut body).ok()?;
                // the sound stream's format chunk: tag 1 (PCM), channels, rate
                let mut k = 0;
                while k + 24 <= body.len() {
                    if &body[k..k + 4] == b"strf" {
                        let tag = u16::from_le_bytes([body[k + 8], body[k + 9]]);
                        if tag == 1 {
                            channels = u16::from_le_bytes([body[k + 10], body[k + 11]]);
                            rate = u32::from_le_bytes([body[k + 12], body[k + 13], body[k + 14], body[k + 15]]);
                        }
                    }
                    k += 1;
                }
            }
        } else if &h[0..4] == b"idx1" {
            idx1 = (at + 8, size);
        }
        at += 8 + size + (size & 1);
    }
    if movi == 0 || idx1.1 == 0 { return None; }
    let mut raw = vec![0u8; idx1.1 as usize];
    f.seek(SeekFrom::Start(idx1.0)).ok()?;
    f.read_exact(&mut raw).ok()?;
    let mut video = Vec::new();
    let mut audio = Vec::new();
    // the offsets count from the 'movi' tag in most files, from the file's start in a few
    for pass in 0..2 {
        let base = if pass == 0 { movi } else { 0 };
        video.clear();
        audio.clear();
        for e in raw.chunks_exact(16) {
            let off = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as u64 + base;
            let len = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as u64;
            if &e[2..4] == b"dc" { video.push((off + 8, len)); }
            else if &e[2..4] == b"wb" { audio.push((off + 8, len)); }
        }
        // the first chunk must be where the index says
        let mut ok = true;
        if let Some(&(o, _)) = video.first() {
            let mut tag = [0u8; 4];
            if f.seek(SeekFrom::Start(o - 8)).is_err() || f.read_exact(&mut tag).is_err() || &tag[2..4] != b"dc" { ok = false; }
        }
        if ok { break; }
    }
    Some(AviIndex { video, audio, rate, channels })
}

/// The sound of an AVI file as 16-bit samples (interleaved when stereo), with its rate and channel count.
pub fn avi_audio(path: &std::path::Path) -> Option<(Vec<i16>, u32, u16)> {
    let mut f = std::fs::File::open(path).ok()?;
    let ix = avi_index(&mut f)?;
    if ix.audio.is_empty() || ix.channels == 0 { return None; }
    let mut pcm: Vec<i16> = Vec::with_capacity(ix.audio.iter().map(|a| a.1 as usize / 2).sum());
    let mut buf = Vec::new();
    for (o, l) in &ix.audio {
        buf.resize(*l as usize, 0u8);
        f.seek(SeekFrom::Start(*o)).ok()?;
        f.read_exact(&mut buf).ok()?;
        pcm.extend(buf.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
    }
    Some((pcm, ix.rate, ix.channels))
}

impl Pack {
    fn open(path: &PathBuf) -> Option<Pack> {
        let mut f = std::fs::File::open(path).ok()?;
        if path.extension().map(|e| e == "avi").unwrap_or(false) {
            let ix = avi_index(&mut f)?;
            let numbers = (1..=ix.video.len() as u32).collect();
            return Some(Pack { file: f, numbers, spans: ix.video });
        }
        let mut head = [0u8; 12];
        f.read_exact(&mut head).ok()?;
        if &head[0..4] != b"XGV1" { return None; }
        let count = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
        let mut buf = vec![0u8; count * 4 + (count + 1) * 4];
        f.read_exact(&mut buf).ok()?;
        let rd = |i: usize| u32::from_le_bytes([buf[i * 4], buf[i * 4 + 1], buf[i * 4 + 2], buf[i * 4 + 3]]);
        let numbers = (0..count).map(rd).collect();
        let offsets: Vec<u32> = (count..count * 2 + 1).map(rd).collect();
        let spans = (0..count).map(|i| (offsets[i] as u64, (offsets[i + 1] as u64).saturating_sub(offsets[i] as u64))).collect();
        Some(Pack { file: f, numbers, spans })
    }

    fn index_of(&self, number: u32) -> Option<usize> {
        // the numbers rise; the nearest frame at or before a number that the movie skipped
        match self.numbers.binary_search(&number) {
            Ok(i) => Some(i),
            Err(0) => None,
            Err(i) => Some(i - 1),
        }
    }

    fn read(&mut self, i: usize) -> Option<Vec<u8>> {
        let (a, l) = *self.spans.get(i)?;
        if l == 0 { return None; }
        self.file.seek(SeekFrom::Start(a)).ok()?;
        let mut v = vec![0u8; l as usize];
        self.file.read_exact(&mut v).ok()?;
        Some(v)
    }
}

/// The file with a movie's pictures: a replacement (.xgv) in a folder ahead of the game's own Movies folder, else the .avi.
fn open_pack(dirs: &[PathBuf], entry: i64) -> Option<Pack> {
    for d in dirs {
        for ext in ["xgv", "avi"] {
            let p = d.join(format!("{:04}.{}", entry, ext));
            if p.exists() { return Pack::open(&p); }
        }
    }
    None
}

struct Shared {
    /// the newest finished frames: (entry, frame number) -> picture
    done: HashMap<(i64, u32), Arc<Frame>>,
    order: Vec<(i64, u32)>,
    /// entries that have no pack
    missing: Vec<i64>,
}

pub struct Movies {
    dirs: Vec<PathBuf>,
    tx: Option<Sender<(i64, u32)>>,
    shared: Arc<Mutex<Shared>>,
    asked: (i64, u32),
    /// decode on the caller's thread (headless tests run far faster than the thread could follow)
    sync: bool,
    packs: HashMap<i64, Option<Pack>>,
}

impl Movies {
    /// `game` is the Movies folder of the game's data (the movies as ordinary video files).
    pub fn new(game: Option<PathBuf>) -> Movies {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(d) = std::env::var("XG_MOVIE_REPLACE").ok().map(PathBuf::from).filter(|p| p.is_dir()) { dirs.push(d); }
        // the game's own folder counts only when video files are in it
        if let Some(d) = game.filter(|p| std::fs::read_dir(p).map(|r| r.filter_map(|e| e.ok()).any(|e| e.path().extension().map(|x| x == "avi").unwrap_or(false))).unwrap_or(false)) { dirs.push(d); }
        let shared = Arc::new(Mutex::new(Shared { done: HashMap::new(), order: Vec::new(), missing: Vec::new() }));
        let mut tx = None;
        if !dirs.is_empty() {
            let (t, r): (Sender<(i64, u32)>, Receiver<(i64, u32)>) = channel();
            let r = Arc::new(Mutex::new(r));
            for k in 0..3 {
                let (d2, s2, r2) = (dirs.clone(), shared.clone(), r.clone());
                let _ = std::thread::Builder::new().name(format!("movie-decode-{}", k)).spawn(move || worker(d2, s2, r2));
            }
            tx = Some(t);
        }
        Movies { dirs, tx, shared, asked: (-1, 0), sync: std::env::var("XG_MOVIE_SYNC").is_ok(), packs: HashMap::new() }
    }

    pub fn active(&self) -> bool { !self.dirs.is_empty() }

    /// The picture for this frame of this movie if it has been decoded (asks for it, and the next ones, if not).
    pub fn frame(&mut self, entry: i64, number: u32) -> Option<Arc<Frame>> {
        if self.sync {
            let dirs = self.dirs.clone();
            if let Ok(s) = self.shared.lock() { if let Some(f) = s.done.get(&(entry, number)) { return Some(f.clone()); } }
            let pack = self.packs.entry(entry).or_insert_with(|| open_pack(&dirs, entry)).as_mut()?;
            let i = pack.index_of(number)?;
            let bytes = pack.read(i)?;
            let t = std::time::Instant::now();
            let (w, h, rgb) = decode_jpeg(&bytes)?;
            if std::env::var("XG_MOVIE_DEBUG").is_ok() && number % 3 == 0 { eprintln!("decoded movie {} frame {} in {:?}", entry, number, t.elapsed()); }
            let f = Arc::new(Frame { w, h, rgb });
            if let Ok(mut s) = self.shared.lock() {
                s.done.insert((entry, number), f.clone());
                s.order.push((entry, number));
                while s.order.len() > 12 { let k = s.order.remove(0); s.done.remove(&k); }
            }
            return Some(f);
        }
        let tx = self.tx.as_ref()?;
        if self.asked != (entry, number) {
            self.asked = (entry, number);
            for k in 0..4 { let _ = tx.send((entry, number + k)); }
        }
        let s = self.shared.lock().ok()?;
        if let Some(f) = s.done.get(&(entry, number)) { return Some(f.clone()); }
        // not decoded yet: the last one of this movie that is
        for k in 1..6u32 {
            if number >= k { if let Some(f) = s.done.get(&(entry, number - k)) { return Some(f.clone()); } }
        }
        None
    }

    /// True when this exact frame is decoded and waiting.
    pub fn ready(&self, entry: i64, number: u32) -> bool {
        self.shared.lock().map(|s| s.done.contains_key(&(entry, number))).unwrap_or(false)
    }

    /// The program has read up to this frame: decoding of the frames shown over the next moments starts now.
    pub fn want(&mut self, entry: i64, number: u32, lag: u32) {
        if self.sync || self.asked.0 == entry && self.asked.1 >= number { return; }
        if let Some(tx) = &self.tx {
            self.asked = (entry, number);
            for k in 0..6u32 { let _ = tx.send((entry, (number + k).saturating_sub(lag + 1))); }
        }
    }

    /// True when this movie has no replacement (so the program's own picture is shown).
    pub fn lacks(&self, entry: i64) -> bool {
        if self.sync { return !self.dirs.iter().any(|d| d.join(format!("{:04}.xgv", entry)).exists() || d.join(format!("{:04}.avi", entry)).exists()); }
        self.shared.lock().map(|s| s.missing.contains(&entry)).unwrap_or(true)
    }
}

fn worker(dirs: Vec<PathBuf>, shared: Arc<Mutex<Shared>>, rx: Arc<Mutex<Receiver<(i64, u32)>>>) {
    let mut packs: HashMap<i64, Option<Pack>> = HashMap::new();
    loop {
        let (entry, number) = match rx.lock().ok().and_then(|r| r.recv().ok()) { Some(r) => r, None => return };
        if shared.lock().map(|s| s.done.contains_key(&(entry, number))).unwrap_or(true) { continue; }
        let pack = packs.entry(entry).or_insert_with(|| open_pack(&dirs, entry));
        let pack = match pack {
            Some(p) => p,
            None => {
                if let Ok(mut s) = shared.lock() { if !s.missing.contains(&entry) { s.missing.push(entry); } }
                continue;
            }
        };
        let i = match pack.index_of(number) { Some(i) => i, None => continue };
        let bytes = match pack.read(i) { Some(b) => b, None => continue };
        if let Some((w, h, rgb)) = decode_jpeg(&bytes) {
            if let Ok(mut s) = shared.lock() {
                s.done.insert((entry, number), Arc::new(Frame { w, h, rgb }));
                s.order.push((entry, number));
                while s.order.len() > 16 {
                    let k = s.order.remove(0);
                    s.done.remove(&k);
                }
            }
        }
    }
}

// ---- a baseline JPEG decoder (8 bit, Huffman, 1 or 3 components, any sampling up to 2 x 2) ----

static ZZ: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

#[derive(Clone)]
struct Huff {
    /// code length << 8 | symbol for codes of up to 9 bits (0: longer)
    look: Vec<u16>,
    maxcode: [i32; 18],
    valptr: [i32; 17],
    mincode: [i32; 17],
    vals: Vec<u8>,
}

impl Default for Huff {
    fn default() -> Huff { Huff { look: vec![0; 512], maxcode: [-1; 18], valptr: [0; 17], mincode: [0; 17], vals: Vec::new() } }
}

impl Huff {
    fn new(counts: &[u8], vals: &[u8]) -> Huff {
        let mut h = Huff { vals: vals.to_vec(), ..Default::default() };
        let mut code = 0i32;
        let mut k = 0i32;
        for l in 1..=16usize {
            h.valptr[l] = k;
            h.mincode[l] = code;
            for n in 0..counts[l - 1] as i32 {
                if l <= 9 {
                    let sym = vals.get((k + n) as usize).copied().unwrap_or(0) as u16;
                    let first = ((code + n) << (9 - l)) as usize;
                    for f in 0..(1usize << (9 - l)) {
                        if first + f < 512 { h.look[first + f] = (l as u16) << 8 | sym; }
                    }
                }
            }
            code += counts[l - 1] as i32;
            k += counts[l - 1] as i32;
            h.maxcode[l] = if counts[l - 1] > 0 { code - 1 } else { -1 };
            code <<= 1;
        }
        h.maxcode[17] = i32::MAX;
        h
    }
}

struct Bits<'a> {
    d: &'a [u8],
    p: usize,
    acc: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    #[inline]
    fn refill(&mut self) {
        while self.n <= 56 {
            let mut b = 0u8;
            if self.p < self.d.len() {
                b = self.d[self.p];
                if b == 0xFF {
                    let m = if self.p + 1 < self.d.len() { self.d[self.p + 1] } else { 0xD9 };
                    if m == 0 { self.p += 2; } else { b = 0; }   // a marker: zeros, and stay in front of it
                } else {
                    self.p += 1;
                }
            }
            self.acc |= (b as u64) << (56 - self.n);
            self.n += 8;
        }
    }

    #[inline]
    fn bits(&mut self, c: u32) -> i32 {
        if c == 0 { return 0; }
        if self.n < c { self.refill(); }
        let v = (self.acc >> (64 - c)) as i32;
        self.acc <<= c;
        self.n -= c;
        v
    }

    #[inline]
    fn decode(&mut self, h: &Huff) -> Option<u8> {
        if self.n < 16 { self.refill(); }
        let e = h.look[(self.acc >> 55) as usize];
        if e != 0 {
            let l = (e >> 8) as u32;
            self.acc <<= l;
            self.n -= l;
            return Some(e as u8);
        }
        for l in 10..=16usize {
            let code = (self.acc >> (64 - l)) as i32;
            if h.maxcode[l] >= 0 && code <= h.maxcode[l] && code >= h.mincode[l] {
                self.acc <<= l;
                self.n -= l as u32;
                return h.vals.get((h.valptr[l] + code - h.mincode[l]) as usize).copied();
            }
        }
        None
    }

    fn restart(&mut self) {
        self.acc = 0;
        self.n = 0;
        while self.p + 1 < self.d.len() {
            if self.d[self.p] == 0xFF && (0xD0..=0xD7).contains(&self.d[self.p + 1]) { self.p += 2; return; }
            self.p += 1;
        }
    }
}

fn extend(v: i32, t: u32) -> i32 {
    if t == 0 { 0 } else if v < (1 << (t - 1)) { v - (1 << t) + 1 } else { v }
}

struct Comp {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    td: usize,
    ta: usize,
    plane: Vec<u8>,
    pw: usize,
    pred: i32,
}

pub fn decode_jpeg(d: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    if d.len() < 4 || d[0] != 0xFF || d[1] != 0xD8 { return None; }
    let mut p = 2;
    let mut q = [[0u16; 64]; 4];
    let mut dc: [Huff; 4] = Default::default();
    let mut ac: [Huff; 4] = Default::default();
    let mut comps: Vec<Comp> = Vec::new();
    let (mut w, mut h) = (0usize, 0usize);
    let mut ri = 0usize;
    loop {
        if p + 4 > d.len() { return None; }
        if d[p] != 0xFF { p += 1; continue; }
        let m = d[p + 1];
        if m == 0xFF { p += 1; continue; }
        p += 2;
        if m == 0xD8 || (0xD0..=0xD7).contains(&m) || m == 0x01 { continue; }
        if m == 0xD9 { return None; }
        let len = u16::from_be_bytes([d[p], d[p + 1]]) as usize;
        let seg = d.get(p + 2..p + len)?;
        match m {
            0xDB => {
                let mut i = 0;
                while i < seg.len() {
                    let (pq, tq) = (seg[i] >> 4, (seg[i] & 15) as usize);
                    i += 1;
                    for k in 0..64 {
                        let v = if pq == 0 { let v = *seg.get(i)? as u16; i += 1; v } else { let v = u16::from_be_bytes([*seg.get(i)?, *seg.get(i + 1)?]); i += 2; v };
                        if tq < 4 { q[tq][ZZ[k]] = v; }
                    }
                }
            }
            0xC4 => {
                let mut i = 0;
                while i + 17 <= seg.len() {
                    let (tc, th) = (seg[i] >> 4, (seg[i] & 15) as usize);
                    let counts = &seg[i + 1..i + 17];
                    let n: usize = counts.iter().map(|&c| c as usize).sum();
                    let vals = seg.get(i + 17..i + 17 + n)?;
                    if th < 4 { if tc == 0 { dc[th] = Huff::new(counts, vals); } else { ac[th] = Huff::new(counts, vals); } }
                    i += 17 + n;
                }
            }
            0xC0 | 0xC1 => {
                if seg[0] != 8 { return None; }
                h = u16::from_be_bytes([seg[1], seg[2]]) as usize;
                w = u16::from_be_bytes([seg[3], seg[4]]) as usize;
                let n = seg[5] as usize;
                for i in 0..n {
                    let b = seg.get(6 + i * 3..9 + i * 3)?;
                    comps.push(Comp { id: b[0], h: (b[1] >> 4) as usize, v: (b[1] & 15) as usize, tq: (b[2] & 3) as usize, td: 0, ta: 0, plane: Vec::new(), pw: 0, pred: 0 });
                }
            }
            0xC2 | 0xC3 | 0xC5..=0xCF => return None,
            0xDD => { ri = u16::from_be_bytes([seg[0], seg[1]]) as usize; }
            0xDA => {
                let n = seg[0] as usize;
                for i in 0..n {
                    let (cid, t) = (seg[1 + i * 2], seg[2 + i * 2]);
                    if let Some(c) = comps.iter_mut().find(|c| c.id == cid) { c.td = (t >> 4) as usize & 3; c.ta = (t & 15) as usize & 3; }
                }
                p += len;
                break;
            }
            _ => {}
        }
        p += len;
    }
    if w == 0 || h == 0 || comps.is_empty() || comps.len() > 3 { return None; }
    let hmax = comps.iter().map(|c| c.h).max()?;
    let vmax = comps.iter().map(|c| c.v).max()?;
    if hmax == 0 || vmax == 0 { return None; }
    let mcux = (w + 8 * hmax - 1) / (8 * hmax);
    let mcuy = (h + 8 * vmax - 1) / (8 * vmax);
    for c in comps.iter_mut() {
        c.pw = mcux * c.h * 8;
        c.plane = vec![0u8; c.pw * mcuy * c.v * 8];
    }
    // dequantisation factors with the transform's scaling folded in (natural order)
    let mut qs = [[0f32; 64]; 4];
    for t in 0..4 { for z in 0..64 { qs[t][z] = q[t][z] as f32 * AAN[z >> 3] * AAN[z & 7] * 0.125; } }
    let mut br = Bits { d, p, acc: 0, n: 0 };
    let mut done_mcus = 0usize;
    let ncomp = comps.len();
    for my in 0..mcuy {
        for mx in 0..mcux {
            if ri > 0 && done_mcus > 0 && done_mcus % ri == 0 {
                br.restart();
                for c in comps.iter_mut() { c.pred = 0; }
            }
            done_mcus += 1;
            for ci in 0..ncomp {
                let (ch, cv, tq, td, ta) = (comps[ci].h, comps[ci].v, comps[ci].tq, comps[ci].td, comps[ci].ta);
                for by in 0..cv {
                    for bx in 0..ch {
                        let mut blk = [0f32; 64];
                        let t = br.decode(&dc[td])? as u32;
                        let diff = extend(br.bits(t), t);
                        comps[ci].pred += diff;
                        blk[0] = comps[ci].pred as f32 * qs[tq][0];
                        let mut k = 1;
                        let mut ac_seen = false;
                        while k < 64 {
                            let rs = br.decode(&ac[ta])?;
                            let (r, s) = ((rs >> 4) as usize, (rs & 15) as u32);
                            if s == 0 {
                                if r == 15 { k += 16; continue; }
                                break;
                            }
                            k += r;
                            if k > 63 { break; }
                            let v = extend(br.bits(s), s);
                            let z = ZZ[k];
                            blk[z] = v as f32 * qs[tq][z];
                            ac_seen = true;
                            k += 1;
                        }
                        let pw = comps[ci].pw;
                        let ox = (mx * ch + bx) * 8;
                        let oy = (my * cv + by) * 8;
                        if !ac_seen {
                            // only the DC term: a flat block
                            let v = (blk[0] + 128.5).clamp(0.0, 255.0) as u8;
                            for y in 0..8 {
                                let o = (oy + y) * pw + ox;
                                comps[ci].plane[o..o + 8].fill(v);
                            }
                            continue;
                        }
                        idct_aan(&mut blk);
                        for y in 0..8 {
                            let row = &mut comps[ci].plane[(oy + y) * pw + ox..(oy + y) * pw + ox + 8];
                            for x in 0..8 { row[x] = (blk[y * 8 + x] + 128.5).clamp(0.0, 255.0) as u8; }
                        }
                    }
                }
            }
        }
    }
    let mut rgb = vec![0u8; w * h * 3];
    if ncomp == 1 {
        for y in 0..h {
            for x in 0..w {
                let l = comps[0].plane[y * comps[0].pw + x];
                let o = (y * w + x) * 3;
                rgb[o] = l; rgb[o + 1] = l; rgb[o + 2] = l;
            }
        }
        return Some((w, h, rgb));
    }
    // per component: the plane column and row each picture column and row reads (the chroma planes are smaller)
    let xmap: Vec<[usize; 3]> = (0..w).map(|x| [x * comps[0].h / hmax, x * comps[1].h / hmax, x * comps[2].h / hmax]).collect();
    for y in 0..h {
        let ry = [y * comps[0].v / vmax, y * comps[1].v / vmax, y * comps[2].v / vmax];
        let (r0, r1, r2) = (
            &comps[0].plane[ry[0] * comps[0].pw..],
            &comps[1].plane[ry[1] * comps[1].pw..],
            &comps[2].plane[ry[2] * comps[2].pw..],
        );
        let out = &mut rgb[y * w * 3..(y + 1) * w * 3];
        for (x, m) in xmap.iter().enumerate() {
            let yy = r0[m[0]] as i32;
            let cb = r1[m[1]] as i32 - 128;
            let cr = r2[m[2]] as i32 - 128;
            let o = x * 3;
            out[o] = (yy + ((91881 * cr) >> 16)).clamp(0, 255) as u8;
            out[o + 1] = (yy - ((22554 * cb + 46802 * cr) >> 16)).clamp(0, 255) as u8;
            out[o + 2] = (yy + ((116130 * cb) >> 16)).clamp(0, 255) as u8;
        }
    }
    Some((w, h, rgb))
}

static AAN: [f32; 8] = [1.0, 1.387039845, 1.306562965, 1.175875602, 1.0, 0.785694958, 0.541196100, 0.275899379];

/// The inverse transform of one block whose coefficients were scaled by AAN[row] * AAN[column] / 8 (the usual float "AAN" form).
fn idct_aan(b: &mut [f32; 64]) {
    for c in 0..8 {
        let (t0, t1, t2, t3) = (b[c], b[16 + c], b[32 + c], b[48 + c]);
        let (t10, t11) = (t0 + t2, t0 - t2);
        let t13 = t1 + t3;
        let t12 = (t1 - t3) * 1.414213562 - t13;
        let (t0, t3, t1, t2) = (t10 + t13, t10 - t13, t11 + t12, t11 - t12);
        let (t4, t5, t6, t7) = (b[8 + c], b[24 + c], b[40 + c], b[56 + c]);
        let (z13, z10, z11, z12) = (t6 + t5, t6 - t5, t4 + t7, t4 - t7);
        let t7 = z11 + z13;
        let t11 = (z11 - z13) * 1.414213562;
        let z5 = (z10 + z12) * 1.847759065;
        let t10 = z5 - z12 * 1.082392200;
        let t12 = z5 - z10 * 2.613125930;
        let t6 = t12 - t7;
        let t5 = t11 - t6;
        let t4 = t10 - t5;
        b[c] = t0 + t7; b[56 + c] = t0 - t7;
        b[8 + c] = t1 + t6; b[48 + c] = t1 - t6;
        b[16 + c] = t2 + t5; b[40 + c] = t2 - t5;
        b[24 + c] = t3 + t4; b[32 + c] = t3 - t4;
    }
    for r in 0..8 {
        let o = r * 8;
        let (t0, t1, t2, t3) = (b[o], b[o + 2], b[o + 4], b[o + 6]);
        let (t10, t11) = (t0 + t2, t0 - t2);
        let t13 = t1 + t3;
        let t12 = (t1 - t3) * 1.414213562 - t13;
        let (t0, t3, t1, t2) = (t10 + t13, t10 - t13, t11 + t12, t11 - t12);
        let (t4, t5, t6, t7) = (b[o + 1], b[o + 3], b[o + 5], b[o + 7]);
        let (z13, z10, z11, z12) = (t6 + t5, t6 - t5, t4 + t7, t4 - t7);
        let t7 = z11 + z13;
        let t11 = (z11 - z13) * 1.414213562;
        let z5 = (z10 + z12) * 1.847759065;
        let t10 = z5 - z12 * 1.082392200;
        let t12 = z5 - z10 * 2.613125930;
        let t6 = t12 - t7;
        let t5 = t11 - t6;
        let t4 = t10 - t5;
        b[o] = t0 + t7; b[o + 7] = t0 - t7;
        b[o + 1] = t1 + t6; b[o + 6] = t1 - t6;
        b[o + 2] = t2 + t5; b[o + 5] = t2 - t5;
        b[o + 3] = t3 + t4; b[o + 4] = t3 - t4;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aan_matches_the_matrix_transform() {
        let mut seed = 12345u32;
        let mut rnd = || { seed = seed.wrapping_mul(1664525).wrapping_add(1013904223); ((seed >> 16) as i32 % 200 - 100) as f32 };
        for _ in 0..50 {
            let mut c = [0f32; 64];
            for k in 0..64 { c[k] = if k < 20 { rnd() } else { 0.0 }; }
            // reference: separable matrix transform
            let mut tmp = [0f32; 64];
            let cs = |x: usize, u: usize| (if u == 0 { 0.5f32.sqrt() } else { 1.0 }) * 0.5 * (((2 * x + 1) * u) as f32 * std::f32::consts::PI / 16.0).cos();
            for y in 0..8 { for x in 0..8 { let mut s = 0.0; for u in 0..8 { s += cs(x, u) * c[y * 8 + u]; } tmp[y * 8 + x] = s; } }
            let mut r = [0f32; 64];
            for x in 0..8 { for y in 0..8 { let mut s = 0.0; for v in 0..8 { s += cs(y, v) * tmp[v * 8 + x]; } r[y * 8 + x] = s; } }
            let mut b = [0f32; 64];
            for k in 0..64 { b[k] = c[k] * AAN[k >> 3] * AAN[k & 7] * 0.125; }
            idct_aan(&mut b);
            for k in 0..64 { assert!((b[k] - r[k]).abs() < 0.05, "{} {} {}", k, b[k], r[k]); }
        }
    }

    #[test]
    #[ignore]
    fn decode_speed() {
        let dir = std::env::var("XG_MOVIE_REPLACE").unwrap_or_else(|_| "../../project/MovieReplace".into());
        let mut pack = match Pack::open(&PathBuf::from(dir).join("0014.xgv")) { Some(p) => p, None => return };
        let frames: Vec<Vec<u8>> = (100..140).filter_map(|i| pack.read(i)).collect();
        let t = std::time::Instant::now();
        let mut px = 0usize;
        for f in &frames { let (w, h, _) = decode_jpeg(f).unwrap(); px += w * h; }
        let per = t.elapsed().as_secs_f64() * 1000.0 / frames.len() as f64;
        println!("decode: {:.1} ms a frame ({} frames, {} MB each on average)", per, frames.len(), frames.iter().map(|f| f.len()).sum::<usize>() / frames.len() / 1048576);
        let _ = px;
    }
}
