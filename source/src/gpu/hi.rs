// The sharper picture: a second, larger copy of video memory (2 to 4 times the size each way, 24-bit colour) that every
// drawing command is also drawn into at the larger size. The game itself never sees it (its own 1024 x 512 memory is
// drawn exactly as before, so textures, copies and the program's logic are untouched); only the display reads it.
//   - polygons are rasterized at the larger size from the same vertices, so edges are finer and shading is smooth, with
//     no dithering and full 8-bit colour;
//   - textures are sampled between texels (filter 1: a mild filter that keeps pixel art crisp, filter 2: plain bilinear,
//     filter 0: nearest), never reaching outside the sprite or polygon's own texture rectangle, and transparent texels
//     weigh nothing, so there are no dark halos on cut-out sprites;
//   - plain copies and fills into video memory are carried over block for block.
// Texture pages are decoded to full colour once, in 32 x 32 tiles on first use, and dropped when anything is drawn over them.

use super::*;

const TILES: usize = 64;

pub struct TexPage {
    key: [i32; 4], // texture page x, y, depth, palette
    tiles: [bool; TILES],
    px: Vec<u32>, // 256 x 256: 0 = transparent, else bit 31 set, bit 30 = semi-transparent flag, then 0xRRGGBB
    age: u64,
    tex: [i32; 4], // video memory rectangle of the texels (x0, y0, x1, y1 inclusive)
    pal: [i32; 4], // and of the palette
}

#[inline]
fn expand(p: i32) -> (i32, i32, i32) {
    let r = p & 31;
    let g = (p >> 5) & 31;
    let b = (p >> 10) & 31;
    ((r << 3) | (r >> 2), (g << 3) | (g >> 2), (b << 3) | (b >> 2))
}

#[inline]
fn pack_texel(t: i32) -> u32 {
    if t == 0 { return 0; }
    let (r, g, b) = expand(t);
    0x8000_0000 | ((t as u32 & 0x8000) << 15) | (r as u32) << 16 | (g as u32) << 8 | b as u32
}

// how unlike two decoded texels are (transparent against anything counts as very unlike)
#[inline]
fn dist(a: u32, b: u32) -> i32 {
    if a == 0 && b == 0 { return 0; }
    if a == 0 || b == 0 { return 1000; }
    (((a >> 16) & 255) as i32 - ((b >> 16) & 255) as i32).abs() + (((a >> 8) & 255) as i32 - ((b >> 8) & 255) as i32).abs()
        + ((a & 255) as i32 - (b & 255) as i32).abs()
}

// floor to an integer without the library call (the target has no rounding instruction); the values are small
#[inline(always)]
fn ffloor(x: f32) -> i32 {
    let t = unsafe { x.to_int_unchecked::<i32>() };
    t - ((x < t as f32) as i32)
}

#[inline(always)]
fn unpack_texel(t: u32) -> Option<(i32, i32, i32, bool)> {
    if t == 0 { None } else { Some((((t >> 16) & 255) as i32, ((t >> 8) & 255) as i32, (t & 255) as i32, t & 0x4000_0000 != 0)) }
}


// Which diagonal of a cell the edge runs along: true when the colour is joined from the top left corner to the bottom
// right one. The two diagonals of the cell decide it; when they are about as alike as each other (a staircase, a thin
// line) the texels just outside the cell are looked at, and the diagonal whose line goes on through them wins.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn diag_main(px: *const u32, t00: u32, t10: u32, t01: u32, t11: u32, ua: i32, ub: i32, va: i32, vb: i32) -> bool {
    let d1 = dist(t00, t11);
    let d2 = dist(t10, t01);
    if (d1 - d2).abs() > 40 { return d1 <= d2; }
    let (r0, r3) = ((((va - 1) & 255) << 8) as usize, (((vb + 1) & 255) << 8) as usize);
    let (x0, x3) = (((ua - 1) & 255) as usize, ((ub + 1) & 255) as usize);
    let (tmm, tpm, tmp, tpp);
    unsafe {
        tmm = *px.add(r0 + x0);
        tpm = *px.add(r0 + x3);
        tmp = *px.add(r3 + x0);
        tpp = *px.add(r3 + x3);
    }
    let main = d1 * 2 + dist(tmm, t00) + dist(t11, tpp);
    let anti = d2 * 2 + dist(tpm, t10) + dist(t01, tmp);
    main <= anti
}

// The colour between four decoded texels of a page (256 wide), with whole numbers where the fraction is 0 or 1 so only
// the texels that count are read, and the integer blend when none is transparent.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn mix_fast(px: *const u32, ua: i32, ub: i32, va: i32, vb: i32, au: f32, av: f32, diag: bool) -> Option<(i32, i32, i32, bool)> {
    let (ua, ub, au) = if au <= 0.0 { (ua, ua, 0.0) } else if au >= 1.0 { (ub, ub, 0.0) } else { (ua, ub, au) };
    let (va, vb, av) = if av <= 0.0 { (va, va, 0.0) } else if av >= 1.0 { (vb, vb, 0.0) } else { (va, vb, av) };
    let ra = ((va & 255) << 8) as usize;
    let rb = ((vb & 255) << 8) as usize;
    let (xa, xb) = ((ua & 255) as usize, (ub & 255) as usize);
    let (t00, t10, t01, t11);
    unsafe {
        t00 = *px.add(ra + xa);
        if au == 0.0 && av == 0.0 { return unpack_texel(t00); }
        t10 = if au > 0.0 { *px.add(ra + xb) } else { t00 };
        t01 = if av > 0.0 { *px.add(rb + xa) } else { t00 };
        t11 = if au > 0.0 && av > 0.0 { *px.add(rb + xb) } else if au > 0.0 { t10 } else { t01 };
    }
    if t00 == t10 && t00 == t01 && t00 == t11 { return unpack_texel(t00); }
    let (a8, v8) = unsafe { ((au * 256.0).to_int_unchecked::<i32>(), (av * 256.0).to_int_unchecked::<i32>()) };
    let main = diag && a8 != 0 && v8 != 0 && diag_main(px, t00, t10, t01, t11, ua, ub, va, vb);
    blend_quad([t00, t10, t01, t11], a8, v8, diag, main)
}

