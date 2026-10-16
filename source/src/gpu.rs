// The console's graphics processor, as software: a 1024 x 512 video memory of 15-bit pixels and the drawing commands the
// game's ordering tables carry (flat, shaded and textured triangles and quads, sprites, rectangles, fills, lines, and the
// copies between memory and video memory).

use crate::mem::Mem;

mod accel;
pub(crate) use accel::wait_mapped;
mod hi;
mod thr;
mod title;

pub const WIDTH: i32 = 1024;
pub const HEIGHT: i32 = 512;

pub const OP_PACKET: u32 = 1;
pub const OP_LOAD: u32 = 2;
pub const OP_FILL: u32 = 3;
pub const OP_ZWRITES: u32 = 4;
pub const OP_STAMP: u32 = 5;
pub const OP_CONFIG: u32 = 6;
pub const OP_PRESENT: u32 = 7;

static DITHER: [i32; 16] = [-4, 0, -3, 1, 2, -2, 3, -1, -3, 1, -4, 0, 3, -1, 2, -2];

pub(crate) const WIDE_W: i32 = 428;

pub struct Gpu {
    pub vram: Vec<u16>,
    tp_x: i32,
    tp_y: i32,
    tp_mode: i32,
    tp_depth: i32,
    dither: bool,
    win_mask_x: i32,
    win_mask_y: i32,
    win_off_x: i32,
    win_off_y: i32,
    area_x1: i32,
    area_y1: i32,
    area_x2: i32,
    area_y2: i32,
    off_x: i32,
    off_y: i32,
    pub disp_x: i32,
    pub disp_y: i32,
    pub disp_w: i32,
    pub disp_h: i32,
    pub line_top: i32,
    pub line_bottom: i32,
    pub display24: bool,
    pub primitives: i64,
    vx: [i32; 4],
    vy: [i32; 4],
    u: [i32; 4],
    v: [i32; 4],
    col: [u32; 4],
    pub scale: i32,
    pub filter: i32,
    pub hi_on: bool,
    /// the large picture is drawn by the graphics card (accel.rs)
    pub acc_on: bool,
    pub use_gpu: bool,
    acc: Option<Box<accel::Accel>>,
    pub title: title::TitleMenu,
    /// the picture size was chosen by hand, so the window must not change it
    pub size_locked: bool,
    /// a picture setting was changed (the window saves them)
    pub settings_changed: bool,
    /// the title's fourth entry asked for the settings screen (the host takes the request)
    pub settings_request: bool,
    /// replacement pictures of texture pages: page key -> layer, the keys of the pages read lately, the dump folder and what is
    /// in it already (tools/enhance.py makes the replacements from the dump)
    pub(crate) repl_files: std::collections::HashMap<u64, std::path::PathBuf>,
    pub(crate) repl_slots: Vec<(u64, u64)>,
    pub(crate) repl_tick: u64,
    /// 16:9 3D scenes: the picture is also drawn 428 pixels wide (see accel.rs): the scene is asked for by the program's mode,
    /// the buffers are the draw buffers' top rows with whether their wide copy is complete, and whether the picture just read is wide
    pub wide_scene: bool,
    pub(crate) wide_buf: Vec<(i32, bool)>,
    pub(crate) wide_pass: u8,
    pub wide_shown: bool,
    /// 16:9 3D scenes: the console's own picture has the interface squeezed to three quarters of its width, like the geometry coprocessor's
    /// squeeze of the scene (the program culls with the squeezed positions, so it draws what a wider view shows)
    pub hud_squeeze: bool,
    /// replacement pages are read from disk on a thread of their own: (requests, finished reads, keys asked for, uploads this frame, frame)
    pub(crate) repl_io: Option<(std::sync::Mutex<std::sync::mpsc::Sender<(u64, std::path::PathBuf)>>, std::sync::Mutex<std::sync::mpsc::Receiver<(u64, Vec<u8>)>>)>,
    pub(crate) repl_pending: std::collections::HashSet<u64>,
    pub(crate) repl_budget: (u64, u32),
    pub(crate) repl_cache: Vec<((i32, i32, i32, i32), u64)>,
    pub(crate) dump_dir: Option<std::path::PathBuf>,
    pub(crate) dumped: std::collections::HashSet<u64>,
    pub(crate) repl_seen: std::collections::HashMap<u64, (u32, u32)>,
    /// the enhanced pictures are used (on when there are any); and the pages are collected into the dump folder
    pub repl_on: bool,
    /// the large picture is drawn by the worker threads (this copy keeps only the program's own video memory)
    pub threaded: bool,
    rec: Vec<u32>,
    rend: Option<Box<thr::Renderer>>,
    band_i: i32,
    band_n: i32,
    pub zown: Vec<u32>,
    pub zt: *const u32,
    pub zf: *const u8,
    pub zfown: Vec<u8>,
    pub zstamp: u32,
    pub persp: bool,
    pub aa: bool,
    hi: Vec<u32>,
    pages: Vec<hi::TexPage>,
    page_clock: u64,
}

// floor(n / d) for n >= 0 and d > 0: a double-precision quotient, corrected by one when it is off
#[inline]
fn floor_div(n: i64, d: i64, dd: f64) -> i64 {
    let mut q = (n as f64 / dd) as i64;
    let r = n - q * d;
    if r < 0 { q -= 1; } else if r >= d { q += 1; }
    q
}

trait Rint { fn rint(self) -> f64; }
impl Rint for f64 {
    // round half to even, without a library call: adding and taking away 1.5 * 2^52 rounds to an integer in the current mode
    #[inline]
    fn rint(self) -> f64 { let m = 6755399441055744.0f64; (self + m) - m }
}

#[inline]
fn pack(r: i32, g: i32, b: i32) -> u16 { ((r >> 3) | (g >> 3) << 5 | (b >> 3) << 10) as u16 }

