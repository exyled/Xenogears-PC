// What happens to the picture between the renderer and the window, all on the graphics card (wgpu, no window of its own):
//
//   frame interpolation   blend of two pictures, or a motion-compensated one (block matching at the console's own resolution)
//   upscaling             FSR 1.0 (EASU, RCAS; AMD FidelityFX, MIT licence) or a plain filter
//   composition           where the picture sits in the window (fit, whole-number, stretch, 16:9 with a blurred fill),
//                         colour correction, scanlines and a shadow mask
//
// The picture goes up as a small texture and the window's own image comes back, which the window shows one to one.
// Anything that goes wrong creating the card's resources leaves the caller with the plain picture.

use bytemuck::{Pod, Zeroable};

const SHADER: &str = r#"
struct U { a: vec4<f32>, b: vec4<f32>, c: vec4<f32>, d: vec4<f32>, e: vec4<f32>, f: vec4<f32> };
// a: source size, content size      b: canvas size, content rect origin    c: content rect size, sharpness, scanlines
// d: mask, widescreen fill, brightness, contrast      e: saturation, gamma, interpolation time, console lines
// f: motion step, 1 = linear filter, motion field size
@group(0) @binding(0) var t0: texture_2d<f32>;
@group(0) @binding(1) var t1: texture_2d<f32>;
@group(0) @binding(2) var t2: texture_2d<f32>;
@group(0) @binding(3) var<uniform> u: U;
@group(0) @binding(4) var smp_l: sampler;
@group(0) @binding(5) var smp_n: sampler;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(p[i], 0.0, 1.0);
}

fn ld(t: texture_2d<f32>, x: i32, y: i32) -> vec3<f32> {
    let d = vec2<i32>(textureDimensions(t));
    return textureLoad(t, vec2<i32>(clamp(x, 0, d.x - 1), clamp(y, 0, d.y - 1)), 0).rgb;
}
fn luma(c: vec3<f32>) -> f32 { return c.b * 0.5 + (c.r * 0.5 + c.g); }
fn rcp(x: f32) -> f32 { return 1.0 / max(x, 0.000001); }
fn l709(c: vec3<f32>) -> f32 { return dot(c, vec3<f32>(0.299, 0.587, 0.114)); }

// ---- frame interpolation ----
@fragment
fn fs_blend(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy);
    return vec4<f32>(mix(ld(t0, p.x, p.y), ld(t1, p.x, p.y), u.e.z), 1.0);
}

// block matching at the console's resolution: for each cell the displacement d (half of the motion between the two pictures,
// in console pixels) that makes the previous picture at -d agree with the current one at +d
@fragment
fn fs_motion(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let step = u.f.x;
    let c = vec2<i32>(floor(pos.xy)) * i32(step) + vec2<i32>(i32(step) / 2);
    var best = 1.0e9;
    var bd = vec2<i32>(0);
    var zero = 0.0;
    for (var dy = -4; dy <= 4; dy = dy + 1) {
        for (var dx = -4; dx <= 4; dx = dx + 1) {
            var s = 0.0;
            for (var j = -1; j <= 1; j = j + 1) {
                for (var i = -1; i <= 1; i = i + 1) {
                    let o = vec2<i32>(i, j) * i32(step);
                    let d = vec2<i32>(dx, dy) * i32(step);
                    let a = l709(ld(t0, c.x + o.x - d.x, c.y + o.y - d.y));
                    let b = l709(ld(t1, c.x + o.x + d.x, c.y + o.y + d.y));
                    s = s + abs(a - b);
                }
            }
            if (dx == 0 && dy == 0) { zero = s; }
            let cost = s + 0.004 * f32(dx * dx + dy * dy);
            if (cost < best) { best = cost; bd = vec2<i32>(dx, dy) * i32(step); }
        }
    }
    let conf = clamp((zero - best) / max(zero, 0.05) * 2.0, 0.0, 1.0);
    return vec4<f32>(vec2<f32>(bd), conf, 1.0);
}