// the blend of a cell's four texels at fractions a8, v8 (in 256ths); `main` says which diagonal the edge runs along
#[inline(always)]
fn blend_quad(t: [u32; 4], a8: i32, v8: i32, diag: bool, main: bool) -> Option<(i32, i32, i32, bool)> {
    let w: [i32; 4] = if !diag || a8 == 0 || v8 == 0 {
        let w10 = (a8 * (256 - v8)) >> 8;
        let w01 = ((256 - a8) * v8) >> 8;
        let w11 = (a8 * v8) >> 8;
        [256 - w10 - w01 - w11, w10, w01, w11]
    } else if main {
        if a8 >= v8 { [256 - a8, a8 - v8, 0, v8] } else { [256 - v8, 0, v8 - a8, a8] }
    } else if a8 + v8 <= 256 {
        [256 - a8 - v8, a8, v8, 0]
    } else {
        [0, 256 - v8, 256 - a8, a8 + v8 - 256]
    };
    let mut big = 0;
    for i in 1..4 { if w[i] > w[big] { big = i; } }
    let semi = t[big] != 0 && t[big] & 0x4000_0000 != 0;
    if t[0] != 0 && t[1] != 0 && t[2] != 0 && t[3] != 0 {
        let (mut rbs, mut gs) = (0x0080_0080u32, 0x80u32);
        for i in 0..4 {
            let k = w[i] as u32;
            rbs = rbs.wrapping_add(k.wrapping_mul(t[i] & 0x00FF_00FF));
            gs += k * ((t[i] >> 8) & 255);
        }
        let rb2 = (rbs >> 8) & 0x00FF_00FF;
        let g2 = (gs >> 8) & 255;
        return Some((((rb2 >> 16) & 255) as i32, g2 as i32, (rb2 & 255) as i32, semi));
    }
    // a transparent texel among them: colour only from the opaque ones, and nothing where they hardly count
    let (mut wr, mut wg, mut wb, mut wsum) = (0i32, 0i32, 0i32, 0i32);
    for i in 0..4 {
        if t[i] == 0 || w[i] <= 0 { continue; }
        wr += w[i] * ((t[i] >> 16) & 255) as i32;
        wg += w[i] * ((t[i] >> 8) & 255) as i32;
        wb += w[i] * (t[i] & 255) as i32;
        wsum += w[i];
    }
    if wsum < 128 { return None; }
    Some(((wr + wsum / 2) / wsum, (wg + wsum / 2) / wsum, (wb + wsum / 2) / wsum, semi))
}

#[inline]
fn overlaps(a: &[i32; 4], x0: i32, y0: i32, x1: i32, y1: i32) -> bool { a[0] <= x1 && a[2] >= x0 && a[1] <= y1 && a[3] >= y0 }

impl Gpu {
    /// Whether this thread draws the large copy's rows of video-memory row `ny` (rows are dealt out in stripes of 16).
    #[inline(always)]
    pub(super) fn owns(&self, ny: i32) -> bool {
        self.band_n <= 1 || (((ny & 511) >> 4) % self.band_n) == self.band_i
    }

    /// Chooses the picture's size factor (1 = the console's own, up to 4) and rebuilds the large copy from video memory.
    /// Starts or stops collecting the texture pages the program reads (into TextureDump beside the game data).
    pub fn set_dump(&mut self, on: bool) {
        if !on { self.dump_dir = None; return; }
        let d = std::env::var("XG_TEX_DUMP").or_else(|_| std::env::var("XG_TEX_DUMP_DEFAULT")).ok().map(std::path::PathBuf::from);
        if let Some(p) = d {
            if std::fs::create_dir_all(&p).is_ok() { self.dump_dir = Some(p); self.repl_cache.clear(); }
        }
    }

    pub fn set_scale(&mut self, s: i32) {
        let s = s.clamp(1, 4);
        self.scale = s;
        self.stop_threads();
        self.threaded = false;
        self.pages.clear();
        self.acc_on = false;
        self.acc = None;
        if s > 1 && std::env::var("XG_GPU").map(|v| v != "0").unwrap_or(true) && self.acc_start(s) {
            self.hi_on = true;
            self.hi = Vec::new();
            return;
        }
        // XG_HI1 keeps the large-copy path on at size 1 (for checking it against the program's own drawing)
        if s == 1 && std::env::var("XG_HI1").is_err() {
            self.hi_on = false;
            self.hi = Vec::new();
            return;
        }
        let n = super::thr::worker_count();
        if n > 0 {
            self.hi_on = false;
            self.hi = Vec::new();
            self.start_threads(n);
            return;
        }
        self.hi_on = true;
        self.hi = vec![0u32; (WIDTH * s * HEIGHT * s) as usize];
        self.hi_reseed();
    }

    /// the software picture knows three filters; the enhanced one (3) is the balanced one there
    pub(crate) fn fil(&self) -> i32 { if self.filter == 3 { 1 } else { self.filter } }

    pub fn set_filter(&mut self, f: i32) {
        self.filter = f.clamp(0, 3);
        let f = self.filter;
        if let Some(a) = self.acc_mut() { a.set_filter(f); }
    }

