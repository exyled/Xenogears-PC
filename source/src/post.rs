// Passes over the finished picture: edge anti-aliasing (the stair-steps left on polygon and sprite edges at the larger
// size are blended along the edge), and a sharper scaler for movie frames.

#[inline]
fn luma(p: u32) -> i32 {
    let (r, g, b) = (((p >> 16) & 255) as i32, ((p >> 8) & 255) as i32, (p & 255) as i32);
    (r + 2 * g + b) >> 2
}

#[inline]
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let mut o = 0u32;
    for sh in [16u32, 8, 0] {
        let (x, y) = (((a >> sh) & 255) as f32, ((b >> sh) & 255) as f32);
        o |= (((x + (y - x) * t) + 0.5) as u32).min(255) << sh;
    }
    o
}

/// Anti-aliases `px` (w x h) in place, using `scratch` for the untouched copy.
pub fn antialias(px: &mut Vec<u32>, w: usize, h: usize, scratch: &mut Vec<u32>) {
    if w < 3 || h < 3 || px.len() != w * h { return; }
    scratch.clear();
    scratch.extend_from_slice(px);
    unsafe { antialias_rows(scratch.as_ptr(), px.as_mut_ptr(), w, h, 0, h); }
}

/// Anti-aliases rows y0..y1 of the picture `src` (w x h) into the same rows of `dst`. Several threads can each take their
/// own rows of one picture: the source is only read and the rows written are not shared.
///
/// # Safety
/// `src` and `dst` must each point to w * h pixels, and no other thread may touch rows y0..y1 of `dst`.
pub unsafe fn antialias_rows(src: *const u32, dst: *mut u32, w: usize, h: usize, y0: usize, y1: usize) {
    if y0 >= y1 { return; }
    let s = std::slice::from_raw_parts(src, w * h);
    let d = std::slice::from_raw_parts_mut(dst.add(y0 * w), (y1 - y0) * w);
    d.copy_from_slice(&s[y0 * w..y1 * w]);
    if w < 3 || h < 3 { return; }
    // brightness once per pixel for the rows these edges can see, then the cheap test (is there an edge here) on it only
    let (l0, l1) = (y0.saturating_sub(1), (y1 + 1).min(h));
    let mut lum: Vec<i16> = vec![0; (l1 - l0) * w];
    for (dv, &p) in lum.iter_mut().zip(s[l0 * w..l1 * w].iter()) { *dv = luma(p) as i16; }
    let at = |y: usize, x: usize| lum[(y - l0) * w + x];
    let mut mark = vec![0u8; (y1 - y0) * w];
    for y in y0.max(1)..y1.min(h - 1) {
        for x in 1..w - 1 {
            let m = at(y, x);
            let (n, so, we, ea) = (at(y - 1, x), at(y + 1, x), at(y, x - 1), at(y, x + 1));
            let mx = m.max(n).max(so).max(we).max(ea);
            let mn = m.min(n).min(so).min(we).min(ea);
            mark[(y - y0) * w + x] = ((mx - mn) >= 10.max(mx / 6)) as u8;
        }
    }
    for y in y0.max(1)..y1.min(h - 1) {
        for x in 1..w - 1 {
            if mark[(y - y0) * w + x] == 0 { continue; }
            let i = y * w + x;
            let (m, n, so, we, ea) = (at(y, x) as i32, at(y - 1, x) as i32, at(y + 1, x) as i32, at(y, x - 1) as i32, at(y, x + 1) as i32);
            let mx = m.max(n).max(so).max(we).max(ea);
            let mn = m.min(n).min(so).min(we).min(ea);
            let range = mx - mn;
            let (nw, ne, sw, se) = (at(y - 1, x - 1) as i32, at(y - 1, x + 1) as i32, at(y + 1, x - 1) as i32, at(y + 1, x + 1) as i32);
            let l = ((n + so + we + ea) * 2 + nw + ne + sw + se) as f32 / 12.0;
            let blend_l = (((l - m as f32).abs() / range as f32 - 0.25).max(0.0) * 1.3333).min(0.75);
            let edge_h = (nw + ne - 2 * n).abs() + 2 * (we + ea - 2 * m).abs() + (sw + se - 2 * so).abs();
            let edge_v = (nw + sw - 2 * we).abs() + 2 * (n + so - 2 * m).abs() + (ne + se - 2 * ea).abs();
            let (l1, l2, p1, p2) = if edge_h >= edge_v { (n, so, s[i - w], s[i + w]) } else { (we, ea, s[i - 1], s[i + 1]) };
            let across = if (l1 - m).abs() >= (l2 - m).abs() { p1 } else { p2 };
            let mut out = mix(s[i], across, 0.5);
            if blend_l > 0.0 {
                let mut sum = [0u32; 3];
                for dy in [-(w as isize), 0, w as isize] {
                    for dx in [-1isize, 0, 1] {
                        let p = s[(i as isize + dy + dx) as usize];
                        sum[0] += (p >> 16) & 255;
                        sum[1] += (p >> 8) & 255;
                        sum[2] += p & 255;
                    }
                }
                let avg = ((sum[0] / 9) << 16) | ((sum[1] / 9) << 8) | (sum[2] / 9);
                out = mix(out, avg, blend_l * 0.5);
            }
            d[(y - y0) * w + x] = out;
        }
    }
}