@fragment
fn fs_warp(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let src = u.a.xy;
    let uv = pos.xy / src;
    let m = textureSampleLevel(t2, smp_l, uv, 0.0);
    let v = m.xy;
    let a = textureSampleLevel(t0, smp_l, (pos.xy - v) / src, 0.0).rgb;
    let b = textureSampleLevel(t1, smp_l, (pos.xy + v) / src, 0.0).rgb;
    let still = mix(textureSampleLevel(t0, smp_l, uv, 0.0).rgb, textureSampleLevel(t1, smp_l, uv, 0.0).rgb, 0.5);
    return vec4<f32>(mix(still, mix(a, b, 0.5), m.z), 1.0);
}

// ---- the pixel-art scaler: EPX (Scale2x) over the console's own pixels, two output pixels for each, with a tolerance so that
// the console's dither does not break the edges it looks for ----
fn console_px(cx: i32, cy: i32) -> vec3<f32> {
    let st = i32(u.f.x);
    return ld(t0, cx * st + st / 2, cy * st + st / 2);
}
fn alike(a: vec3<f32>, b: vec3<f32>) -> bool {
    let d = abs(a - b);
    return max(d.x, max(d.y, d.z)) < 0.045;
}

@fragment
fn fs_epx(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy);
    let cx = p.x >> 1;
    let cy = p.y >> 1;
    let e = console_px(cx, cy);
    let b = console_px(cx, cy - 1);
    let d = console_px(cx - 1, cy);
    let f = console_px(cx + 1, cy);
    let h = console_px(cx, cy + 1);
    let right = (p.x & 1) == 1;
    let low = (p.y & 1) == 1;
    var o = e;
    if (!right && !low) { if (alike(b, d) && !alike(b, f) && !alike(d, h)) { o = d; } }
    else if (right && !low) { if (alike(b, f) && !alike(b, d) && !alike(f, h)) { o = f; } }
    else if (!right && low) { if (alike(d, h) && !alike(d, b) && !alike(h, f)) { o = d; } }
    else { if (alike(h, f) && !alike(d, h) && !alike(b, f)) { o = f; } }
    return vec4<f32>(o, 1.0);
}

// ---- upscaling ----
@fragment
fn fs_plain(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = pos.xy / u.a.zw;
    if (u.f.y > 0.5) {
        // "sharp" bilinear: each source texel keeps its colour over most of its width and blends only across about one
        // output pixel at its edge, so enlarging by a fraction does not blur the picture
        let sz = vec2<f32>(textureDimensions(t0));
        let r = max(u.a.zw / sz, vec2<f32>(1.0));
        let t = uv * sz;
        let ti = floor(t);
        let f = clamp((t - ti - vec2<f32>(0.5)) * r + vec2<f32>(0.5), vec2<f32>(0.0), vec2<f32>(1.0));
        return vec4<f32>(textureSampleLevel(t0, smp_l, (ti + f) / sz, 0.0).rgb, 1.0);
    }
    return vec4<f32>(textureSampleLevel(t0, smp_n, uv, 0.0).rgb, 1.0);
}

fn easu_set(w: f32, lA: f32, lB: f32, lC: f32, lD: f32, lE: f32) -> vec3<f32> {
    let dc = lD - lC;
    let cb = lC - lB;
    var lenX = rcp(max(abs(dc), abs(cb)));
    let dirX = lD - lB;
    lenX = clamp(abs(dirX) * lenX, 0.0, 1.0);
    lenX = lenX * lenX;
    let ec = lE - lC;
    let ca = lC - lA;
    var lenY = rcp(max(abs(ec), abs(ca)));
    let dirY = lE - lA;
    lenY = clamp(abs(dirY) * lenY, 0.0, 1.0);
    lenY = lenY * lenY;
    return vec3<f32>(dirX * w, dirY * w, (lenX + lenY) * w);
}

fn easu_w(off: vec2<f32>, dir: vec2<f32>, len2: vec2<f32>, lob: f32, clp: f32) -> f32 {
    var v = vec2<f32>(off.x * dir.x + off.y * dir.y, off.x * (-dir.y) + off.y * dir.x);
    v = v * len2;
    var d2 = v.x * v.x + v.y * v.y;
    d2 = min(d2, clp);
    var wB = 2.0 / 5.0 * d2 - 1.0;
    var wA = lob * d2 - 1.0;
    wB = wB * wB;
    wA = wA * wA;
    wB = 25.0 / 16.0 * wB - (25.0 / 16.0 - 1.0);
    return wB * wA;
}