    pub fn hi_reseed(&mut self) {
        self.pages.clear();
        if self.acc_on { self.acc_reseed(); return; }
        if !self.hi_on { return; }
        for y in 0..HEIGHT {
            if !self.owns(y) { continue; }
            for x in 0..WIDTH {
                let p = self.vram[(y * WIDTH + x) as usize];
                self.hi_put(x, y, p);
            }
        }
    }

    /// Something was drawn or copied into this rectangle of video memory (inclusive): decoded textures over it are stale.
    #[inline]
    pub(super) fn dirty(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        if self.pages.is_empty() { return; }
        if x0 < 0 || y0 < 0 || x1 > 1023 || y1 > 511 { self.pages.clear(); return; }
        self.pages.retain(|p| !(overlaps(&p.tex, x0, y0, x1, y1) || overlaps(&p.pal, x0, y0, x1, y1)));
    }

    // one video-memory pixel as a block of the large copy
    #[inline]
    pub(super) fn hi_put(&mut self, x: i32, y: i32, p: u16) {
        if self.acc_on { return; }
        let (r, g, b) = expand(p as i32);
        self.hi_block(x, y, (r as u32) << 16 | (g as u32) << 8 | b as u32);
    }

    #[inline]
    pub(super) fn hi_block(&mut self, x: i32, y: i32, c: u32) {
        if !self.owns(y) { return; }
        let s = self.scale;
        let hw = WIDTH * s;
        let bx = (x & 1023) * s;
        let by = (y & 511) * s;
        for j in 0..s {
            let row = ((by + j) * hw + bx) as usize;
            for i in 0..s { self.hi[row + i as usize] = c; }
        }
    }

    pub(super) fn hi_fill(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, g: i32, b: i32) {
        if self.acc_on { self.acc_fill(x, y, w, h, r, g, b); return; }
        let c = (r as u32) << 16 | (g as u32) << 8 | b as u32;
        let s = self.scale;
        let hw = (WIDTH * s) as usize;
        for j in 0..h {
            let yy = (y + j) & 511;
            if !self.owns(yy) { continue; }
            if x >= 0 && x + w <= 1024 {
                let (a, e) = ((x * s) as usize, ((x + w) * s) as usize);
                for sy in 0..s {
                    let row = (yy * s + sy) as usize * hw;
                    self.hi[row + a..row + e].fill(c);
                }
            } else {
                for i in 0..w { self.hi_block(x + i, yy, c); }
            }
        }
    }

    // Copies a rectangle of the large copy. The source rows this thread does not draw are rebuilt from the program's own
    // video memory (which still has them, as the copy has not happened yet), at the console's detail.
    pub(super) fn hi_move(&mut self, sx: i32, sy: i32, w: i32, h: i32, dx: i32, dy: i32) {
        if self.acc_on { self.acc_move(sx, sy, w, h, dx, dy); return; }
        let s = self.scale;
        let hw = WIDTH * s;
        let (pw, ph) = ((w * s).max(0), (h * s).max(0));
        let mut tmp: Vec<u32> = vec![0u32; (pw * ph) as usize];
        for j in 0..ph {
            // only the rows this thread will write are needed
            let dyn_row = (dy + j / s) & 511;
            if !self.owns(dyn_row) { continue; }
            let srow = (sy + j / s) & 511;
            if self.owns(srow) {
                let yy = (srow * s + j % s) * hw;
                for i in 0..pw {
                    tmp[(j * pw + i) as usize] = self.hi[(yy + ((sx + i / s) & 1023) * s + i % s) as usize];
                }
            } else {
                for i in 0..pw {
                    let p = self.vram[(srow * WIDTH + ((sx + i / s) & 1023)) as usize];
                    let (r, g, b) = expand(p as i32);
                    tmp[(j * pw + i) as usize] = (r as u32) << 16 | (g as u32) << 8 | b as u32;
                }
            }
        }
        for j in 0..ph {
            let dyn_row = (dy + j / s) & 511;
            if !self.owns(dyn_row) { continue; }
            let yy = (dyn_row * s + j % s) * hw;
            for i in 0..pw {
                self.hi[(yy + ((dx + i / s) & 1023) * s + i % s) as usize] = tmp[(j * pw + i) as usize];
            }
        }
    }

    // The blend of a colour with what is in the large copy (the semi-transparent modes, in 8-bit colour).
    #[inline]
    fn hi_blend(&mut self, i: usize, mut r: i32, mut g: i32, mut b: i32) {
        let bg = self.hi[i];
        let br = ((bg >> 16) & 255) as i32;
        let bgc = ((bg >> 8) & 255) as i32;
        let bb = (bg & 255) as i32;
        match self.tp_mode {
            0 => { r = (br + r) / 2; g = (bgc + g) / 2; b = (bb + b) / 2; }
            1 => { r += br; g += bgc; b += bb; }
            2 => { r = br - r; g = bgc - g; b = bb - b; }
            _ => { r = br + r / 4; g = bgc + g / 4; b = bb + b / 4; }
        }
        self.hi[i] = (r.clamp(0, 255) as u32) << 16 | (g.clamp(0, 255) as u32) << 8 | b.clamp(0, 255) as u32;
    }

    #[inline]
    pub(super) fn hi_put_pixel(&mut self, x: i32, y: i32, r: i32, g: i32, b: i32, semi: bool) {
        if self.acc_on { self.acc_put_pixel(x, y, r, g, b, semi); return; }
        if !self.owns(y) { return; }
        let s = self.scale;
        for j in 0..s {
            for i in 0..s {
                let idx = (((y & 511) * s + j) * WIDTH * s + (x & 1023) * s + i) as usize;
                if semi { self.hi_blend(idx, r, g, b); }
                else { self.hi[idx] = (r as u32) << 16 | (g as u32) << 8 | b as u32; }
            }
        }
    }

    // ---- textures ----