impl Gpu {
    pub fn new() -> Gpu {
        Gpu {
            vram: vec![0u16; (WIDTH * HEIGHT) as usize],
            tp_x: 0, tp_y: 0, tp_mode: 0, tp_depth: 0, dither: false,
            win_mask_x: 0, win_mask_y: 0, win_off_x: 0, win_off_y: 0,
            area_x1: 0, area_y1: 0, area_x2: 1023, area_y2: 511,
            off_x: 0, off_y: 0,
            disp_x: 0, disp_y: 0, disp_w: 320, disp_h: 240, line_top: 16, line_bottom: 256,
            display24: false, primitives: 0,
            vx: [0; 4], vy: [0; 4], u: [0; 4], v: [0; 4], col: [0; 4],
            scale: 1, filter: 1, hi_on: false, acc_on: false, use_gpu: false, acc: None, title: title::TitleMenu::new(), size_locked: false, settings_changed: false, settings_request: false, repl_files: std::collections::HashMap::new(), repl_slots: Vec::new(), repl_tick: 0, wide_scene: false, wide_buf: Vec::new(), wide_pass: 0, wide_shown: false, hud_squeeze: false, repl_io: None, repl_pending: std::collections::HashSet::new(), repl_budget: (u64::MAX, 0), repl_cache: Vec::new(), dump_dir: None, dumped: std::collections::HashSet::new(), repl_seen: std::collections::HashMap::new(), repl_on: true, threaded: false, rec: Vec::new(), rend: None, band_i: 0, band_n: 1, zown: Vec::new(), zfown: Vec::new(), zf: std::ptr::null(), zt: std::ptr::null(), zstamp: 0, persp: true, aa: true, hi: Vec::new(), pages: Vec::new(), page_clock: 0,
        }
    }

    pub fn save_to(&self, w: &mut crate::snap::W) {
        w.u16s(&self.vram);
        for v in [self.tp_x, self.tp_y, self.tp_mode, self.tp_depth, self.win_mask_x, self.win_mask_y, self.win_off_x, self.win_off_y,
            self.area_x1, self.area_y1, self.area_x2, self.area_y2, self.off_x, self.off_y, self.disp_x, self.disp_y, self.disp_w,
            self.disp_h, self.line_top, self.line_bottom] { w.i32(v); }
        w.bool(self.dither);
        w.bool(self.display24);
    }

    pub fn load_from(&mut self, r: &mut crate::snap::R) {
        let v = r.u16s();
        if v.len() == self.vram.len() { self.vram.copy_from_slice(&v); }
        self.tp_x = r.i32(); self.tp_y = r.i32(); self.tp_mode = r.i32(); self.tp_depth = r.i32();
        self.win_mask_x = r.i32(); self.win_mask_y = r.i32(); self.win_off_x = r.i32(); self.win_off_y = r.i32();
        self.area_x1 = r.i32(); self.area_y1 = r.i32(); self.area_x2 = r.i32(); self.area_y2 = r.i32();
        self.off_x = r.i32(); self.off_y = r.i32();
        self.disp_x = r.i32(); self.disp_y = r.i32(); self.disp_w = r.i32(); self.disp_h = r.i32();
        self.line_top = r.i32(); self.line_bottom = r.i32();
        self.dither = r.bool();
        self.display24 = r.bool();
        self.hi_reseed();
        if self.threaded { self.restart_threads(); }
    }