@fragment
fn fs_easu(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let ip = floor(pos.xy);
    let scale = vec2<f32>(textureDimensions(t0)) / u.a.zw;
    var pp = ip * scale + (0.5 * scale - vec2<f32>(0.5, 0.5));
    let fp = floor(pp);
    pp = pp - fp;
    let ix = i32(fp.x);
    let iy = i32(fp.y);
    let b = ld(t0, ix, iy - 1);     let c = ld(t0, ix + 1, iy - 1);
    let e = ld(t0, ix - 1, iy);     let f = ld(t0, ix, iy);     let g = ld(t0, ix + 1, iy);     let h = ld(t0, ix + 2, iy);
    let i = ld(t0, ix - 1, iy + 1); let j = ld(t0, ix, iy + 1); let k = ld(t0, ix + 1, iy + 1); let l = ld(t0, ix + 2, iy + 1);
    let n = ld(t0, ix, iy + 2);     let o = ld(t0, ix + 1, iy + 2);
    let bL = luma(b); let cL = luma(c); let eL = luma(e); let fL = luma(f); let gL = luma(g); let hL = luma(h);
    let iL = luma(i); let jL = luma(j); let kL = luma(k); let lL = luma(l); let nL = luma(n); let oL = luma(o);
    var dl = easu_set((1.0 - pp.x) * (1.0 - pp.y), bL, eL, fL, gL, jL);
    dl = dl + easu_set(pp.x * (1.0 - pp.y), cL, fL, gL, hL, kL);
    dl = dl + easu_set((1.0 - pp.x) * pp.y, fL, iL, jL, kL, nL);
    dl = dl + easu_set(pp.x * pp.y, gL, jL, kL, lL, oL);
    var dir = dl.xy;
    var len = dl.z;
    let dirR0 = dir.x * dir.x + dir.y * dir.y;
    let zro = dirR0 < (1.0 / 32768.0);
    var dirR = inverseSqrt(max(dirR0, 0.000000001));
    if (zro) { dirR = 1.0; dir.x = 1.0; }
    dir = dir * dirR;
    len = len * 0.5;
    len = len * len;
    let stretch = (dir.x * dir.x + dir.y * dir.y) * rcp(max(abs(dir.x), abs(dir.y)));
    let len2 = vec2<f32>(1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len);
    let lob = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * len;
    let clp = 1.0 / lob;
    var aC = vec3<f32>(0.0);
    var aW = 0.0;
    var w = easu_w(vec2<f32>(0.0, -1.0) - pp, dir, len2, lob, clp); aC = aC + b * w; aW = aW + w;
    w = easu_w(vec2<f32>(1.0, -1.0) - pp, dir, len2, lob, clp); aC = aC + c * w; aW = aW + w;
    w = easu_w(vec2<f32>(-1.0, 0.0) - pp, dir, len2, lob, clp); aC = aC + e * w; aW = aW + w;
    w = easu_w(vec2<f32>(0.0, 0.0) - pp, dir, len2, lob, clp); aC = aC + f * w; aW = aW + w;
    w = easu_w(vec2<f32>(1.0, 0.0) - pp, dir, len2, lob, clp); aC = aC + g * w; aW = aW + w;
    w = easu_w(vec2<f32>(2.0, 0.0) - pp, dir, len2, lob, clp); aC = aC + h * w; aW = aW + w;
    w = easu_w(vec2<f32>(-1.0, 1.0) - pp, dir, len2, lob, clp); aC = aC + i * w; aW = aW + w;
    w = easu_w(vec2<f32>(0.0, 1.0) - pp, dir, len2, lob, clp); aC = aC + j * w; aW = aW + w;
    w = easu_w(vec2<f32>(1.0, 1.0) - pp, dir, len2, lob, clp); aC = aC + k * w; aW = aW + w;
    w = easu_w(vec2<f32>(2.0, 1.0) - pp, dir, len2, lob, clp); aC = aC + l * w; aW = aW + w;
    w = easu_w(vec2<f32>(0.0, 2.0) - pp, dir, len2, lob, clp); aC = aC + n * w; aW = aW + w;
    w = easu_w(vec2<f32>(1.0, 2.0) - pp, dir, len2, lob, clp); aC = aC + o * w; aW = aW + w;
    let mn = min(min(f, g), min(j, k));
    let mx = max(max(f, g), max(j, k));
    return vec4<f32>(clamp(aC / aW, mn, mx), 1.0);
}