    /// The decoded copy of the current texture page with this palette (None when it wraps past the edge of video memory).
    fn page_for(&mut self, clut: i32) -> Option<usize> {
        let key = [self.tp_x, self.tp_y, self.tp_depth, clut];
        self.page_clock += 1;
        let clock = self.page_clock;
        if let Some(i) = self.pages.iter().position(|p| p.key == key) {
            self.pages[i].age = clock;
            return Some(i);
        }
        let per = match self.tp_depth { 0 => 64, 1 => 128, _ => 256 };
        if self.tp_x + per > 1024 { return None; }
        let (pw, pr) = match self.tp_depth { 0 => (16, 0), 1 => (256, 0), _ => (0, 0) };
        let _ = pr;
        let cx = (clut & 0x3F) * 16;
        let cy = (clut >> 6) & 0x1FF;
        if self.tp_depth < 2 && cx + pw > 1024 { return None; }
        let page = TexPage {
            key,
            tiles: [false; TILES],
            px: vec![0u32; 256 * 256],
            age: clock,
            tex: [self.tp_x, self.tp_y, self.tp_x + per - 1, (self.tp_y + 255).min(511)],
            pal: if self.tp_depth < 2 { [cx, cy, cx + pw - 1, cy] } else { [-1, -1, -2, -2] },
        };
        if self.pages.len() >= 24 {
            let old = (0..self.pages.len()).min_by_key(|&i| self.pages[i].age).unwrap_or(0);
            self.pages.swap_remove(old);
        }
        self.pages.push(page);
        Some(self.pages.len() - 1)
    }

    fn decode_tile(&mut self, pi: usize, tile: usize) {
        let key = self.pages[pi].key;
        let clut = key[3];
        let clut_x = (clut & 0x3F) * 16;
        let clut_row = ((clut >> 6) & 0x1FF) * WIDTH;
        let (tx, ty) = ((tile & 7) as i32 * 32, (tile >> 3) as i32 * 32);
        let mut out = [0u32; 1024];
        for v in 0..32 {
            let row = (((key[1] + ty + v) & 511) * WIDTH) as usize;
            for u in 0..32 {
                out[(v * 32 + u) as usize] = pack_texel(self.texel(row, tx + u, clut_x, clut_row, key[2]));
            }
        }
        let page = &mut self.pages[pi];
        for v in 0..32usize {
            let dst = (ty as usize + v) * 256 + tx as usize;
            page.px[dst..dst + 32].copy_from_slice(&out[v * 32..v * 32 + 32]);
        }
        page.tiles[tile] = true;
    }

    /// Decodes the tiles of the current page that a primitive's texture rectangle can reach, and gives its pixels
    /// (null when the page cannot be cached).
    fn tex_ptr(&mut self, pi: Option<usize>, bb: [i32; 4], win: bool) -> *const u32 {
        let pi = match pi { Some(p) => p, None => return std::ptr::null() };
        if win || bb[1] - bb[0] > 224 || bb[3] - bb[2] > 224 {
            for t in 0..TILES { if !self.pages[pi].tiles[t] { self.decode_tile(pi, t); } }
        } else {
            let mut v = bb[2];
            loop {
                let mut u = bb[0];
                loop {
                    let tile = (((v & 255) >> 5) * 8 + ((u & 255) >> 5)) as usize;
                    if !self.pages[pi].tiles[tile] { self.decode_tile(pi, tile); }
                    if u >= bb[1] { break; }
                    u = (u + 16).min(bb[1]);
                }
                if v >= bb[3] { break; }
                v = (v + 16).min(bb[3]);
            }
        }
        self.pages[pi].px.as_ptr()
    }

    // one texel through the texture window: 0 transparent, else bit 31 | semi bit 30 | 0xRRGGBB
    #[inline(always)]
    fn hi_tap(&self, px: *const u32, ui: i32, vi: i32, win: bool, clut_x: i32, clut_row: i32) -> u32 {
        let (mut u, mut v) = (ui, vi);
        if win {
            u = (u & !self.win_mask_x) | (self.win_off_x & self.win_mask_x);
            v = (v & !self.win_mask_y) | (self.win_off_y & self.win_mask_y);
        }
        u &= 255;
        v &= 255;
        if !px.is_null() {
            return unsafe { *px.add((v * 256 + u) as usize) };
        }
        let ty = (((self.tp_y + v) & 511) * WIDTH) as usize;
        pack_texel(self.texel(ty, u, clut_x, clut_row, self.tp_depth))
    }