    // ---- transfers ----

    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, g: i32, b: i32) {
        if self.threaded {
            self.rec.extend_from_slice(&[OP_FILL << 24, x as u32, y as u32, w as u32, h as u32, (r & 255 | (g & 255) << 8 | (b & 255) << 16) as u32]);
        }
        self.fill_raw(x, y, w, h, r, g, b);
    }

    pub fn debug_area(&self) -> [i32; 4] { [self.area_x1, self.area_y1, self.area_x2, self.area_y2] }
    pub fn debug_off(&self) -> [i32; 2] { [self.off_x, self.off_y] }
    pub fn debug_disp(&self) -> [i32; 4] { [self.disp_x, self.disp_y, self.disp_w, self.disp_h] }

    fn fill_raw(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, g: i32, b: i32) {
        let c = ((r >> 3) | (g >> 3) << 5 | (b >> 3) << 10) as u16;
        for j in 0..h {
            let yy = (y + j) & 511;
            for i in 0..w {
                self.vram[(yy * WIDTH + ((x + i) & 1023)) as usize] = c;
            }
        }
        if self.hi_on { self.hi_fill(x, y, w, h, r, g, b); self.dirty(x, y, x + w - 1, y + h - 1); }
    }

    pub fn load(&mut self, m: &Mem, x: i32, y: i32, w: i32, h: i32, src: u32) {
        if self.threaded {
            let n = (w * h).max(0) as usize;
            self.rec.extend_from_slice(&[OP_LOAD << 24, x as u32, y as u32, w as u32, h as u32]);
            let mut k = 0usize;
            while k < n {
                let lo = m.read16(src.wrapping_add(2 * k as u32)) as u32;
                let hi = if k + 1 < n { m.read16(src.wrapping_add(2 * (k as u32 + 1))) as u32 } else { 0 };
                self.rec.push(lo | hi << 16);
                k += 2;
            }
        }
        self.load_with(x, y, w, h, |k| m.read16(src.wrapping_add(2 * k as u32)));
    }

    // the pixels of an image, row by row, from a source that gives pixel k
    fn load_with<F: Fn(i32) -> u16>(&mut self, x: i32, y: i32, w: i32, h: i32, get: F) {
        let mut k = 0;
        for j in 0..h {
            let yy = (y + j) & 511;
            for i in 0..w {
                let p = get(k);
                k += 1;
                self.vram[(yy * WIDTH + ((x + i) & 1023)) as usize] = p;
                if self.hi_on { self.hi_put(x + i, yy, p); }
            }
        }
        if self.hi_on { self.dirty(x, y, x + w - 1, y + h - 1); }
        if self.acc_on { self.acc_upload(x, y, w, h); }
    }

    pub fn store(&self, m: &mut Mem, x: i32, y: i32, w: i32, h: i32, mut dst: u32) {
        for j in 0..h {
            let yy = (y + j) & 511;
            for i in 0..w {
                m.write16(dst, self.vram[(yy * WIDTH + ((x + i) & 1023)) as usize]);
                dst = dst.wrapping_add(2);
            }
        }
    }

    pub fn move_rect(&mut self, sx: i32, sy: i32, w: i32, h: i32, dx: i32, dy: i32) {
        if self.hi_on { self.hi_move(sx, sy, w, h, dx, dy); self.dirty(dx, dy, dx + w - 1, dy + h - 1); }
        let mut tmp = vec![0u16; (w * h).max(0) as usize];
        for j in 0..h {
            for i in 0..w {
                tmp[(j * w + i) as usize] = self.vram[(((sy + j) & 511) * WIDTH + ((sx + i) & 1023)) as usize];
            }
        }
        for j in 0..h {
            for i in 0..w {
                self.vram[(((dy + j) & 511) * WIDTH + ((dx + i) & 1023)) as usize] = tmp[(j * w + i) as usize];
            }
        }
    }

    // ---- the ordering table ----

    /// Draws the linked list of packets starting at head (each begins with a tag: next address in the low 24 bits, word count in the top 8).
    pub fn draw_list(&mut self, m: &Mem, head: u32) {
        let mut p = head;
        let mut guard = 0;
        while guard < 200000 {
            guard += 1;
            let tag = m.read32(p);
            let words = (tag >> 24) as i32;
            if words == 9 && self.title_packet(m, p.wrapping_add(4)) {
            } else if words > 0 {
                self.execute(m, p.wrapping_add(4), words);
            }
            let next = tag & 0xFFFFFF;
            if next == 0xFFFFFF { break; }
            p = 0x80000000 | next;
        }
        self.title_overlay();
    }

    /// Runs `words` words of drawing commands from memory.
    pub fn execute(&mut self, m: &Mem, at: u32, words: i32) {
        let at0 = at & 0x1FFFFFFF;
        let wbase: i64 = if at & 3 == 0 && at0 < crate::mem::RAM_SIZE { (at0 >> 2) as i64 } else { -1 };
        let w = |i: i32| -> u32 {
            let k = wbase + i as i64;
            if wbase >= 0 && i >= 0 && (k as usize) < crate::mem::RAM_WORDS && (k as u32).wrapping_sub(crate::mem::STATE_OLD >> 2) >= crate::mem::STATE_BYTES >> 2 { return m.w[k as usize]; }
            m.read32(at.wrapping_add((i as u32).wrapping_mul(4)))
        };
        if self.threaded {
            self.rec.push(OP_PACKET << 24 | words as u32);
            for i in 0..words { let v = w(i); self.rec.push(v); }
        }
        self.execute_with(words, w);
    }

    /// Runs drawing commands given as a slice of words (what the worker threads are sent).
    pub fn execute_slice(&mut self, words: &[u32]) {
        self.execute_with(words.len() as i32, |i| if i >= 0 && (i as usize) < words.len() { words[i as usize] } else { 0 });
    }

    fn execute_with<W: Fn(i32) -> u32>(&mut self, words: i32, w: W) {
        let mut pos: i32 = 0;
        while pos < words {
            let cmd = w(pos);
            let op = (cmd >> 24) as i32;
            if op >= 0x20 && op < 0x40 {
                let quad = op & 0x08 != 0;
                let tex = op & 0x04 != 0;
                let gour = op & 0x10 != 0;
                let n = if quad { 4 } else { 3 };
                let mut clut = 0i32;
                let mut tpage: i32 = -1;
                let mut k = pos;
                for i in 0..n {
                    if i == 0 || gour { self.col[i] = w(k) & 0xFFFFFF; k += 1; } else { self.col[i] = self.col[0]; }
                    let xy = w(k); k += 1;
                    self.vx[i] = (xy & 0xFFFF) as i16 as i32;
                    self.vy[i] = (xy >> 16) as i16 as i32;
                    if tex {
                        let uv = w(k); k += 1;
                        self.u[i] = (uv & 0xFF) as i32;
                        self.v[i] = ((uv >> 8) & 0xFF) as i32;
                        if i == 0 { clut = (uv >> 16) as i32; } else if i == 1 { tpage = (uv >> 16) as i32; }
                    }
                }
                pos = k;
                if tex && tpage >= 0 { self.set_tex_page(tpage); }
                let raw = op & 1 != 0;
                let semi = op & 2 != 0;
                let (vx, vy, u, v, col) = (self.vx, self.vy, self.u, self.v, self.col);
                self.draw_tri(vx[0], vy[0], vx[1], vy[1], vx[2], vy[2], col[0], col[1], col[2], u[0], v[0], u[1], v[1], u[2], v[2], tex, raw, semi, clut);
                if quad {
                    self.draw_tri(vx[1], vy[1], vx[2], vy[2], vx[3], vy[3], col[1], col[2], col[3], u[1], v[1], u[2], v[2], u[3], v[3], tex, raw, semi, clut);
                }
                self.primitives += 1;
            } else if op >= 0x60 && op < 0x80 {
                let tex = op & 4 != 0;
                let size = (op >> 3) & 3;
                let mut k = pos + 1;
                let xy = w(k); k += 1;
                let x = (xy & 0xFFFF) as i16 as i32;
                let y = (xy >> 16) as i16 as i32;
                let (mut uu, mut vv, mut clut) = (0, 0, 0);
                if tex {
                    let uv = w(k); k += 1;
                    uu = (uv & 0xFF) as i32;
                    vv = ((uv >> 8) & 0xFF) as i32;
                    clut = (uv >> 16) as i32;
                }
                let (rw, rh);
                if size == 0 {
                    let wh = w(k); k += 1;
                    rw = (wh & 0xFFFF) as i32;
                    rh = (wh >> 16) as i32;
                } else {
                    let s = if size == 1 { 1 } else if size == 2 { 8 } else { 16 };
                    rw = s;
                    rh = s;
                }
                pos = k;
                self.draw_rect(x, y, rw, rh, cmd & 0xFFFFFF, tex, op & 1 != 0, op & 2 != 0, uu, vv, clut);
                self.primitives += 1;
            } else if op >= 0x40 && op < 0x60 {
                let gour = op & 0x10 != 0;
                let poly = op & 0x08 != 0;
                let mut k = pos;
                let c0 = w(k) & 0xFFFFFF; k += 1;
                let xy0 = w(k); k += 1;
                let mut px = (xy0 & 0xFFFF) as i16 as i32;
                let mut py = (xy0 >> 16) as i16 as i32;
                let mut cprev = c0;
                while k < words {
                    let mut c1 = c0;
                    if gour {
                        let t = w(k);
                        if poly && (t & 0xF000F000) == 0x50005000 { k += 1; break; }
                        c1 = t & 0xFFFFFF;
                        k += 1;
                    }
                    let xy1 = w(k); k += 1;
                    if poly && !gour && (xy1 & 0xF000F000) == 0x50005000 { break; }
                    let x1 = (xy1 & 0xFFFF) as i16 as i32;
                    let y1 = (xy1 >> 16) as i16 as i32;
                    self.draw_line(px, py, x1, y1, cprev, c1, op & 2 != 0);
                    px = x1; py = y1; cprev = c1;
                    if !poly { break; }
                }
                pos = k;
                self.primitives += 1;
            } else {
                match op {
                    0x02 => {
                        let xy = w(pos + 1);
                        let wh = w(pos + 2);
                        let x = (xy & 0x3F0) as i32;
                        let y = ((xy >> 16) & 0x1FF) as i32;
                        let ww = (((wh & 0x3FF) + 0xF) & !0xF) as i32;
                        let hh = ((wh >> 16) & 0x1FF) as i32;
                        self.fill_raw(x, y, ww, hh, (cmd & 0xFF) as i32, ((cmd >> 8) & 0xFF) as i32, ((cmd >> 16) & 0xFF) as i32);
                        pos += 3;
                    }
                    0x80 => {
                        let s = w(pos + 1);
                        let d = w(pos + 2);
                        let wh = w(pos + 3);
                        self.move_rect((s & 0x3FF) as i32, ((s >> 16) & 0x1FF) as i32, (wh & 0x3FF) as i32, ((wh >> 16) & 0x1FF) as i32,
                            (d & 0x3FF) as i32, ((d >> 16) & 0x1FF) as i32);
                        pos += 4;
                    }
                    0xA0 => {
                        let d = w(pos + 1);
                        let wh = w(pos + 2);
                        let x = (d & 0x3FF) as i32;
                        let y = ((d >> 16) & 0x1FF) as i32;
                        let mut ww = (wh & 0x3FF) as i32;
                        let mut hh = ((wh >> 16) & 0x1FF) as i32;
                        if ww == 0 { ww = 1024; }
                        if hh == 0 { hh = 512; }
                        let base = pos + 3;
                        self.load_with(x, y, ww, hh, |k| ((w(base + (k >> 1)) >> ((k & 1) * 16)) & 0xFFFF) as u16);
                        pos += 3 + (ww * hh + 1) / 2;
                    }
                    0xE1 => { self.set_tex_page((cmd & 0xFFFF) as i32); self.dither = cmd & 0x200 != 0; pos += 1; }
                    0xE2 => {
                        self.win_mask_x = (cmd & 0x1F) as i32 * 8;
                        self.win_mask_y = ((cmd >> 5) & 0x1F) as i32 * 8;
                        self.win_off_x = ((cmd >> 10) & 0x1F) as i32 * 8;
                        self.win_off_y = ((cmd >> 15) & 0x1F) as i32 * 8;
                        pos += 1;
                    }
                    0xE3 => { self.area_x1 = (cmd & 0x3FF) as i32; self.area_y1 = ((cmd >> 10) & 0x1FF) as i32; pos += 1; }
                    0xE4 => { self.area_x2 = (cmd & 0x3FF) as i32; self.area_y2 = ((cmd >> 10) & 0x1FF) as i32; pos += 1; }
                    0xE5 => {
                        self.off_x = (((cmd & 0x7FF) << 21) as i32) >> 21;
                        self.off_y = ((((cmd >> 11) & 0x7FF) << 21) as i32) >> 21;
                        pos += 1;
                    }
                    _ => pos += 1,
                }
            }
        }
    }

    fn set_tex_page(&mut self, tp: i32) {
        self.tp_x = (tp & 0xF) * 64;
        self.tp_y = ((tp >> 4) & 1) * 256;
        self.tp_mode = (tp >> 5) & 3;
        self.tp_depth = (tp >> 7) & 3;
    }

    // ---- rasterizing ----

    // The blend of a pixel with what is already in video memory (the semi-transparent modes).
    #[inline]
    fn blend(&mut self, i: usize, mut r: i32, mut g: i32, mut b: i32) {
        let bg = self.vram[i] as i32;
        let br = (bg & 31) << 3;
        let bgc = ((bg >> 5) & 31) << 3;
        let bb = ((bg >> 10) & 31) << 3;
        match self.tp_mode {
            0 => { r = (br + r) / 2; g = (bgc + g) / 2; b = (bb + b) / 2; }
            1 => { r += br; g += bgc; b += bb; }
            2 => { r = br - r; g = bgc - g; b = bb - b; }
            _ => { r = br + r / 4; g = bgc + g / 4; b = bb + b / 4; }
        }
        r = r.clamp(0, 255);
        g = g.clamp(0, 255);
        b = b.clamp(0, 255);
        self.vram[i] = pack(r, g, b);
    }

    fn put_pixel(&mut self, x: i32, y: i32, r: i32, g: i32, b: i32, semi: bool) {
        if x < self.area_x1 || x > self.area_x2 || y < self.area_y1 || y > self.area_y2 { return; }
        if self.hi_on { self.hi_put_pixel(x, y, r, g, b, semi); self.dirty(x, y, x, y); }
        let i = ((y & 511) * WIDTH + (x & 1023)) as usize;
        if semi { self.blend(i, r, g, b); return; }
        self.vram[i] = pack(r, g, b);
    }

    #[inline]
    fn texel(&self, ty: usize, u: i32, clut_x: i32, clut_row: i32, depth: i32) -> i32 {
        let vram = &self.vram;
        let tp_x = self.tp_x;
        if depth == 0 {
            let wd = vram[ty + ((tp_x + (u >> 2)) & 1023) as usize] as i32;
            vram[(clut_row + ((clut_x + ((wd >> ((u & 3) * 4)) & 0xF)) & 1023)) as usize] as i32
        } else if depth == 1 {
            let wd = vram[ty + ((tp_x + (u >> 1)) & 1023) as usize] as i32;
            vram[(clut_row + ((clut_x + ((wd >> ((u & 1) * 8)) & 0xFF)) & 1023)) as usize] as i32
        } else {
            vram[ty + ((tp_x + u) & 1023) as usize] as i32
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_tri(&mut self, mut x0: i32, mut y0: i32, mut x1: i32, mut y1: i32, mut x2: i32, mut y2: i32, c0: u32, c1: u32, c2: u32,
                u0: i32, v0: i32, u1: i32, v1: i32, u2: i32, v2: i32, tex: bool, raw: bool, semi: bool, clut: i32) {
        x0 += self.off_x; x1 += self.off_x; x2 += self.off_x;
        y0 += self.off_y; y1 += self.off_y; y2 += self.off_y;
        let area: i64 = (x1 - x0) as i64 * (y2 - y0) as i64 - (x2 - x0) as i64 * (y1 - y0) as i64;
        if area == 0 { return; }
        if (x0 - x1).abs() > 1023 || (x1 - x2).abs() > 1023 || (x0 - x2).abs() > 1023
            || (y0 - y1).abs() > 511 || (y1 - y2).abs() > 511 || (y0 - y2).abs() > 511 { return; }
        let min_x = x0.min(x1).min(x2).max(self.area_x1);
        let max_x = x0.max(x1).max(x2).min(self.area_x2);
        let min_y = y0.min(y1).min(y2).max(self.area_y1);
        let max_y = y0.max(y1).max(y2).min(self.area_y2);
        if min_x > max_x || min_y > max_y {
            if self.acc_on && self.wide_scene { self.wide_pass = 2; self.hi_tri([x0, x1, x2], [y0, y1, y2], [c0, c1, c2], [u0, u1, u2], [v0, v1, v2], tex, raw, semi, clut); self.wide_pass = 0; }
            return;
        }
        if self.hi_on {
            self.wide_pass = if self.acc_on && self.wide_scene { 1 } else { 0 };
            self.hi_tri([x0, x1, x2], [y0, y1, y2], [c0, c1, c2], [u0, u1, u2], [v0, v1, v2], tex, raw, semi, clut);
            self.wide_pass = 0;
            self.dirty(min_x, min_y, max_x, max_y);
        }
        let clut_x = (clut & 0x3F) * 16;
        let clut_row = ((clut >> 6) & 0x1FF) * WIDTH;
        let gouraud = c0 != c1 || c1 != c2;
        let r0 = (c0 & 0xFF) as i32;
        let g0 = ((c0 >> 8) & 0xFF) as i32;
        let b0 = ((c0 >> 16) & 0xFF) as i32;
        let pos = area > 0;

        let d0x = -((y2 - y1) as i64);
        let d1x = -((y0 - y2) as i64);
        let d2x = -((y1 - y0) as i64);
        let d0y = (x2 - x1) as i64;
        let d1y = (x0 - x2) as i64;
        let d2y = (x1 - x0) as i64;
        let mut row0 = d0y * (min_y - y1) as i64 + d0x * (min_x - x1) as i64;
        let mut row1 = d1y * (min_y - y2) as i64 + d1x * (min_x - x2) as i64;
        let mut row2 = d2y * (min_y - y0) as i64 + d2x * (min_x - x0) as i64;

        if tex && !gouraud {
            self.draw_tri_tex(min_x, max_x, min_y, max_y, d0x, d1x, d2x, d0y, d1y, d2y, row0, row1, row2, area, r0, g0, b0,
                u0, v0, u1, v1, u2, v2, raw, semi, clut_x, clut_row);
            return;
        }

        let r1 = (c1 & 0xFF) as i32;
        let g1 = ((c1 >> 8) & 0xFF) as i32;
        let b1 = ((c1 >> 16) & 0xFF) as i32;
        let r2 = (c2 & 0xFF) as i32;
        let g2 = ((c2 >> 8) & 0xFF) as i32;
        let b2 = ((c2 >> 16) & 0xFF) as i32;
        let ar = area as f64;
        let dith = self.dither && gouraud;
        let depth = self.tp_depth;
        let tp_y = self.tp_y;
        let win = self.win_mask_x != 0 || self.win_mask_y != 0;
        let (wmx, wmy, wox, woy) = (self.win_mask_x, self.win_mask_y, self.win_off_x, self.win_off_y);
        let bs: i64 = if pos { 1 } else { -1 };
        let n0 = bs * d0x;
        let n1 = bs * d1x;
        let n2 = bs * d2x;
        let n0d = n0.abs() as f64;
        let n1d = n1.abs() as f64;
        let n2d = n2.abs() as f64;
        let span = (max_x - min_x) as i64;
        let mut y = min_y;
        while y <= max_y {
            'row: {
                let mut tlo: i64 = 0;
                let mut thi: i64 = span;
                let q0 = bs * row0;
                let q1 = bs * row1;
                let q2 = bs * row2;
                if n0 > 0 { if q0 < 0 { let t = floor_div(-q0 + n0 - 1, n0, n0d); if t > tlo { tlo = t; } } }
                else if q0 < 0 { break 'row; }
                else if n0 < 0 { let t = floor_div(q0, -n0, n0d); if t < thi { thi = t; } }
                if n1 > 0 { if q1 < 0 { let t = floor_div(-q1 + n1 - 1, n1, n1d); if t > tlo { tlo = t; } } }
                else if q1 < 0 { break 'row; }
                else if n1 < 0 { let t = floor_div(q1, -n1, n1d); if t < thi { thi = t; } }
                if n2 > 0 { if q2 < 0 { let t = floor_div(-q2 + n2 - 1, n2, n2d); if t > tlo { tlo = t; } } }
                else if q2 < 0 { break 'row; }
                else if n2 < 0 { let t = floor_div(q2, -n2, n2d); if t < thi { thi = t; } }
                if tlo > thi { break 'row; }

                let mut w0 = row0 + d0x * tlo;
                let mut w1 = row1 + d1x * tlo;
                let mut w2 = row2 + d2x * tlo;
                let xe = min_x + thi as i32;
                let row_base = y * WIDTH;
                let d_row = ((y & 3) * 4) as usize;
                let mut x = min_x + tlo as i32;
                while x <= xe {
                    let (mut r, mut g, mut bl);
                    let mut tex_semi = false;
                    let mut skip = false;
                    if gouraud {
                        let a = w0 as f64 / ar;
                        let b = w1 as f64 / ar;
                        let c = w2 as f64 / ar;
                        r = (a * r0 as f64 + b * r1 as f64 + c * r2 as f64) as i32;
                        g = (a * g0 as f64 + b * g1 as f64 + c * g2 as f64) as i32;
                        bl = (a * b0 as f64 + b * b1 as f64 + c * b2 as f64) as i32;
                        if tex {
                            let mut u = (a * u0 as f64 + b * u1 as f64 + c * u2 as f64).rint() as i32;
                            let mut v = (a * v0 as f64 + b * v1 as f64 + c * v2 as f64).rint() as i32;
                            if win { u = (u & !wmx) | (wox & wmx); v = (v & !wmy) | (woy & wmy); }
                            u &= 255;
                            v &= 255;
                            let ty = (((tp_y + v) & 511) * WIDTH) as usize;
                            let t = self.texel(ty, u, clut_x, clut_row, depth);
                            if t == 0 { skip = true; } else {
                                let tr = (t & 31) << 3;
                                let tg = ((t >> 5) & 31) << 3;
                                let tb = ((t >> 10) & 31) << 3;
                                if raw { r = tr; g = tg; bl = tb; }
                                else {
                                    r = 255.min(tr * r >> 7);
                                    g = 255.min(tg * g >> 7);
                                    bl = 255.min(tb * bl >> 7);
                                }
                                tex_semi = t & 0x8000 != 0;
                            }
                        }
                    } else { r = r0; g = g0; bl = b0; }
                    if !skip {
                        if dith {
                            let d = DITHER[d_row + (x & 3) as usize];
                            r = (r + d).clamp(0, 255);
                            g = (g + d).clamp(0, 255);
                            bl = (bl + d).clamp(0, 255);
                        }
                        let idx = (row_base + x) as usize;
                        if semi && (!tex || tex_semi) { self.blend(idx, r, g, bl); }
                        else { self.vram[idx] = pack(r, g, bl); }
                    }
                    x += 1;
                    w0 += d0x; w1 += d1x; w2 += d2x;
                }
            }
            y += 1;
            row0 += d0y; row1 += d1y; row2 += d2y;
        }
    }

    // A textured triangle in a flat colour (the commonest primitive), with exact integer steps for the texture coordinates.
    #[allow(clippy::too_many_arguments)]
    fn draw_tri_tex(&mut self, min_x: i32, max_x: i32, min_y: i32, max_y: i32, d0x: i64, d1x: i64, d2x: i64, d0y: i64, d1y: i64, d2y: i64,
                    mut row0: i64, mut row1: i64, mut row2: i64, area: i64, r0: i32, g0: i32, b0: i32,
                    u0: i32, v0: i32, u1: i32, v1: i32, u2: i32, v2: i32, raw: bool, semi: bool, clut_x: i32, clut_row: i32) {
        let pos = area > 0;
        let bs: i64 = if pos { 1 } else { -1 };
        let s_area: i64 = if pos { area } else { -area };
        let ar = area as f64;
        let s_area_d = s_area as f64;
        let dith = self.dither && !raw;
        let depth = self.tp_depth;
        let tp_y = self.tp_y;
        let win = self.win_mask_x != 0 || self.win_mask_y != 0;
        let (wmx, wmy, wox, woy) = (self.win_mask_x, self.win_mask_y, self.win_off_x, self.win_off_y);
        let n0 = bs * d0x;
        let n1 = bs * d1x;
        let n2 = bs * d2x;
        let n0d = n0.abs() as f64;
        let n1d = n1.abs() as f64;
        let n2d = n2.abs() as f64;
        let span = (max_x - min_x) as i64;
        let (u0l, v0l, u1l, v1l, u2l, v2l) = (u0 as i64, v0 as i64, u1 as i64, v1 as i64, u2 as i64, v2 as i64);
        let d_nu = bs * (d0x * u0l + d1x * u1l + d2x * u2l);
        let d_nv = bs * (d0x * v0l + d1x * v1l + d2x * v2l);
        let qdu = if d_nu >= 0 { floor_div(d_nu, s_area, s_area_d) } else { -floor_div(-d_nu + s_area - 1, s_area, s_area_d) };
        let rdu = d_nu - qdu * s_area;
        let qdv = if d_nv >= 0 { floor_div(d_nv, s_area, s_area_d) } else { -floor_div(-d_nv + s_area - 1, s_area, s_area_d) };
        let rdv = d_nv - qdv * s_area;
        let mut y = min_y;
        while y <= max_y {
            'row: {
                let mut tlo: i64 = 0;
                let mut thi: i64 = span;
                let q0 = bs * row0;
                let q1 = bs * row1;
                let q2 = bs * row2;
                if n0 > 0 { if q0 < 0 { let t = floor_div(-q0 + n0 - 1, n0, n0d); if t > tlo { tlo = t; } } }
                else if q0 < 0 { break 'row; }
                else if n0 < 0 { let t = floor_div(q0, -n0, n0d); if t < thi { thi = t; } }
                if n1 > 0 { if q1 < 0 { let t = floor_div(-q1 + n1 - 1, n1, n1d); if t > tlo { tlo = t; } } }
                else if q1 < 0 { break 'row; }
                else if n1 < 0 { let t = floor_div(q1, -n1, n1d); if t < thi { thi = t; } }
                if n2 > 0 { if q2 < 0 { let t = floor_div(-q2 + n2 - 1, n2, n2d); if t > tlo { tlo = t; } } }
                else if q2 < 0 { break 'row; }
                else if n2 < 0 { let t = floor_div(q2, -n2, n2d); if t < thi { thi = t; } }
                if tlo > thi { break 'row; }

                let w0 = row0 + d0x * tlo;
                let w1 = row1 + d1x * tlo;
                let w2 = row2 + d2x * tlo;
                let nu = bs * (w0 * u0l + w1 * u1l + w2 * u2l);
                let nv = bs * (w0 * v0l + w1 * v1l + w2 * v2l);
                let mut qu = floor_div(nu, s_area, s_area_d);
                let mut ru = nu - qu * s_area;
                let mut qv = floor_div(nv, s_area, s_area_d);
                let mut rv = nv - qv * s_area;
                let xs = min_x + tlo as i32;
                let xe = min_x + thi as i32;
                let row_base = y * WIDTH;
                let d_row = ((y & 3) * 4) as usize;
                let mut x = xs;
                while x <= xe {
                    let du2 = ru * 2;
                    let dv2 = rv * 2;
                    let mut u: i32;
                    let mut v: i32;
                    if du2 == s_area || dv2 == s_area {
                        let k = (x - xs) as i64;
                        let a = (w0 + d0x * k) as f64 / ar;
                        let b = (w1 + d1x * k) as f64 / ar;
                        let c = (w2 + d2x * k) as f64 / ar;
                        u = (a * u0 as f64 + b * u1 as f64 + c * u2 as f64).rint() as i32;
                        v = (a * v0 as f64 + b * v1 as f64 + c * v2 as f64).rint() as i32;
                    } else {
                        u = (qu + (((s_area - du2) as u64) >> 63) as i64) as i32;
                        v = (qv + (((s_area - dv2) as u64) >> 63) as i64) as i32;
                    }
                    ru += rdu;
                    let ou = (s_area - 1 - ru) >> 63;
                    ru -= s_area & ou;
                    qu += qdu - ou;
                    rv += rdv;
                    let ov = (s_area - 1 - rv) >> 63;
                    rv -= s_area & ov;
                    qv += qdv - ov;

                    if win { u = (u & !wmx) | (wox & wmx); v = (v & !wmy) | (woy & wmy); }
                    u &= 255;
                    v &= 255;
                    let ty = (((tp_y + v) & 511) * WIDTH) as usize;
                    let t = self.texel(ty, u, clut_x, clut_row, depth);
                    if t != 0 {
                        let tr = (t & 31) << 3;
                        let tg = ((t >> 5) & 31) << 3;
                        let tb = ((t >> 10) & 31) << 3;
                        let (mut r, mut g, mut bl);
                        if raw { r = tr; g = tg; bl = tb; }
                        else {
                            r = 255.min(tr * r0 >> 7);
                            g = 255.min(tg * g0 >> 7);
                            bl = 255.min(tb * b0 >> 7);
                        }
                        if dith {
                            let d = DITHER[d_row + (x & 3) as usize];
                            r = (r + d).clamp(0, 255);
                            g = (g + d).clamp(0, 255);
                            bl = (bl + d).clamp(0, 255);
                        }
                        let idx = (row_base + x) as usize;
                        if semi && t & 0x8000 != 0 { self.blend(idx, r, g, bl); }
                        else { self.vram[idx] = pack(r, g, bl); }
                    }
                    x += 1;
                }
            }
            y += 1;
            row0 += d0y; row1 += d1y; row2 += d2y;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_rect(&mut self, mut x: i32, mut y: i32, w: i32, h: i32, colour: u32, tex: bool, raw: bool, semi: bool, u0: i32, v0: i32, clut: i32) {
        x += self.off_x;
        y += self.off_y;
        let clut_x = (clut & 0x3F) * 16;
        let clut_row = ((clut >> 6) & 0x1FF) * WIDTH;
        let cr = (colour & 0xFF) as i32;
        let cg = ((colour >> 8) & 0xFF) as i32;
        let cb = ((colour >> 16) & 0xFF) as i32;
        let j0 = 0.max(self.area_y1 - y);
        let j1 = (h - 1).min(self.area_y2 - y);
        let i0 = 0.max(self.area_x1 - x);
        let i1 = (w - 1).min(self.area_x2 - x);
        if j0 > j1 || i0 > i1 {
            if self.acc_on && self.wide_scene { self.wide_pass = 2; self.hi_rect(x, y, w, h, 0, h - 1, 0, w - 1, colour, tex, raw, semi, u0, v0, clut); self.wide_pass = 0; }
            return;
        }
        if self.hi_on {
            self.wide_pass = if self.acc_on && self.wide_scene { 1 } else { 0 };
            self.hi_rect(x, y, w, h, j0, j1, i0, i1, colour, tex, raw, semi, u0, v0, clut);
            self.wide_pass = 0;
            self.dirty(x + i0, y + j0, x + i1, y + j1);
        }
        if !tex {
            let flat = pack(cr, cg, cb);
            for j in j0..=j1 {
                let row_base = (y + j) * WIDTH + x;
                if semi { for i in i0..=i1 { self.blend((row_base + i) as usize, cr, cg, cb); } }
                else { for i in i0..=i1 { self.vram[(row_base + i) as usize] = flat; } }
            }
            return;
        }
        let depth = self.tp_depth;
        let tp_y = self.tp_y;
        let win = self.win_mask_x != 0 || self.win_mask_y != 0;
        let (wmx, wmy, wox, woy) = (self.win_mask_x, self.win_mask_y, self.win_off_x, self.win_off_y);
        for j in j0..=j1 {
            let mut v = v0 + j;
            if win { v = (v & !wmy) | (woy & wmy); }
            let ty = (((tp_y + (v & 255)) & 511) * WIDTH) as usize;
            let row_base = (y + j) * WIDTH + x;
            for i in i0..=i1 {
                let mut u = u0 + i;
                if win { u = (u & !wmx) | (wox & wmx); }
                u &= 255;
                let t = self.texel(ty, u, clut_x, clut_row, depth);
                if t == 0 { continue; }
                let tr = (t & 31) << 3;
                let tg = ((t >> 5) & 31) << 3;
                let tb = ((t >> 10) & 31) << 3;
                let (r, g, b);
                if raw { r = tr; g = tg; b = tb; }
                else {
                    r = 255.min(tr * cr >> 7);
                    g = 255.min(tg * cg >> 7);
                    b = 255.min(tb * cb >> 7);
                }
                let idx = (row_base + i) as usize;
                if semi && t & 0x8000 != 0 { self.blend(idx, r, g, b); }
                else { self.vram[idx] = pack(r, g, b); }
            }
        }
    }

    fn draw_line(&mut self, mut x0: i32, mut y0: i32, mut x1: i32, mut y1: i32, c0: u32, c1: u32, semi: bool) {
        x0 += self.off_x; x1 += self.off_x;
        y0 += self.off_y; y1 += self.off_y;
        let dx = (x1 - x0).abs();
        let dy = (y1 - y0).abs();
        let n = dx.max(dy);
        if n > 2048 { return; }
        let r0 = (c0 & 0xFF) as i32;
        let g0 = ((c0 >> 8) & 0xFF) as i32;
        let b0 = ((c0 >> 16) & 0xFF) as i32;
        let r1 = (c1 & 0xFF) as i32;
        let g1 = ((c1 >> 8) & 0xFF) as i32;
        let b1 = ((c1 >> 16) & 0xFF) as i32;
        for i in 0..=n {
            let t = if n == 0 { 0.0 } else { i as f64 / n as f64 };
            let x = (x0 as f64 + (x1 - x0) as f64 * t).rint() as i32;
            let y = (y0 as f64 + (y1 - y0) as f64 * t).rint() as i32;
            self.put_pixel(x, y, (r0 as f64 + (r1 - r0) as f64 * t) as i32, (g0 as f64 + (g1 - g0) as f64 * t) as i32,
                (b0 as f64 + (b1 - b0) as f64 * t) as i32, semi);
        }
    }

    // ---- the picture ----

    fn byte24(&self, row: i32, b: i32) -> u8 {
        let p = self.vram[(row + ((b >> 1) & 1023)) as usize];
        if b & 1 == 0 { p as u8 } else { (p >> 8) as u8 }
    }

    /// The display area as 0x00RRGGBB pixels (disp_w x disp_h), top row first.
    pub fn read_display(&self, out: &mut Vec<u32>) {
        let (w, h) = (self.disp_w, self.disp_h);
        out.clear();
        out.resize((w * h) as usize, 0);
        let scale = if h > 240 { 2 } else { 1 };
        let first = (self.line_top - 16) * scale;
        let last = (self.line_bottom - 16) * scale;
        let mut i = 0usize;
        for y in 0..h {
            for x in 0..w {
                if y < first || y >= last { out[i] = 0; i += 1; continue; }
                let row = ((self.disp_y + y - first) & 511) * WIDTH;
                if self.display24 {
                    let b = self.disp_x * 2 + x * 3;
                    let r = self.byte24(row, b) as u32;
                    let g = self.byte24(row, b + 1) as u32;
                    let bl = self.byte24(row, b + 2) as u32;
                    out[i] = r << 16 | g << 8 | bl;
                    i += 1;
                    continue;
                }
                let p = self.vram[(row + ((self.disp_x + x) & 1023)) as usize] as u32;
                let r = ((p & 31) << 3) | ((p & 31) >> 2);
                let g = (((p >> 5) & 31) << 3) | (((p >> 5) & 31) >> 2);
                let b = (((p >> 10) & 31) << 3) | (((p >> 10) & 31) >> 2);
                out[i] = r << 16 | g << 8 | b;
                i += 1;
            }
        }
    }
}