#[inline]
fn cubic(t: f32) -> [f32; 4] {
    // Catmull-Rom weights for the four texels around a position at fraction t
    let t2 = t * t;
    let t3 = t2 * t;
    [
        -0.5 * t3 + t2 - 0.5 * t,
        1.5 * t3 - 2.5 * t2 + 1.0,
        -1.5 * t3 + 2.0 * t2 + 0.5 * t,
        0.5 * t3 - 0.5 * t2,
    ]
}

/// Scales a w x h picture up by an integer factor with a cubic filter (sharper than bilinear, no ringing worth the name
/// at these sizes), writing (w * s) x (h * s) pixels.
pub fn upscale_cubic(src: &[u32], w: usize, h: usize, s: usize, out: &mut Vec<u32>) {
    let (ow, oh) = (w * s, h * s);
    out.clear();
    out.resize(ow * oh, 0);
    // horizontal pass into floats, three channels
    let mut tmp = vec![0f32; ow * h * 3];
    // output sample k of source pixel x lies at x + (k + 0.5) / s - 0.5; the fraction above floor
    let frac: Vec<(isize, [f32; 4])> = (0..s).map(|k| {
        let p = (k as f32 + 0.5) / s as f32 - 0.5;
        let fl = p.floor();
        (fl as isize, cubic(p - fl))
    }).collect();
    for y in 0..h {
        for x in 0..w {
            for (k, &(base, ref c)) in frac.iter().enumerate() {
                let o = (y * ow + x * s + k) * 3;
                for ch in 0..3 {
                    let sh = 16 - 8 * ch as u32;
                    let mut a = 0.0f32;
                    for t in 0..4isize {
                        let xx = (x as isize + base - 1 + t).clamp(0, w as isize - 1) as usize;
                        a += c[t as usize] * ((src[y * w + xx] >> sh) & 255) as f32;
                    }
                    tmp[o + ch] = a;
                }
            }
        }
    }
    for y in 0..h {
        for (k, &(base, ref c)) in frac.iter().enumerate() {
            let oy = y * s + k;
            let rows: [usize; 4] = [
                (y as isize + base - 1).clamp(0, h as isize - 1) as usize,
                (y as isize + base).clamp(0, h as isize - 1) as usize,
                (y as isize + base + 1).clamp(0, h as isize - 1) as usize,
                (y as isize + base + 2).clamp(0, h as isize - 1) as usize,
            ];
            for x in 0..ow {
                let mut o = 0u32;
                for ch in 0..3 {
                    let mut a = 0.0f32;
                    for t in 0..4 { a += c[t] * tmp[(rows[t] * ow + x) * 3 + ch]; }
                    let v = (a + 0.5).clamp(0.0, 255.0) as u32;
                    o |= v << (16 - 8 * ch as u32);
                }
                out[oy * ow + x] = o;
            }
        }
    }
}

/// Softens the seams between the 8 x 8 blocks of a decoded movie frame: where the step across a block edge is small (a
/// compression seam, not a picture edge) the pixels on both sides are drawn toward each other, and real edges are left alone.
pub fn deblock(px: &mut [u32], w: usize, h: usize) {
    if px.len() != w * h || w < 16 || h < 16 { return; }
    #[inline]
    fn smooth(a: &mut u32, b: &mut u32, c: &mut u32, d: &mut u32) {
        // a b | c d across a seam
        let mut o = [0u32; 4];
        for sh in [16u32, 8, 0] {
            let (pa, pb, pc, pd) = (((*a >> sh) & 255) as i32, ((*b >> sh) & 255) as i32, ((*c >> sh) & 255) as i32, ((*d >> sh) & 255) as i32);
            let step = pc - pb;
            // a seam is a small step with smooth ground on each side; anything bigger is part of the picture
            let (sa, sd) = ((pb - pa).abs(), (pd - pc).abs());
            let (nb, nc, na, nd) = if step.abs() < 20 && sa < 12 && sd < 12 {
                (pb + step / 3, pc - step / 3, pa + step / 6, pd - step / 6)
            } else {
                (pb, pc, pa, pd)
            };
            o[0] |= (na.clamp(0, 255) as u32) << sh;
            o[1] |= (nb.clamp(0, 255) as u32) << sh;
            o[2] |= (nc.clamp(0, 255) as u32) << sh;
            o[3] |= (nd.clamp(0, 255) as u32) << sh;
        }
        *a = o[0]; *b = o[1]; *c = o[2]; *d = o[3];
    }
    for y in 0..h {
        let row = &mut px[y * w..(y + 1) * w];
        let mut x = 8;
        while x + 2 <= w {
            let (l, r) = row.split_at_mut(x);
            let (l1, l2) = l.split_at_mut(x - 1);
            let (r1, r2) = r.split_at_mut(1);
            smooth(&mut l1[x - 2], &mut l2[0], &mut r1[0], &mut r2[0]);
            x += 8;
        }
    }
    for y in (8..h).step_by(8) {
        if y + 2 > h { break; }
        for x in 0..w {
            let (mut a, mut b, mut c, mut d) = (px[(y - 2) * w + x], px[(y - 1) * w + x], px[y * w + x], px[(y + 1) * w + x]);
            smooth(&mut a, &mut b, &mut c, &mut d);
            px[(y - 2) * w + x] = a; px[(y - 1) * w + x] = b; px[y * w + x] = c; px[(y + 1) * w + x] = d;
        }
    }
}