    // The colour at a position between texels (a texel's centre is at its whole number), kept inside the rectangle
    // bb = [u min, u max, v min, v max]. None when it is transparent.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn hi_sample(&self, px: *const u32, uf: f32, vf: f32, bb: [i32; 4], win: bool, clut_x: i32, clut_row: i32) -> Option<(i32, i32, i32, bool)> {
        if self.fil() == 0 {
            let (ui, vi) = (ffloor(uf + 0.5).clamp(bb[0], bb[1]), ffloor(vf + 0.5).clamp(bb[2], bb[3]));
            let t = self.hi_tap(px, ui, vi, win, clut_x, clut_row);
            if t == 0 { return None; }
            return Some((((t >> 16) & 255) as i32, ((t >> 8) & 255) as i32, (t & 255) as i32, t & 0x4000_0000 != 0));
        }
        let (iu, iv) = (ffloor(uf), ffloor(vf));
        let (fu, fv) = (iu as f32, iv as f32);
        let k = if self.fil() == 1 { (self.scale as f32 * 0.5).max(1.0) } else { 1.0 };
        let au = (((uf - fu) - 0.5) * k + 0.5).clamp(0.0, 1.0);
        let av = (((vf - fv) - 0.5) * k + 0.5).clamp(0.0, 1.0);
        let u_a = iu.clamp(bb[0], bb[1]);
        let u_b = (iu + 1).clamp(bb[0], bb[1]);
        let v_a = iv.clamp(bb[2], bb[3]);
        let v_b = (iv + 1).clamp(bb[2], bb[3]);
        self.hi_mix(px, u_a, u_b, v_a, v_b, au, av, win, clut_x, clut_row)
    }

    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn hi_mix(&self, px: *const u32, u_a: i32, u_b: i32, v_a: i32, v_b: i32, au: f32, av: f32, win: bool, clut_x: i32, clut_row: i32)
        -> Option<(i32, i32, i32, bool)> {
        if !win && !px.is_null() { return mix_fast(px, u_a, u_b, v_a, v_b, au, av, self.fil() == 1); }
        let t00 = self.hi_tap(px, u_a, v_a, win, clut_x, clut_row);
        let (mut t10, mut t01) = (t00, t00);
        if u_b != u_a { t10 = self.hi_tap(px, u_b, v_a, win, clut_x, clut_row); }
        if v_b != v_a { t01 = self.hi_tap(px, u_a, v_b, win, clut_x, clut_row); }
        let t11 = if u_b != u_a && v_b != v_a { self.hi_tap(px, u_b, v_b, win, clut_x, clut_row) }
              else if u_b != u_a { t10 } else if v_b != v_a { t01 } else { t00 };
        let t = [t00, t10, t01, t11];
        let w: [f32; 4] = if self.fil() == 2 || (t00 == t10 && t00 == t01 && t00 == t11) {
            [(1.0 - au) * (1.0 - av), au * (1.0 - av), (1.0 - au) * av, au * av]
        } else {
            // the cell is split along the diagonal whose two ends are more alike (the edge runs that way), and the colour
            // is interpolated inside the triangle the position falls in: diagonal edges stay straight instead of blurring
            if dist(t00, t11) <= dist(t10, t01) {
                if au >= av { [1.0 - au, au - av, 0.0, av] } else { [1.0 - av, 0.0, av - au, au] }
            } else if au + av <= 1.0 {
                [1.0 - au - av, au, av, 0.0]
            } else {
                [0.0, 1.0 - av, 1.0 - au, au + av - 1.0]
            }
        };
        let mut near = t00;
        let mut best = w[0];
        for i in 1..4 { if w[i] > best { best = w[i]; near = t[i]; } }
        let (mut wr, mut wg, mut wb, mut wsum) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for i in 0..4 {
            if t[i] == 0 || w[i] <= 0.0 { continue; }
            wr += w[i] * ((t[i] >> 16) & 255) as f32;
            wg += w[i] * ((t[i] >> 8) & 255) as f32;
            wb += w[i] * (t[i] & 255) as f32;
            wsum += w[i];
        }
        if wsum < 0.5 { return None; }
        let inv = 1.0 / wsum;
        Some(((wr * inv + 0.5) as i32, (wg * inv + 0.5) as i32, (wb * inv + 0.5) as i32, near != 0 && near & 0x4000_0000 != 0))
    }

    // ---- primitives ----

    // 1/z of a triangle's three vertices (scaled so the nearest is 1), or None when any is unknown or the depths are too
    // unlike to be one surface the transform really produced
    pub(super) fn vertex_depths(&self, x: [i32; 3], y: [i32; 3]) -> Option<[f64; 3]> {
        if !self.persp || self.zt.is_null() { return None; }
        let mut z = [0u32; 3];
        for i in 0..3 {
            let key = ((((y[i] - self.off_y) & 1023) as usize) << 10) | (((x[i] - self.off_x) & 1023) as usize);
            let e = unsafe { *self.zt.add(key) };
            if (self.zstamp.wrapping_sub(e >> 16)) & 0xFFFF > 2 { return None; }
            z[i] = e & 0xFFFF;
            if z[i] == 0 { return None; }
        }
        let zmin = z[0].min(z[1]).min(z[2]);
        let zmax = z[0].max(z[1]).max(z[2]);
        if zmax > zmin * 12 { return None; }
        if zmax == zmin { return None; } // flat to the screen: affine is already exact
        let m = zmin as f64;
        Some([m / z[0] as f64, m / z[1] as f64, m / z[2] as f64])
    }

    // the sixteenths of a pixel the geometry coprocessor's projection had beyond each integer vertex (0 where it did not
    // transform the vertex this frame or the last, or where two vertices met on one pixel)
    pub(super) fn vertex_fracs(&self, x: [i32; 3], y: [i32; 3]) -> ([i64; 3], [i64; 3]) {
        let mut fx = [0i64; 3];
        let mut fy = [0i64; 3];
        if !self.persp || self.zt.is_null() || self.zf.is_null() { return (fx, fy); }
        for i in 0..3 {
            let key = ((((y[i] - self.off_y) & 1023) as usize) << 10) | (((x[i] - self.off_x) & 1023) as usize);
            let e = unsafe { *self.zt.add(key) };
            if (self.zstamp.wrapping_sub(e >> 16)) & 0xFFFF > 2 { continue; }
            let f = unsafe { *self.zf.add(key) };
            if f == 0xFF { continue; }
            fx[i] = (f >> 4) as i64;
            fy[i] = (f & 15) as i64;
        }
        (fx, fy)
    }

    /// A triangle drawn into the large copy (x and y already include the drawing offset).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn hi_tri(&mut self, x: [i32; 3], y: [i32; 3], c: [u32; 3], u: [i32; 3], v: [i32; 3], tex: bool, raw: bool, semi: bool, clut: i32) {
        if self.acc_on { self.acc_tri(x, y, c, u, v, tex, raw, semi, clut); return; }
        let s = self.scale as i64;
        let sc = 2 * s;
        let area: i64 = (x[1] - x[0]) as i64 * (y[2] - y[0]) as i64 - (x[2] - x[0]) as i64 * (y[1] - y[0]) as i64;
        if area == 0 { return; }
        // positions in units of 1 / (32 * scale) of a pixel, so the projection's sixteenths are kept
        const Q: i64 = 16;
        let (fx, fy) = self.vertex_fracs(x, y);
        let vx = [x[0] as i64 * sc * Q + fx[0] * sc, x[1] as i64 * sc * Q + fx[1] * sc, x[2] as i64 * sc * Q + fx[2] * sc];
        let vy = [y[0] as i64 * sc * Q + fy[0] * sc, y[1] as i64 * sc * Q + fy[1] * sc, y[2] as i64 * sc * Q + fy[2] * sc];
        let area = (vx[1] - vx[0]) * (vy[2] - vy[0]) - (vx[2] - vx[0]) * (vy[1] - vy[0]);
        if area == 0 { return; }
        let bs: i64 = if area > 0 { 1 } else { -1 };
        // edge i is opposite vertex i; the weight of vertex i is E[i] / (area in the scaled units)
        let gx = [-(vy[2] - vy[1]), -(vy[0] - vy[2]), -(vy[1] - vy[0])];
        let gy = [vx[2] - vx[1], vx[0] - vx[2], vx[1] - vx[0]];
        let mut slack = [0i64; 3]; // a tie on an edge is inside only on its top / left side
        for i in 0..3 {
            let (a, b) = (bs * gx[i], bs * gy[i]);
            slack[i] = if a > 0 || (a == 0 && b > 0) { 0 } else { 1 };
        }
        let inv_area = 1.0 / (area as f64);

        let min_x = x[0].min(x[1]).min(x[2]);
        let max_x = x[0].max(x[1]).max(x[2]);
        let min_y = y[0].min(y[1]).min(y[2]);
        let max_y = y[0].max(y[1]).max(y[2]);
        let xlo = (min_x as i64 * s).max(self.area_x1 as i64 * s);
        let xhi = (max_x as i64 * s + 2 * s - 1).min(self.area_x2 as i64 * s + s - 1);
        let ylo = (min_y as i64 * s).max(self.area_y1 as i64 * s);
        let yhi = (max_y as i64 * s + 2 * s - 1).min(self.area_y2 as i64 * s + s - 1);
        if xlo > xhi || ylo > yhi { return; }

        let gouraud = c[0] != c[1] || c[1] != c[2];
        let col = |k: usize| ((c[k] & 0xFF) as f64, ((c[k] >> 8) & 0xFF) as f64, ((c[k] >> 16) & 0xFF) as f64);
        let (c0, c1, c2) = (col(0), col(1), col(2));
        let clut_x = (clut & 0x3F) * 16;
        let clut_row = ((clut >> 6) & 0x1FF) * WIDTH;
        let win = self.win_mask_x != 0 || self.win_mask_y != 0;
        let bb = [u[0].min(u[1]).min(u[2]), u[0].max(u[1]).max(u[2]), v[0].min(v[1]).min(v[2]), v[0].max(v[1]).max(v[2])];
        let px = if tex { let pi = self.page_for(clut); self.tex_ptr(pi, bb, win) } else { std::ptr::null() };
        let hw = WIDTH as i64 * s;
        let (r0, g0, b0) = (c0.0 as i32, c0.1 as i32, c0.2 as i32);
        // inverse depths of the three vertices when the geometry coprocessor transformed them this frame or the last
        let inv_z = if tex { self.vertex_depths(x, y) } else { None };
        // per hi pixel step along a row: Px grows by 2, so a weight grows by 2 * g * inv_area
        let step = [2.0 * Q as f64 * gx[0] as f64 * inv_area, 2.0 * Q as f64 * gx[1] as f64 * inv_area, 2.0 * Q as f64 * gx[2] as f64 * inv_area];
        let d_r = (step[0] * c0.0 + step[1] * c1.0 + step[2] * c2.0) as f32;
        let d_g = (step[0] * c0.1 + step[1] * c1.1 + step[2] * c2.1) as f32;
        let d_b = (step[0] * c0.2 + step[1] * c1.2 + step[2] * c2.2) as f32;
        let d_u = (step[0] * u[0] as f64 + step[1] * u[1] as f64 + step[2] * u[2] as f64) as f32;
        let d_v = (step[0] * v[0] as f64 + step[1] * v[1] as f64 + step[2] * v[2] as f64) as f32;
        let pz = inv_z.unwrap_or([1.0; 3]);
        let persp = inv_z.is_some();
        // the common case (cached decoded page, no texture window, balanced filter) shares the four texels and the choice of
        // diagonal between neighbouring large pixels that fall in the same cell
        let fast = tex && !px.is_null() && !win && self.fil() == 1;
        let kf = (self.scale as f32 * 0.5).max(1.0);
        let d_pn = (step[0] * pz[0] + step[1] * pz[1] + step[2] * pz[2]) as f32;
        let d_pu = (step[0] * u[0] as f64 * pz[0] + step[1] * u[1] as f64 * pz[1] + step[2] * u[2] as f64 * pz[2]) as f32;
        let d_pv = (step[0] * v[0] as f64 * pz[0] + step[1] * v[1] as f64 * pz[1] + step[2] * v[2] as f64 * pz[2]) as f32;

        for yy in ylo..=yhi {
            if !self.owns((yy / s) as i32) { continue; }
            let (mut cu, mut cv) = (i32::MIN, i32::MIN);
            let mut cq = [0u32; 4];
            let (mut cflat, mut cmain) = (false, false);
            let py = (2 * yy + 1 - s) * Q;
            let px0 = (2 * xlo + 1 - s) * Q;
            let e = [
                gy[0] * (py - vy[1]) + gx[0] * (px0 - vx[1]),
                gy[1] * (py - vy[2]) + gx[1] * (px0 - vx[2]),
                gy[2] * (py - vy[0]) + gx[2] * (px0 - vx[0]),
            ];
            // the run of pixels on this row where every edge is satisfied: bs * (e + 2 * gx * k) - slack >= 0
            let mut k_lo: i64 = 0;
            let mut k_hi: i64 = xhi - xlo;
            let mut empty = false;
            for i in 0..3 {
                let base = bs * e[i] - slack[i];
                let st = 2 * Q * bs * gx[i];
                if st == 0 {
                    if base < 0 { empty = true; break; }
                } else if st > 0 {
                    if base < 0 { k_lo = k_lo.max((-base + st - 1) / st); }
                } else {
                    if base < 0 { empty = true; break; }
                    k_hi = k_hi.min(base / (-st));
                }
            }
            if empty || k_lo > k_hi { continue; }
            let a0 = (e[0] + 2 * Q * gx[0] * k_lo) as f64 * inv_area;
            let a1 = (e[1] + 2 * Q * gx[1] * k_lo) as f64 * inv_area;
            let a2 = (e[2] + 2 * Q * gx[2] * k_lo) as f64 * inv_area;
            let mut cr = (a0 * c0.0 + a1 * c1.0 + a2 * c2.0) as f32;
            let mut cg = (a0 * c0.1 + a1 * c1.1 + a2 * c2.1) as f32;
            let mut cb = (a0 * c0.2 + a1 * c1.2 + a2 * c2.2) as f32;
            let mut uf = (a0 * u[0] as f64 + a1 * u[1] as f64 + a2 * u[2] as f64) as f32;
            let mut vf = (a0 * v[0] as f64 + a1 * v[1] as f64 + a2 * v[2] as f64) as f32;
            let mut pn = (a0 * pz[0] + a1 * pz[1] + a2 * pz[2]) as f32;
            let mut pu = (a0 * u[0] as f64 * pz[0] + a1 * u[1] as f64 * pz[1] + a2 * u[2] as f64 * pz[2]) as f32;
            let mut pv = (a0 * v[0] as f64 * pz[0] + a1 * v[1] as f64 * pz[1] + a2 * v[2] as f64 * pz[2]) as f32;
            let row = yy * hw;
            for k in k_lo..=k_hi {
                let idx = (row + xlo + k) as usize;
                // the values are inside 0..~300, so the unchecked float to integer conversion is safe and cheaper
                let (mut r, mut g, mut bl) = if gouraud {
                    unsafe { ((cr + 0.5).to_int_unchecked::<i32>(), (cg + 0.5).to_int_unchecked::<i32>(), (cb + 0.5).to_int_unchecked::<i32>()) }
                } else { (r0, g0, b0) };
                let mut tex_semi = false;
                let mut draw = true;
                if tex {
                    let (su, sv) = if persp { let inv = 1.0 / pn; (pu * inv, pv * inv) } else { (uf, vf) };
                    let smp = if fast {
                        let (iu, iv) = (ffloor(su), ffloor(sv));
                        if iu != cu || iv != cv {
                            cu = iu; cv = iv;
                            let (ua, ub) = (iu.clamp(bb[0], bb[1]), (iu + 1).clamp(bb[0], bb[1]));
                            let (va, vb) = (iv.clamp(bb[2], bb[3]), (iv + 1).clamp(bb[2], bb[3]));
                            let (ra, rb) = (((va & 255) << 8) as usize, ((vb & 255) << 8) as usize);
                            let (xa, xb) = ((ua & 255) as usize, (ub & 255) as usize);
                            unsafe { cq = [*px.add(ra + xa), *px.add(ra + xb), *px.add(rb + xa), *px.add(rb + xb)]; }
                            cflat = cq[0] == cq[1] && cq[0] == cq[2] && cq[0] == cq[3];
                            cmain = !cflat && diag_main(px, cq[0], cq[1], cq[2], cq[3], ua, ub, va, vb);
                        }
                        if cflat { unpack_texel(cq[0]) } else {
                            let au = (((su - cu as f32) - 0.5) * kf + 0.5).clamp(0.0, 1.0);
                            let av = (((sv - cv as f32) - 0.5) * kf + 0.5).clamp(0.0, 1.0);
                            let (a8, v8) = unsafe { ((au * 256.0).to_int_unchecked::<i32>(), (av * 256.0).to_int_unchecked::<i32>()) };
                            blend_quad(cq, a8, v8, true, cmain)
                        }
                    } else { self.hi_sample(px, su, sv, bb, win, clut_x, clut_row) };
                    match smp {
                        None => draw = false,
                        Some((tr, tg, tb, ts)) => {
                            tex_semi = ts;
                            if raw { r = tr; g = tg; bl = tb; }
                            else {
                                r = 255.min(tr * r.clamp(0, 255) >> 7);
                                g = 255.min(tg * g.clamp(0, 255) >> 7);
                                bl = 255.min(tb * bl.clamp(0, 255) >> 7);
                            }
                        }
                    }
                }
                if draw {
                    let (r, g, bl) = (r.clamp(0, 255), g.clamp(0, 255), bl.clamp(0, 255));
                    if semi && (!tex || tex_semi) { self.hi_blend(idx, r, g, bl); }
                    else { self.hi[idx] = (r as u32) << 16 | (g as u32) << 8 | bl as u32; }
                }
                cr += d_r; cg += d_g; cb += d_b;
                uf += d_u; vf += d_v;
                pn += d_pn; pu += d_pu; pv += d_pv;
            }
        }
    }

    /// A rectangle (a sprite) into the large copy; x and y include the drawing offset, and j0..j1 / i0..i1 are the rows and
    /// columns the clipped native drawing covers.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn hi_rect(&mut self, x: i32, y: i32, w: i32, h: i32, j0: i32, j1: i32, i0: i32, i1: i32, colour: u32, tex: bool,
                          raw: bool, semi: bool, u0: i32, v0: i32, clut: i32) {
        if self.acc_on { self.acc_rect(x, y, w, h, j0, j1, i0, i1, colour, tex, raw, semi, u0, v0, clut); return; }
        let s = self.scale;
        let hw = (WIDTH * s) as usize;
        let cr = (colour & 0xFF) as i32;
        let cg = ((colour >> 8) & 0xFF) as i32;
        let cb = ((colour >> 16) & 0xFF) as i32;
        if !tex {
            let c = (cr as u32) << 16 | (cg as u32) << 8 | cb as u32;
            for j in j0..=j1 {
                if !self.owns(y + j) { continue; }
                for sy in 0..s {
                    let row = (((y + j) * s + sy) as usize) * hw;
                    for i in (i0 * s)..=(i1 * s + s - 1) {
                        let idx = row + (x * s + i) as usize;
                        if semi { self.hi_blend(idx, cr, cg, cb); } else { self.hi[idx] = c; }
                    }
                }
            }
            return;
        }
        let clut_x = (clut & 0x3F) * 16;
        let clut_row = ((clut >> 6) & 0x1FF) * WIDTH;
        let win = self.win_mask_x != 0 || self.win_mask_y != 0;
        let bb = [u0, u0 + w - 1, v0, v0 + h - 1];
        let px = { let pi = self.page_for(clut); self.tex_ptr(pi, bb, win) };
        let inv = 1.0 / s as f32;
        let k = if self.fil() == 1 { (s as f32 * 0.5).max(1.0) } else if self.fil() == 2 { 1.0 } else { 0.0 };
        // the columns: texel pair and weight for every large-copy column of the sprite
        let ncols = ((i1 - i0 + 1) * s) as usize;
        let mut cols: Vec<(i32, i32, f32)> = Vec::with_capacity(ncols);
        for i in i0..=i1 {
            for sx in 0..s {
                let uf = u0 as f32 + i as f32 + (sx as f32 + 0.5) * inv - 0.5;
                cols.push(Self::taps(uf, k, bb[0], bb[1]));
            }
        }
        for j in j0..=j1 {
            if !self.owns(y + j) { continue; }
            for sy in 0..s {
                let vf = v0 as f32 + j as f32 + (sy as f32 + 0.5) * inv - 0.5;
                let (v_a, v_b, av) = Self::taps(vf, k, bb[2], bb[3]);
                let row = (((y + j) * s + sy) as usize) * hw + (x + i0) as usize * s as usize;
                for (n, &(u_a, u_b, au)) in cols.iter().enumerate() {
                    if let Some((tr, tg, tb, ts)) = self.hi_mix(px, u_a, u_b, v_a, v_b, au, av, win, clut_x, clut_row) {
                        let (r, g, b) = if raw { (tr, tg, tb) } else {
                            (255.min(tr * cr >> 7), 255.min(tg * cg >> 7), 255.min(tb * cb >> 7))
                        };
                        let idx = row + n;
                        if semi && ts { self.hi_blend(idx, r, g, b); }
                        else { self.hi[idx] = (r as u32) << 16 | (g as u32) << 8 | b as u32; }
                    }
                }
            }
        }
    }

    // the two texels either side of a position and the weight of the second (k = 0: the nearest texel only)
    #[inline]
    fn taps(f: f32, k: f32, lo: i32, hi: i32) -> (i32, i32, f32) {
        if k == 0.0 {
            let i = ffloor(f + 0.5).clamp(lo, hi);
            return (i, i, 0.0);
        }
        let i = ffloor(f);
        let a = (((f - i as f32) - 0.5) * k + 0.5).clamp(0.0, 1.0);
        (i.clamp(lo, hi), (i + 1).clamp(lo, hi), a)
    }

    /// The display as 0x00RRGGBB pixels, (disp_w * scale) x (disp_h * scale), top row first.
    pub fn read_display_hi(&mut self, out: &mut Vec<u32>) {
        let s = self.scale;
        if !self.hi_on { self.read_display(out); return; }
        if self.acc_on && !self.display24 { self.acc_read_display(out); return; }
        let (w, h) = (self.disp_w, self.disp_h);
        let (ow, oh) = ((w * s) as usize, (h * s) as usize);
        if self.display24 {
            // movie frames are read as 24-bit pictures; scale them up with a cubic filter
            let mut small = Vec::new();
            self.read_display(&mut small);
            crate::post::deblock(&mut small, w as usize, h as usize);
            crate::post::upscale_cubic(&small, w as usize, h as usize, s as usize, out);
            return;
        }
        out.clear();
        out.resize(ow * oh, 0);
        let scale = if h > 240 { 2 } else { 1 };
        let first = (self.line_top - 16) * scale;
        let last = (self.line_bottom - 16) * scale;
        let hw = (WIDTH * s) as usize;
        for y in 0..h {
            if y < first || y >= last { continue; }
            let src_y = ((self.disp_y + y - first) & 511) as usize;
            for sy in 0..s as usize {
                let src = (src_y * s as usize + sy) * hw;
                let dst = (y as usize * s as usize + sy) * ow;
                for x in 0..w {
                    let sx = ((self.disp_x + x) & 1023) as usize * s as usize;
                    let o = x as usize * s as usize;
                    out[dst + o..dst + o + s as usize].copy_from_slice(&self.hi[src + sx..src + sx + s as usize]);
                }
            }
        }
        if self.aa {
            let mut scratch = Vec::new();
            crate::post::antialias(out, ow, oh, &mut scratch);
        }
    }
}