@fragment
fn fs_rcas(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy);
    let b = ld(t0, p.x, p.y - 1);
    let d = ld(t0, p.x - 1, p.y);
    let e = ld(t0, p.x, p.y);
    let f = ld(t0, p.x + 1, p.y);
    let h = ld(t0, p.x, p.y + 1);
    let mn4 = min(min(b, d), min(f, h));
    let mx4 = max(max(b, d), max(f, h));
    let hitMin = mn4 / (4.0 * max(mx4, vec3<f32>(0.000001)));
    let hitMax = (vec3<f32>(1.0) - mx4) / min(4.0 * mn4 - vec3<f32>(4.0), vec3<f32>(-0.000001));
    let lobeRGB = max(-hitMin, hitMax);
    let lobe = max(-0.1875, min(max(lobeRGB.r, max(lobeRGB.g, lobeRGB.b)), 0.0)) * u.c.z;
    let rcpL = 1.0 / (4.0 * lobe + 1.0);
    return vec4<f32>(clamp((lobe * (b + d + f + h) + e) * rcpL, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

// ---- composition ----
fn colour(c: vec3<f32>) -> vec3<f32> {
    var x = (c - vec3<f32>(0.5)) * u.d.w + vec3<f32>(0.5 + u.d.z);
    let l = dot(x, vec3<f32>(0.2126, 0.7152, 0.0722));
    x = mix(vec3<f32>(l), x, u.e.x);
    return pow(clamp(x, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / u.e.y));
}

@fragment
fn fs_compose(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = pos.xy;
    let win = u.b.xy;
    let rmin = u.b.zw;
    let rsize = u.c.xy;
    let inside = p.x >= rmin.x && p.x < rmin.x + rsize.x && p.y >= rmin.y && p.y < rmin.y + rsize.y;
    var col = vec3<f32>(0.0);
    if (inside) {
        let uv = (p - rmin) / rsize;
        col = colour(textureSampleLevel(t0, smp_l, uv, 0.0).rgb);
        if (u.c.w > 0.0) {
            // the beam: bright in the middle of a console line, darker toward its edges
            let ph = fract(uv.y * u.e.w);
            let dd = abs(ph - 0.5) * 2.0;
            col = col * (1.0 - u.c.w * dd * dd) * (1.0 + 0.3 * u.c.w);
        }
        if (u.d.x > 0.0) {
            let m = i32(p.x) % 3;
            var mc = vec3<f32>(1.0 - u.d.x);
            mc[m] = 1.0;
            col = col * mc * (1.0 + 0.4 * u.d.x);
        }
    } else if (u.d.y > 0.5) {
        // the sides of a wider window: the picture itself, enlarged to cover it and blurred, darker
        let s = max(win.x / rsize.x, win.y / rsize.y);
        let uvc = (p - win * 0.5) / (rsize * s) + vec2<f32>(0.5);
        var acc = vec3<f32>(0.0);
        for (var j = -2; j <= 2; j = j + 1) {
            for (var i = -2; i <= 2; i = i + 1) {
                acc = acc + textureSampleLevel(t0, smp_l, clamp(uvc + vec2<f32>(f32(i), f32(j)) * 0.012, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).rgb;
            }
        }
        col = colour(acc / 25.0) * 0.45;
    }
    return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
struct Uni {
    a: [f32; 4],
    b: [f32; 4],
    c: [f32; 4],
    d: [f32; 4],
    e: [f32; 4],
    f: [f32; 4],
}

/// Everything the window's picture is made with.
#[derive(Clone, Copy, Debug)]
pub struct Opts {
    pub fsr: bool,
    /// RCAS sharpness in stops (0 sharpest)
    pub sharp: f32,
    /// 0 fit the window (4:3), 1 whole-number multiples with bars, 2 stretch to the window, 3 4:3 with a 16:9 fill
    pub scale_mode: u32,
    pub scan: f32,
    pub mask: f32,
    pub bright: f32,
    pub contrast: f32,
    pub sat: f32,
    pub gamma: f32,
    /// the console's lines in the picture (240, or 480 interlaced)
    pub lines: f32,
    /// the plain filter when FSR is off: true smooth, false nearest
    pub linear: bool,
    /// the scene on is a 3D one and the picture was projected for 16:9 (scale_mode 4 then fills the 16:9 window with it)
    pub wide_scene: bool,
    /// the pixel-art scaler (EPX over the console's pixels) before the upscaler
    pub pixel_art: bool,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts { fsr: true, sharp: 0.2, scale_mode: 0, scan: 0.0, mask: 0.0, bright: 0.0, contrast: 1.0, sat: 1.0, gamma: 1.0, lines: 240.0, linear: true, wide_scene: false, pixel_art: false }
    }
}

/// How two pictures become the one in between.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Between {
    Blend,
    Motion,
}

struct Sized {
    key: [u32; 8],
    cur: wgpu::Texture,
    prev: wgpu::Texture,
    mid: wgpu::Texture,
    mf: wgpu::Texture,
    epx: wgpu::Texture,
    up: wgpu::Texture,
    sharp: wgpu::Texture,
    canvas: wgpu::Texture,
    read: wgpu::Buffer,
    row: u32,
}

pub struct Present {
    device: wgpu::Device,
    queue: wgpu::Queue,
    bgl: wgpu::BindGroupLayout,
    ubuf: wgpu::Buffer,
    smp_l: wgpu::Sampler,
    smp_n: wgpu::Sampler,
    p_blend: wgpu::RenderPipeline,
    p_motion: wgpu::RenderPipeline,
    p_warp: wgpu::RenderPipeline,
    p_plain: wgpu::RenderPipeline,
    p_epx: wgpu::RenderPipeline,
    p_easu: wgpu::RenderPipeline,
    p_rcas: wgpu::RenderPipeline,
    p_compose: wgpu::RenderPipeline,
    sized: Option<Sized>,
}

/// Where the picture goes in a canvas of cw x ch: (x, y, w, h). The picture is a 4:3 television picture however many pixels
/// it has; `lines` is the console's number of lines in it.
pub fn content_rect(mode: u32, cw: usize, ch: usize, lines: f32, wide_scene: bool) -> (usize, usize, usize, usize) {
    let (cwf, chf) = (cw as f64, ch as f64);
    let fit = |k: f64| -> (usize, usize, usize, usize) {
        let w = ((4.0 * k).round() as usize).clamp(1, cw);
        let h = ((3.0 * k).round() as usize).clamp(1, ch);
        ((cw - w) / 2, (ch - h) / 2, w, h)
    };
    match mode {
        2 => (0, 0, cw, ch),
        4 if wide_scene => {
            let k = (cwf / 16.0).min(chf / 9.0);
            let w = ((16.0 * k).round() as usize).clamp(1, cw);
            let h = ((9.0 * k).round() as usize).clamp(1, ch);
            ((cw - w) / 2, (ch - h) / 2, w, h)
        }
        1 => {
            // whole multiples of the console's own size (4:3 of `lines` lines)
            let l = lines.max(1.0) as f64;
            let nat_w = l * 4.0 / 3.0;
            let k = (cwf / nat_w).min(chf / l).floor();
            if k >= 1.0 {
                let w = (nat_w * k).round() as usize;
                let h = (l * k).round() as usize;
                ((cw - w.min(cw)) / 2, (ch - h.min(ch)) / 2, w.min(cw), h.min(ch))
            } else {
                fit((cwf / 4.0).min(chf / 3.0))
            }
        }
        _ => fit((cwf / 4.0).min(chf / 3.0)),
    }
}

impl Present {
    pub fn new() -> Option<Present> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::PRIMARY, ..Default::default() });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).ok()?;
        device.on_uncaptured_error(Box::new(|e| eprintln!("graphics card (presentation): {}", e)));
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("present"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                tex_entry(0),
                tex_entry(1),
                tex_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 4, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                wgpu::BindGroupLayoutEntry { binding: 5, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
        let mk = |entry: &str, format: wgpu::TextureFormat| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &module, entry_point: "vs", buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: entry,
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let c8 = wgpu::TextureFormat::Rgba8Unorm;
        let p_blend = mk("fs_blend", c8);
        let p_motion = mk("fs_motion", wgpu::TextureFormat::Rgba16Float);
        let p_warp = mk("fs_warp", c8);
        let p_plain = mk("fs_plain", c8);
        let p_epx = mk("fs_epx", c8);
        let p_easu = mk("fs_easu", c8);
        let p_rcas = mk("fs_rcas", c8);
        let p_compose = mk("fs_compose", c8);
        let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("present params"),
            size: std::mem::size_of::<Uni>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = |f: wgpu::FilterMode| device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: f,
            min_filter: f,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let smp_l = sampler(wgpu::FilterMode::Linear);
        let smp_n = sampler(wgpu::FilterMode::Nearest);
        Some(Present { device, queue, bgl, ubuf, smp_l, smp_n, p_blend, p_motion, p_warp, p_plain, p_epx, p_easu, p_rcas, p_compose, sized: None })
    }

    fn texture(&self, w: u32, h: u32, format: wgpu::TextureFormat, usage: wgpu::TextureUsages) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    }

    fn make_sized(&self, key: [u32; 8]) -> Sized {
        let [sw, sh, rw, rh, cw, ch, mw, mh] = key;
        use wgpu::TextureUsages as T;
        let c8 = wgpu::TextureFormat::Rgba8Unorm;
        let src_usage = T::TEXTURE_BINDING | T::COPY_DST;
        let rt_usage = T::TEXTURE_BINDING | T::RENDER_ATTACHMENT;
        let cur = self.texture(sw, sh, c8, src_usage);
        let prev = self.texture(sw, sh, c8, src_usage);
        let mid = self.texture(sw, sh, c8, rt_usage);
        let mf = self.texture(mw, mh, wgpu::TextureFormat::Rgba16Float, rt_usage);
        let epx = self.texture(mw * 2, mh * 2, c8, rt_usage);
        let up = self.texture(rw, rh, c8, rt_usage);
        let sharp = self.texture(rw, rh, c8, rt_usage);
        let canvas = self.texture(cw, ch, c8, T::RENDER_ATTACHMENT | T::COPY_SRC);
        let row = (cw * 4 + 255) & !255;
        let read = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("present readback"),
            size: row as u64 * ch as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Sized { key, cur, prev, mid, mf, epx, up, sharp, canvas, read, row }
    }

    fn upload(&self, t: &wgpu::Texture, px: &[u32], w: usize, h: usize) {
        let mut bytes = Vec::with_capacity(w * h * 4);
        for &p in px { bytes.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]); }
        self.queue.write_texture(
            wgpu::ImageCopyTexture { texture: t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &bytes,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(w as u32 * 4), rows_per_image: Some(h as u32) },
            wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        );
    }

    /// The window's picture for `cur` (sw x sh, 0x00RRGGBB) in a canvas of cw x ch. With `prev` and `between` the picture is the
    /// one halfway between the two. False when the card could not do it.
    pub fn frame(&mut self, cur: &[u32], sw: usize, sh: usize, prev: Option<&[u32]>, between: Option<Between>, cw: usize, ch: usize,
                 o: &Opts, out: &mut Vec<u32>) -> bool {
        if sw == 0 || sh == 0 || cw == 0 || ch == 0 || cur.len() != sw * sh || cw > 8192 || ch > 8192 { return false; }
        let (rx, ry, rw, rh) = content_rect(o.scale_mode, cw, ch, o.lines, o.wide_scene);
        // the motion field: one cell per console pixel
        let step = ((sh as f32 / o.lines.max(1.0)).round().max(1.0)) as u32;
        let (mw, mh) = ((sw as u32 + step - 1) / step, (sh as u32 + step - 1) / step);
        let key = [sw as u32, sh as u32, rw as u32, rh as u32, cw as u32, ch as u32, mw, mh];
        if self.sized.as_ref().map(|s| s.key != key).unwrap_or(true) { self.sized = Some(self.make_sized(key)); }
        let z = self.sized.as_ref().unwrap();
        self.upload(&z.cur, cur, sw, sh);
        let interp = match (prev, between) {
            (Some(p), Some(b)) if p.len() == sw * sh => { self.upload(&z.prev, p, sw, sh); Some(b) }
            _ => None,
        };
        // the size of the picture that goes up to the window: the scaler's output (twice the console's) or the render's
        let (iw, ih) = if o.pixel_art { (mw as usize * 2, mh as usize * 2) } else { (sw, sh) };
        let use_fsr = o.fsr && rw as f32 > iw as f32 * 1.05 && rh as f32 > ih as f32 * 1.05;
        let mut u = Uni::default();
        u.a = [sw as f32, sh as f32, rw as f32, rh as f32];
        u.b = [cw as f32, ch as f32, rx as f32, ry as f32];
        u.c = [rw as f32, rh as f32, (-o.sharp).exp2(), o.scan];
        u.d = [o.mask, if o.scale_mode == 3 || (o.scale_mode == 4 && !o.wide_scene) { 1.0 } else { 0.0 }, o.bright, o.contrast];
        u.e = [o.sat, o.gamma.max(0.1), 0.5, o.lines];
        u.f = [step as f32, if o.linear { 1.0 } else { 0.0 }, mw as f32, mh as f32];
        self.queue.write_buffer(&self.ubuf, 0, bytemuck::bytes_of(&u));
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = |a: &wgpu::Texture, b: &wgpu::Texture, c: &wgpu::Texture| self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view(a)) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view(b)) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&view(c)) },
                wgpu::BindGroupEntry { binding: 3, resource: self.ubuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.smp_l) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&self.smp_n) },
            ],
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let pass = |enc: &mut wgpu::CommandEncoder, pipe: &wgpu::RenderPipeline, bg: &wgpu::BindGroup, target: &wgpu::Texture| {
            let v = view(target);
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &v,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rp.set_pipeline(pipe);
            rp.set_bind_group(0, bg, &[]);
            rp.draw(0..3, 0..1);
        };
        // 1. the picture to show: the current one, or the one between
        let source: &wgpu::Texture = match interp {
            Some(Between::Blend) => {
                pass(&mut enc, &self.p_blend, &bind(&z.prev, &z.cur, &z.cur), &z.mid);
                &z.mid
            }
            Some(Between::Motion) => {
                pass(&mut enc, &self.p_motion, &bind(&z.prev, &z.cur, &z.cur), &z.mf);
                pass(&mut enc, &self.p_warp, &bind(&z.prev, &z.cur, &z.mf), &z.mid);
                &z.mid
            }
            None => &z.cur,
        };
        let source: &wgpu::Texture = if o.pixel_art {
            pass(&mut enc, &self.p_epx, &bind(source, source, source), &z.epx);
            &z.epx
        } else {
            source
        };
        // 2. up to the size of its place in the window
        let content: &wgpu::Texture = if use_fsr {
            pass(&mut enc, &self.p_easu, &bind(source, source, source), &z.up);
            pass(&mut enc, &self.p_rcas, &bind(&z.up, &z.up, &z.up), &z.sharp);
            &z.sharp
        } else {
            pass(&mut enc, &self.p_plain, &bind(source, source, source), &z.up);
            &z.up
        };
        // 3. into the window's image
        pass(&mut enc, &self.p_compose, &bind(content, content, content), &z.canvas);
        enc.copy_texture_to_buffer(
            wgpu::ImageCopyTexture { texture: &z.canvas, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::ImageCopyBuffer { buffer: &z.read, layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(z.row), rows_per_image: Some(ch as u32) } },
            wgpu::Extent3d { width: cw as u32, height: ch as u32, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(enc.finish()));
        let slice = z.read.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        if !crate::gpu::wait_mapped(&self.device, &rx) { return false; }
        {
            let data = slice.get_mapped_range();
            out.clear();
            out.reserve(cw * ch);
            for y in 0..ch {
                let row = &data[y * z.row as usize..y * z.row as usize + cw * 4];
                for x in 0..cw {
                    out.push((row[x * 4] as u32) << 16 | (row[x * 4 + 1] as u32) << 8 | row[x * 4 + 2] as u32);
                }
            }
        }
        z.read.unmap();
        true
    }
}
