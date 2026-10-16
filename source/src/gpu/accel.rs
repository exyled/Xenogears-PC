// The large picture drawn by the PC's graphics card (wgpu, with no window of its own): the same primitives that hi.rs draws
// in software go to a render target of 1024 x 512 video-memory pixels times the size factor, as triangles and sprites that
// read their texels from a copy of the program's own video memory (a 16-bit texture), with the palette, the texture window
// and the console's four semi-transparency modes done in the shader and the blend unit. The program's own video memory is
// still drawn by the software GPU (it is what the program reads back); the changes it makes are uploaded to the card's copy
// when a textured primitive is going to read them.
//
// On by default whenever a card can be opened (XG_GPU=0 keeps the software picture); anything that goes wrong creating the
// card's resources leaves the software picture.

use super::*;
use bytemuck::{Pod, Zeroable};

// The wide picture of the 16:9 scenes. The target is 1024 rows tall: the lower half holds, for each of the two draw buffers, a
// copy of the buffer that is WIDE_W pixels wide (the 320 of the console and WIDE_E more on each side). The scene's own geometry is
// drawn there with the squeeze of the geometry coprocessor taken out again, and the interface is laid out as the console drew it, in
// the middle.

const WIDE_E: i32 = 54;
const WIDE_TOP: i32 = 512;
const WIDE_STEP: i32 = 256;

const SHADER: &str = r#"
struct Globals { k: f32, filt: u32, p0: u32, p1: u32 };
@group(0) @binding(0) var vram: texture_2d<u32>;
@group(0) @binding(1) var<uniform> g: Globals;
@group(0) @binding(2) var repl_tex: texture_2d_array<f32>;
@group(0) @binding(3) var repl_smp: sampler;

struct VIn {
    @location(0) pos: vec2<f32>,
    @location(1) w: f32,
    @location(2) col: vec3<f32>,
    @location(3) uv: vec2<f32>,
    @location(4) st: vec4<u32>,
    @location(5) st2: vec2<u32>,
};
struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(linear) col: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) st: vec4<u32>,
    @location(3) @interpolate(flat) st2: vec2<u32>,
};

@vertex
fn vs(v: VIn) -> VOut {
    var o: VOut;
    let x = v.pos.x / 1024.0 * 2.0 - 1.0;
    let y = 1.0 - v.pos.y / 1024.0 * 2.0;
    o.clip = vec4<f32>(x * v.w, y * v.w, 0.0, v.w);
    o.col = v.col;
    o.uv = v.uv;
    o.st = v.st;
    o.st2 = v.st2;
    return o;
}

fn vload(x: i32, y: i32) -> u32 {
    return textureLoad(vram, vec2<i32>(x & 1023, y & 511), 0).r;
}

// one texel through the texture window and the palette: 0 = transparent, else bit 15 = semi-transparent, and the colour
fn texel(ui: i32, vi: i32, st: vec4<u32>) -> u32 {
    let flags = st.z;
    if ((flags & 128u) != 0u) {
        return vload(ui, vi);
    }
    var u = ui;
    var v = vi;
    if ((flags & 8u) != 0u) {
        let mx = i32((flags >> 8u) & 255u);
        let my = i32((flags >> 16u) & 255u);
        let ox = i32((flags >> 24u) & 255u);
        let oy = i32(st.w & 255u);
        u = (u & ~mx) | (ox & mx);
        v = (v & ~my) | (oy & my);
    }
    u = u & 255;
    v = v & 255;
    let tp_x = i32(st.x & 0xFFFFu);
    let tp_y = i32(st.x >> 16u);
    let depth = i32(st.y & 3u);
    let clut_x = i32((st.y >> 2u) & 0xFFFu);
    let clut_y = i32((st.y >> 14u) & 0x1FFu);
    let ty = (tp_y + v) & 511;
    if (depth == 0) {
        let wd = vload(tp_x + (u >> 2), ty);
        let idx = i32((wd >> u32((u & 3) * 4)) & 15u);
        return vload(clut_x + idx, clut_y);
    } else if (depth == 1) {
        let wd = vload(tp_x + (u >> 1), ty);
        let idx = i32((wd >> u32((u & 1) * 8)) & 255u);
        return vload(clut_x + idx, clut_y);
    }
    return vload(tp_x + u, ty);
}

fn rgb_of(t: u32) -> vec3<f32> {
    let r = f32(t & 31u);
    let gg = f32((t >> 5u) & 31u);
    let b = f32((t >> 10u) & 31u);
    return vec3<f32>(floor(r * 8.0 + r / 4.0), floor(gg * 8.0 + gg / 4.0), floor(b * 8.0 + b / 4.0));
}

// how unlike two texels are (transparent against anything counts as very unlike)
fn dist(a: u32, b: u32) -> f32 {
    if (a == 0u && b == 0u) { return 0.0; }
    if (a == 0u || b == 0u) { return 1000.0; }
    let d = abs(rgb_of(a) - rgb_of(b));
    return d.x + d.y + d.z;
}

// filter 3: a texel with the console's dither pattern taken out. Where the nine texels around it are all close in colour (a
// flat or smoothly shaded area that was dithered, not an edge) it is the 1-2-1 weighted mean of the nine.
fn dd_color(cu: i32, cv: i32, st: vec4<u32>, b: vec4<i32>) -> vec3<f32> {
    let c0 = texel(cu, cv, st);
    if (c0 == 0u) { return vec3<f32>(0.0); }
    let r0 = rgb_of(c0);
    var acc = vec3<f32>(0.0);
    var maxd = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let t = texel(clamp(cu + dx, b.x, b.y), clamp(cv + dy, b.z, b.w), st);
            if (t == 0u) { return r0; }
            let c = rgb_of(t);
            let w = f32((2 - abs(dx)) * (2 - abs(dy)));
            acc = acc + w * c;
            let d = abs(c - r0);
            maxd = max(maxd, d.x + d.y + d.z);
        }
    }
    if (maxd <= 56.0) { return acc / 16.0; }
    return r0;
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let flags = i.st.z;
    var rgb = i.col;
    if ((flags & 1u) != 0u) {
        // bounding rectangle of the primitive's texels
        let bu0 = i32(i.st2.x & 0xFFFFu) - 4096;
        let bu1 = i32(i.st2.x >> 16u) - 4096;
        let bv0 = i32(i.st2.y & 0xFFFFu) - 4096;
        let bv1 = i32(i.st2.y >> 16u) - 4096;
        let uf = i.uv.x;
        let vf = i.uv.y;
        var found = false;
        var semi = false;
        var tcol = vec3<f32>(0.0);
        let opaque_zero = (flags & 16u) != 0u;
        let repl = i.st.w >> 8u;
        if (repl != 0u && (flags & 128u) == 0u) {
            // a replacement picture of the texture page (4 times the size, colours already through the palette): the
            // position inside the page after the texture window, sampled smoothly, never outside the primitive's own texels
            var ur = clamp(uf, f32(bu0) - 0.375, f32(bu1) + 0.375);
            var vr = clamp(vf, f32(bv0) - 0.375, f32(bv1) + 0.375);
            if ((flags & 8u) != 0u) {
                let mx = i32((flags >> 8u) & 255u);
                let my = i32((flags >> 16u) & 255u);
                let ox = i32((flags >> 24u) & 255u);
                let oy = i32(i.st.w & 255u);
                let cu = floor(ur + 0.5);
                let cv = floor(vr + 0.5);
                ur = f32((i32(cu) & ~mx) | (ox & mx)) + (ur - cu);
                vr = f32((i32(cv) & ~my) | (oy & my)) + (vr - cv);
            }
            let um = ur - 256.0 * floor((ur + 0.5) / 256.0);
            let vm = vr - 256.0 * floor((vr + 0.5) / 256.0);
            let c = textureSampleLevel(repl_tex, repl_smp, vec2<f32>((um + 0.5) / 256.0, (vm + 0.5) / 256.0), i32(repl - 1u), 0.0);
            let t0 = texel(clamp(i32(floor(uf + 0.5)), bu0, bu1), clamp(i32(floor(vf + 0.5)), bv0, bv1), i.st);
            if (c.a >= 0.5) {
                found = true;
                semi = (t0 & 0x8000u) != 0u;
                tcol = floor(c.rgb * 255.0 + 0.5);
            }
        } else if ((flags & 4u) != 0u || g.filt == 0u) {
            let iu = clamp(i32(floor(uf + 0.5)), bu0, bu1);
            let iv = clamp(i32(floor(vf + 0.5)), bv0, bv1);
            let t = texel(iu, iv, i.st);
            if (t != 0u || opaque_zero) {
                found = true;
                semi = (t & 0x8000u) != 0u;
                tcol = rgb_of(t);
            }
        } else {
            let fu = floor(uf);
            let fv = floor(vf);
            let iu = i32(fu);
            let iv = i32(fv);
            var k = 1.0;
            if (g.filt == 1u || g.filt == 3u) { k = g.k; }
            let au = clamp(((uf - fu) - 0.5) * k + 0.5, 0.0, 1.0);
            let av = clamp(((vf - fv) - 0.5) * k + 0.5, 0.0, 1.0);
            let ua = clamp(iu, bu0, bu1);
            let ub = clamp(iu + 1, bu0, bu1);
            let va = clamp(iv, bv0, bv1);
            let vb = clamp(iv + 1, bv0, bv1);
            var t = array<u32, 4>(texel(ua, va, i.st), texel(ub, va, i.st), texel(ua, vb, i.st), texel(ub, vb, i.st));
            var w = array<f32, 4>((1.0 - au) * (1.0 - av), au * (1.0 - av), (1.0 - au) * av, au * av);
            if ((g.filt == 1u || g.filt == 3u) && au > 0.0 && av > 0.0 && !(t[0] == t[1] && t[0] == t[2] && t[0] == t[3])) {
                // the cell is split along the diagonal the edge runs along, and the colour is interpolated inside the
                // triangle the position falls in
                let d1 = dist(t[0], t[3]);
                let d2 = dist(t[1], t[2]);
                var main_diag = d1 <= d2;
                if (abs(d1 - d2) <= 40.0) {
                    let tmm = texel(ua - 1, va - 1, i.st);
                    let tpm = texel(ub + 1, va - 1, i.st);
                    let tmp = texel(ua - 1, vb + 1, i.st);
                    let tpp = texel(ub + 1, vb + 1, i.st);
                    let dm = d1 * 2.0 + dist(tmm, t[0]) + dist(t[3], tpp);
                    let da = d2 * 2.0 + dist(tpm, t[1]) + dist(t[2], tmp);
                    main_diag = dm <= da;
                }
                if (main_diag) {
                    if (au >= av) { w = array<f32, 4>(1.0 - au, au - av, 0.0, av); }
                    else { w = array<f32, 4>(1.0 - av, 0.0, av - au, au); }
                } else if (au + av <= 1.0) {
                    w = array<f32, 4>(1.0 - au - av, au, av, 0.0);
                } else {
                    w = array<f32, 4>(0.0, 1.0 - av, 1.0 - au, au + av - 1.0);
                }
            }
            var acc = vec3<f32>(0.0);
            var wsum = 0.0;
            var best = -1.0;
            var near = 0u;
            let bnd = vec4<i32>(bu0, bu1, bv0, bv1);
            var cu4 = array<i32, 4>(ua, ub, ua, ub);
            var cv4 = array<i32, 4>(va, va, vb, vb);
            for (var n = 0; n < 4; n = n + 1) {
                if (w[n] > best) { best = w[n]; near = t[n]; }
                if (t[n] != 0u && w[n] > 0.0) {
                    var c = rgb_of(t[n]);
                    if (g.filt == 3u && (flags & 128u) == 0u) { c = dd_color(cu4[n], cv4[n], i.st, bnd); }
                    acc = acc + w[n] * c;
                    wsum = wsum + w[n];
                }
            }
            if (wsum >= 0.5) {
                found = true;
                tcol = floor(acc / wsum + 0.5);
                semi = near != 0u && (near & 0x8000u) != 0u;
            }
        }
        if (!found) { discard; }
        let pf = (flags >> 5u) & 3u;
        if (pf == 1u && semi) { discard; }
        if (pf == 2u && !semi) { discard; }
        if ((flags & 2u) != 0u) {
            rgb = tcol;
        } else {
            rgb = min(floor(tcol * clamp(rgb, vec3<f32>(0.0), vec3<f32>(255.0)) / 128.0), vec3<f32>(255.0));
        }
    }
    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(255.0));
    return vec4<f32>(rgb / 255.0, 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vtx {
    pos: [f32; 2],
    w: f32,
    col: [f32; 3],
    uv: [f32; 2],
    st: [u32; 4],
    st2: [u32; 2],
}

enum Op {
    Upload { x: i32, y: i32, w: i32, h: i32, data: Vec<u16> },
    Draw { pipe: usize, scissor: [u32; 4], first: u32, count: u32 },
    Move { sx: i32, sy: i32, w: i32, h: i32, dx: i32, dy: i32 },
}

/// the size of a replacement picture of a 256 x 256 texture page (4 times)
pub const REPL_SIZE: u32 = 2048;
const F_TEX: u32 = 1;
const F_RAW: u32 = 2;
const F_NEAREST: u32 = 4;
const F_WIN: u32 = 8;
const F_ZERO_OPAQUE: u32 = 16;
const F_BLIT: u32 = 128;

pub struct Accel {
    device: wgpu::Device,
    queue: wgpu::Queue,
    vram: wgpu::Texture,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    scratch: wgpu::Texture,
    globals: wgpu::Buffer,
    bind: wgpu::BindGroup,
    bgl: wgpu::BindGroupLayout,
    vram_view: wgpu::TextureView,
    repl_view: wgpu::TextureView,
    repl_smp: wgpu::Sampler,
    repl_tex: wgpu::Texture,
    pipes: Vec<wgpu::RenderPipeline>,
    vbuf: wgpu::Buffer,
    vcap: usize,
    verts: Vec<Vtx>,
    ops: Vec<Op>,
    scale: i32,
    filter: i32,
    dirty: Option<[i32; 4]>,
}

impl Accel {
    pub fn new(scale: i32, filter: i32) -> Option<Accel> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::PRIMARY, ..Default::default() });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).ok()?;
        // a problem on the card is reported, not fatal: the picture just keeps what it had
        device.on_uncaptured_error(Box::new(|e| eprintln!("graphics card: {}", e)));
        let s = scale.clamp(1, 4) as u32;
        let vram = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vram"),
            size: wgpu::Extent3d { width: 1024, height: 512, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let vram_view = vram.create_view(&wgpu::TextureViewDescriptor::default());
        let mk_target = |label: &str| device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: 1024 * s, height: 1024 * s, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let target = mk_target("target");
        let scratch = mk_target("scratch");
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        // replacement pictures of texture pages: none until set_replacements gives some (one empty layer meanwhile)
        let repl_smp = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let dummy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("repl none"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let repl_view = dummy.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&vram_view) },
                wgpu::BindGroupEntry { binding: 1, resource: globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&repl_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&repl_smp) },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("xg"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
        let attrs = [
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32, offset: 8, shader_location: 1 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 2 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 24, shader_location: 3 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32x4, offset: 32, shader_location: 4 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32x2, offset: 48, shader_location: 5 },
        ];
        let vlayout = wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vtx>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attrs };
        let k = |sf: wgpu::BlendFactor, df: wgpu::BlendFactor, op: wgpu::BlendOperation| {
            let c = wgpu::BlendComponent { src_factor: sf, dst_factor: df, operation: op };
            Some(wgpu::BlendState { color: c, alpha: c })
        };
        use wgpu::{BlendFactor as F, BlendOperation as O};
        let blends = [
            None,
            k(F::Constant, F::Constant, O::Add),    // 0: half of each
            k(F::One, F::One, O::Add),              // 1: add
            k(F::One, F::One, O::ReverseSubtract),  // 2: background minus foreground
            k(F::Constant, F::One, O::Add),         // 3: background plus a quarter
        ];
        let pipes: Vec<wgpu::RenderPipeline> = blends.iter().enumerate().map(|(n, b)| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&format!("pipe{}", n)),
                layout: Some(&layout),
                vertex: wgpu::VertexState { module: &module, entry_point: "vs", buffers: &[vlayout.clone()], compilation_options: Default::default() },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: "fs",
                    targets: &[Some(wgpu::ColorTargetState { format: wgpu::TextureFormat::Rgba8Unorm, blend: *b, write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        }).collect();
        let vcap = 1 << 16;
        let vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("verts"),
            size: (vcap * std::mem::size_of::<Vtx>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut a = Accel { device, queue, vram, target, target_view, scratch, globals, bind, bgl, vram_view, repl_view, repl_smp, repl_tex: dummy, pipes, vbuf, vcap, verts: Vec::new(), ops: Vec::new(), scale: s as i32, filter, dirty: None };
        a.write_globals();
        Some(a)
    }

    fn write_globals(&mut self) {
        let mut k = (self.scale as f32 * 0.5).max(1.0);
        if self.filter == 3 {
            // the enhanced filter spreads each texel's edge over about one output pixel per texel of the picture's size
            k = std::env::var("XG_ENH_K").ok().and_then(|v| v.parse().ok()).unwrap_or((self.scale as f32 * 0.5).max(1.0).min(1.25));
        }
        let data: [u32; 4] = [k.to_bits(), self.filter as u32, 0, 0];
        self.queue.write_buffer(&self.globals, 0, bytemuck::cast_slice(&data));
    }

    /// Puts one replacement picture into a layer of the array (the drawing that is waiting goes first: it may be reading the layer).
    pub fn write_replacement(&mut self, layer: u32, data: &[u8]) {
        if data.len() != (REPL_SIZE * REPL_SIZE * 4) as usize { return; }
        self.flush();
        self.queue.write_texture(
            wgpu::ImageCopyTexture { texture: &self.repl_tex, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: 0, z: layer }, aspect: wgpu::TextureAspect::All },
            data,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(REPL_SIZE * 4), rows_per_image: Some(REPL_SIZE) },
            wgpu::Extent3d { width: REPL_SIZE, height: REPL_SIZE, depth_or_array_layers: 1 },
        );
    }

    /// Replacement pictures of texture pages (RGBA, REPL_SIZE x REPL_SIZE each): layer n + 1 is what a primitive whose
    /// vertices carry n + 1 samples.
    pub fn set_replacements(&mut self, layers: &[Vec<u8>]) {
        self.flush();
        let n = layers.len().max(1) as u32;
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("replacements"),
            size: wgpu::Extent3d { width: REPL_SIZE, height: REPL_SIZE, depth_or_array_layers: n },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (i, data) in layers.iter().enumerate() {
            if data.len() != (REPL_SIZE * REPL_SIZE * 4) as usize { continue; }
            self.queue.write_texture(
                wgpu::ImageCopyTexture { texture: &tex, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: 0, z: i as u32 }, aspect: wgpu::TextureAspect::All },
                data,
                wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(REPL_SIZE * 4), rows_per_image: Some(REPL_SIZE) },
                wgpu::Extent3d { width: REPL_SIZE, height: REPL_SIZE, depth_or_array_layers: 1 },
            );
        }
        self.repl_view = tex.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
        self.repl_tex = tex;
        self.bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&self.vram_view) },
                wgpu::BindGroupEntry { binding: 1, resource: self.globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.repl_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.repl_smp) },
            ],
        });
    }

    pub fn set_filter(&mut self, f: i32) {
        self.flush();
        self.filter = f;
        self.write_globals();
    }

    /// Video memory pixels (x, y, w, h) changed in the program's own video memory by the software drawing: the card's copy
    /// is brought up to date before a primitive reads them.
    pub fn mark_dirty(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        let r = [x0.max(0), y0.max(0), x1.min(1023), y1.min(511)];
        if r[0] > r[2] || r[1] > r[3] { return; }
        self.dirty = Some(match self.dirty {
            None => r,
            Some(d) => [d[0].min(r[0]), d[1].min(r[1]), d[2].max(r[2]), d[3].max(r[3])],
        });
    }

    fn sync_rect(&mut self, vram: &[u16], r: [i32; 4]) {
        if let Some(d) = self.dirty {
            if d[0] <= r[2] && d[2] >= r[0] && d[1] <= r[3] && d[3] >= r[1] {
                self.upload(vram, d[0], d[1], d[2] - d[0] + 1, d[3] - d[1] + 1, false);
                self.dirty = None;
            }
        }
    }

    /// The pixels (as the program's video memory has them now) go to the card's copy, and when `show`, into the picture too.
    pub fn upload(&mut self, vram: &[u16], x: i32, y: i32, w: i32, h: i32, show: bool) {
        if w <= 0 || h <= 0 { return; }
        let (x, y) = (x & 1023, y & 511);
        // split at the edges of video memory
        let w1 = w.min(1024 - x);
        let h1 = h.min(512 - y);
        let pieces = [(x, y, w1, h1), (0, y, w - w1, h1), (x, 0, w1, h - h1), (0, 0, w - w1, h - h1)];
        for &(px, py, pw, ph) in &pieces {
            if pw <= 0 || ph <= 0 { continue; }
            let mut data = Vec::with_capacity((pw * ph) as usize);
            for j in 0..ph {
                let row = ((py + j) * 1024 + px) as usize;
                data.extend_from_slice(&vram[row..row + pw as usize]);
            }
            self.ops.push(Op::Upload { x: px, y: py, w: pw, h: ph, data });
            if show {
                self.quad_blit(px, py, pw, ph);
            }
        }
        if self.ops.len() > 4000 || self.verts.len() > 150_000 { self.flush(); }
    }

    fn push_draw(&mut self, pipe: usize, scissor: [u32; 4], verts: &[Vtx]) {
        let first = self.verts.len() as u32;
        self.verts.extend_from_slice(verts);
        if let Some(Op::Draw { pipe: p, scissor: s, first: f, count: c }) = self.ops.last_mut() {
            if *p == pipe && *s == scissor && *f + *c == first {
                *c += verts.len() as u32;
                return;
            }
        }
        self.ops.push(Op::Draw { pipe, scissor, first, count: verts.len() as u32 });
    }

    fn full_scissor(&self) -> [u32; 4] { [0, 0, 1024 * self.scale as u32, 512 * self.scale as u32] }

    fn area_scissor(&self, a: [i32; 4]) -> [u32; 4] {
        let s = self.scale;
        let x0 = (a[0].max(0) * s) as u32;
        let y0 = (a[1].max(0) * s) as u32;
        let x1 = (((a[2].min(1023)) + 1) * s) as u32;
        let y1 = (((a[3].min(511)) + 1) * s) as u32;
        [x0, y0, x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1)]
    }

    fn quad(&mut self, pipe: usize, scissor: [u32; 4], p: [[f32; 2]; 4], c: [f32; 3], uv: [[f32; 2]; 4], st: [u32; 4], st2: [u32; 2]) {
        let v = |i: usize| Vtx { pos: p[i], w: 1.0, col: c, uv: uv[i], st, st2 };
        let t = [v(0), v(1), v(2), v(0), v(2), v(3)];
        self.push_draw(pipe, scissor, &t);
    }

    /// The card's copy of this rectangle of video memory, shown as it is (the 15-bit colours, one video-memory pixel to a
    /// block of the picture).
    fn quad_blit(&mut self, x: i32, y: i32, w: i32, h: i32) {
        let (x0, y0, x1, y1) = (x as f32, y as f32, (x + w) as f32, (y + h) as f32);
        let sc = self.full_scissor();
        // texel centres at whole numbers: the edges are half a texel out
        let uv = [[x0 - 0.5, y0 - 0.5], [x1 - 0.5, y0 - 0.5], [x1 - 0.5, y1 - 0.5], [x0 - 0.5, y1 - 0.5]];
        let bb = pack_bb([0, 1023, 0, 511]);
        self.quad(0, sc, [[x0, y0], [x1, y0], [x1, y1], [x0, y1]], [0.0; 3], uv,
            [0, 2, F_TEX | F_RAW | F_NEAREST | F_ZERO_OPAQUE | F_BLIT, 0], bb);
    }

    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: [f32; 3]) {
        if w <= 0 || h <= 0 { return; }
        let (x, y) = (x & 1023, y & 511);
        let w1 = w.min(1024 - x);
        let h1 = h.min(512 - y);
        let sc = self.full_scissor();
        for &(px, py, pw, ph) in &[(x, y, w1, h1), (0, y, w - w1, h1), (x, 0, w1, h - h1), (0, 0, w - w1, h - h1)] {
            if pw <= 0 || ph <= 0 { continue; }
            let (x0, y0, x1, y1) = (px as f32, py as f32, (px + pw) as f32, (py + ph) as f32);
            self.quad(0, sc, [[x0, y0], [x1, y0], [x1, y1], [x0, y1]], c, [[0.0; 2]; 4], [0; 4], [0; 2]);
        }
    }

    pub fn move_rect(&mut self, sx: i32, sy: i32, w: i32, h: i32, dx: i32, dy: i32, vram: &[u16]) {
        if w <= 0 || h <= 0 { return; }
        let inside = |x: i32, y: i32| x >= 0 && y >= 0 && x + w <= 1024 && y + h <= 512;
        if inside(sx, sy) && inside(dx, dy) {
            self.ops.push(Op::Move { sx, sy, w, h, dx, dy });
        } else {
            // across the edge of video memory: the console-resolution result is shown
            let _ = vram;
            self.mark_dirty(dx & 1023, dy & 511, (dx & 1023) + w - 1, (dy & 511) + h - 1);
        }
    }

    pub fn flush(&mut self) {
        if self.ops.is_empty() { return; }
        if self.verts.len() > self.vcap {
            self.vcap = self.verts.len().next_power_of_two();
            self.vbuf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("verts"),
                size: (self.vcap * std::mem::size_of::<Vtx>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !self.verts.is_empty() {
            self.queue.write_buffer(&self.vbuf, 0, bytemuck::cast_slice(&self.verts));
        }
        let s = self.scale as u32;
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let ops = std::mem::take(&mut self.ops);
        let mut pass: Option<wgpu::RenderPass<'static>> = None;
        for op in &ops {
            match op {
                Op::Upload { x, y, w, h, data } => {
                    drop(pass.take());
                    self.queue.submit(Some(encoder.finish()));
                    encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
                    self.queue.write_texture(
                        wgpu::ImageCopyTexture { texture: &self.vram, mip_level: 0, origin: wgpu::Origin3d { x: *x as u32, y: *y as u32, z: 0 }, aspect: wgpu::TextureAspect::All },
                        bytemuck::cast_slice(data),
                        wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some((*w * 2) as u32), rows_per_image: Some(*h as u32) },
                        wgpu::Extent3d { width: *w as u32, height: *h as u32, depth_or_array_layers: 1 },
                    );
                }
                Op::Draw { pipe, scissor, first, count } => {
                    if pass.is_none() {
                        let p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: None,
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &self.target_view,
                                resolve_target: None,
                                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                            })],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                        });
                        pass = Some(p.forget_lifetime());
                    }
                    let p = pass.as_mut().unwrap();
                    p.set_pipeline(&self.pipes[*pipe]);
                    p.set_bind_group(0, &self.bind, &[]);
                    p.set_vertex_buffer(0, self.vbuf.slice(..));
                    p.set_scissor_rect(scissor[0], scissor[1], scissor[2], scissor[3]);
                    let c = match *pipe { 1 => 0.5, 4 => 0.25, _ => 1.0 };
                    p.set_blend_constant(wgpu::Color { r: c, g: c, b: c, a: c });
                    p.draw(*first..*first + *count, 0..1);
                }
                Op::Move { sx, sy, w, h, dx, dy } => {
                    drop(pass.take());
                    let size = wgpu::Extent3d { width: (*w as u32) * s, height: (*h as u32) * s, depth_or_array_layers: 1 };
                    encoder.copy_texture_to_texture(
                        wgpu::ImageCopyTexture { texture: &self.target, mip_level: 0, origin: wgpu::Origin3d { x: *sx as u32 * s, y: *sy as u32 * s, z: 0 }, aspect: wgpu::TextureAspect::All },
                        wgpu::ImageCopyTexture { texture: &self.scratch, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                        size,
                    );
                    encoder.copy_texture_to_texture(
                        wgpu::ImageCopyTexture { texture: &self.scratch, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                        wgpu::ImageCopyTexture { texture: &self.target, mip_level: 0, origin: wgpu::Origin3d { x: *dx as u32 * s, y: *dy as u32 * s, z: 0 }, aspect: wgpu::TextureAspect::All },
                        size,
                    );
                }
            }
        }
        drop(pass.take());
        self.queue.submit(Some(encoder.finish()));
        self.verts.clear();
    }

    /// A rectangle of the picture (in video-memory pixels), as 0x00RRGGBB, (w * scale) x (h * scale), top row first.
    pub fn read(&mut self, x: i32, y: i32, w: i32, h: i32, out: &mut Vec<u32>) {
        self.flush();
        let s = self.scale as u32;
        let (pw, ph) = (w as u32 * s, h as u32 * s);
        let row_bytes = pw * 4;
        let padded = (row_bytes + 255) & !255;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read"),
            size: (padded * ph) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_buffer(
            wgpu::ImageCopyTexture { texture: &self.target, mip_level: 0, origin: wgpu::Origin3d { x: x as u32 * s, y: y as u32 * s, z: 0 }, aspect: wgpu::TextureAspect::All },
            wgpu::ImageCopyBuffer { buffer: &buf, layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(ph) } },
            wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(enc.finish()));
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        let mapped = wait_mapped(&self.device, &rx);
        out.clear();
        if mapped {
            let data = slice.get_mapped_range();
            out.reserve((pw * ph) as usize);
            for j in 0..ph {
                let row = &data[(j * padded) as usize..(j * padded + row_bytes) as usize];
                for p in row.chunks_exact(4) {
                    out.push((p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32);
                }
            }
            drop(data);
            buf.unmap();
        } else {
            out.resize((pw * ph) as usize, 0);
        }
    }
}

/// Waits for a mapped buffer for at most four seconds (a card that stopped answering must not freeze the game): true when it
/// was mapped without an error.
pub(crate) fn wait_mapped(device: &wgpu::Device, rx: &std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>) -> bool {
    let t0 = std::time::Instant::now();
    loop {
        device.poll(wgpu::Maintain::Poll);
        match rx.try_recv() {
            Ok(r) => return r.is_ok(),
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if t0.elapsed() > std::time::Duration::from_secs(4) { return false; }
                if t0.elapsed() < std::time::Duration::from_millis(3) { std::thread::yield_now(); } else { std::thread::sleep(std::time::Duration::from_micros(500)); }
            }
            Err(_) => return false,
        }
    }
}

// a rectangle of texel coordinates (u min, u max, v min, v max; each biased by 4096 so that it is unsigned) in two words
/// A triangle into the draw list: opaque, semi-transparent, or (textured and semi-transparent) the opaque texels first and then the
/// semi-transparent ones through the blend unit.
fn emit_tri(a: &mut Accel, sc: [u32; 4], t: [Vtx; 3], tex: bool, semi: bool, mode: usize) {
    if !semi {
        a.push_draw(0, sc, &t);
    } else if !tex {
        a.push_draw(1 + mode, sc, &t);
    } else {
        let mut t0 = t;
        for vt in t0.iter_mut() { vt.st[2] |= 1 << 5; }
        a.push_draw(0, sc, &t0);
        let mut t1 = t;
        for vt in t1.iter_mut() { vt.st[2] |= 2 << 5; }
        a.push_draw(1 + mode, sc, &t1);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_quad(a: &mut Accel, sc: [u32; 4], p: [[f32; 2]; 4], col: [f32; 3], uv: [[f32; 2]; 4], st: [u32; 4], st2: [u32; 2], tex: bool, semi: bool, mode: usize) {
    if !semi {
        a.quad(0, sc, p, col, uv, st, st2);
    } else if !tex {
        a.quad(1 + mode, sc, p, col, uv, st, st2);
    } else {
        let mut s0 = st;
        s0[2] |= 1 << 5;
        a.quad(0, sc, p, col, uv, s0, st2);
        let mut s1 = st;
        s1[2] |= 2 << 5;
        a.quad(1 + mode, sc, p, col, uv, s1, st2);
    }
}

fn pack_bb(b: [i32; 4]) -> [u32; 2] {
    let f = |v: i32| (v + 4096).clamp(0, 0xFFFF) as u32;
    [f(b[0]) | f(b[1]) << 16, f(b[2]) | f(b[3]) << 16]
}

impl Gpu {
    pub(super) fn acc_mut(&mut self) -> Option<&mut Accel> { self.acc.as_deref_mut() }

    /// Chooses the card's picture for this size factor (false: it could not be made and the software picture stays).
    pub(super) fn acc_start(&mut self, s: i32) -> bool {
        match Accel::new(s, self.filter) {
            Some(mut a) => {
                self.load_replacements(&mut a);
                self.acc = Some(Box::new(a));
                self.acc_on = true;
                self.acc_reseed();
                true
            }
            None => false,
        }
    }

    /// The folder XG_TEX_REPLACE names holds replacement pictures (files <16 hex digits>.rgba of REPL_SIZE x REPL_SIZE x 4
    /// bytes, made by tools/enhance.py from what XG_TEX_DUMP wrote). They are listed here and read when a page that has one is
    /// first drawn; XG_TEX_MAX layers (default 64) are on the card at a time, the one used longest ago makes room.
    fn load_replacements(&mut self, a: &mut Accel) {
        if let Ok(d) = std::env::var("XG_TEX_DUMP") {
            let p = std::path::PathBuf::from(d);
            if std::fs::create_dir_all(&p).is_ok() { self.dump_dir = Some(p); }
        }
        let dir = match std::env::var("XG_TEX_REPLACE") { Ok(d) => std::path::PathBuf::from(d), Err(_) => return };
        let cap: usize = std::env::var("XG_TEX_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(64).clamp(1, 1024);
        let mut files = std::collections::HashMap::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.filter_map(|e| e.ok()) {
                let f = e.path();
                if f.extension().and_then(|x| x.to_str()) != Some("rgba") { continue; }
                if let Some(key) = f.file_stem().and_then(|x| x.to_str()).and_then(|x| u64::from_str_radix(x, 16).ok()) {
                    files.insert(key, f);
                }
            }
        }
        if files.is_empty() { return; }
        eprintln!("texture replacements: {} pages in {}, {} on the card at a time", files.len(), dir.display(), cap.min(files.len()));
        let n = cap.min(files.len());
        a.set_replacements(&vec![Vec::new(); n]);
        self.repl_files = files;
        self.repl_slots = vec![(0, 0); n];
        // the files are read on a thread of their own: reading 16 MB from the disk while drawing used to stall the game
        let (rq_tx, rq_rx) = std::sync::mpsc::channel::<(u64, std::path::PathBuf)>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<(u64, Vec<u8>)>();
        let _ = std::thread::Builder::new().name("texture-reader".into()).spawn(move || {
            while let Ok((key, path)) = rq_rx.recv() {
                let data = std::fs::read(&path).unwrap_or_default();
                if done_tx.send((key, data)).is_err() { return; }
            }
        });
        self.repl_io = Some((std::sync::Mutex::new(rq_tx), std::sync::Mutex::new(done_rx)));
    }

    /// The layer holding the replacement of the page with this key (plus one); 0 while it is not on the card. A page that is
    /// not there yet is asked for from the reader thread, and put on the card (at most two a frame, so a frame never waits on the
    /// disk or on large uploads) when it has been read; until then the page shows as it is.
    fn replacement_slot(&mut self, key: u64) -> u32 {
        self.repl_tick += 1;
        if let Some(i) = self.repl_slots.iter().position(|e| e.1 != 0 && e.0 == key) {
            self.repl_slots[i].1 = self.repl_tick;
            return i as u32 + 1;
        }
        if self.repl_budget.0 != self.zstamp as u64 { self.repl_budget = (self.zstamp as u64, 0); }
        // pages that have been read: onto the card
        let mut ready: Vec<(u64, Vec<u8>)> = Vec::new();
        if let Some((_, rx)) = &self.repl_io {
            if let Ok(rx) = rx.lock() {
                while self.repl_budget.1 + (ready.len() as u32) < 2 {
                    match rx.try_recv() { Ok(r) => ready.push(r), Err(_) => break }
                }
            }
        }
        for (k, data) in ready {
            self.repl_pending.remove(&k);
            if data.len() != (REPL_SIZE * REPL_SIZE * 4) as usize { self.repl_files.remove(&k); continue; }
            // a free layer, else the one used longest ago
            let i = match self.repl_slots.iter().position(|e| e.1 == 0) {
                Some(i) => i,
                None => (0..self.repl_slots.len()).min_by_key(|&i| self.repl_slots[i].1).unwrap_or(0),
            };
            if let Some(a) = self.acc.as_mut() { a.write_replacement(i as u32, &data); } else { continue; }
            self.repl_budget.1 += 1;
            self.repl_slots[i] = (k, self.repl_tick);
        }
        if let Some(i) = self.repl_slots.iter().position(|e| e.1 != 0 && e.0 == key) {
            self.repl_slots[i].1 = self.repl_tick;
            return i as u32 + 1;
        }
        if !self.repl_pending.contains(&key) {
            if let (Some(path), Some((tx, _))) = (self.repl_files.get(&key), &self.repl_io) {
                if let Ok(tx) = tx.lock() {
                    if tx.send((key, path.clone())).is_ok() { self.repl_pending.insert(key); }
                }
            }
        }
        0
    }

    pub(super) fn acc_reseed(&mut self) {
        if let Some(a) = self.acc.as_mut() {
            a.ops.clear();
            a.verts.clear();
            a.dirty = None;
            a.upload(&self.vram, 0, 0, 1024, 512, true);
        }
    }

    pub(super) fn acc_fill(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, g: i32, b: i32) {
        self.wide_fill(x, y, w, h, [r as f32, g as f32, b as f32]);
        self.repl_invalidate(x & 1023, y & 511, (x & 1023) + w - 1, (y & 511) + h - 1);
        if let Some(a) = self.acc.as_mut() {
            a.fill(x, y, w, h, [r as f32, g as f32, b as f32]);
            a.mark_dirty(x & 1023, y & 511, (x & 1023) + w - 1, (y & 511) + h - 1);
        }
    }

    pub(super) fn acc_upload(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.wide_touch(x, y, w, h);
        self.repl_invalidate(x & 1023, y & 511, (x & 1023) + w - 1, (y & 511) + h - 1);
        if let Some(a) = self.acc.as_mut() {
            a.upload(&self.vram, x, y, w, h, true);
        }
    }

    pub(super) fn acc_move(&mut self, sx: i32, sy: i32, w: i32, h: i32, dx: i32, dy: i32) {
        self.wide_touch(dx, dy, w, h);
        self.repl_invalidate(dx & 1023, dy & 511, (dx & 1023) + w - 1, (dy & 511) + h - 1);
        if let Some(a) = self.acc.as_mut() {
            a.move_rect(sx, sy, w, h, dx, dy, &self.vram);
            a.mark_dirty(dx & 1023, dy & 511, (dx & 1023) + w - 1, (dy & 511) + h - 1);
        }
    }

    pub(super) fn acc_put_pixel(&mut self, x: i32, y: i32, r: i32, g: i32, b: i32, semi: bool) {
        let mode = self.tp_mode;
        if let Some(a) = self.acc.as_mut() {
            let (x, y) = ((x & 1023) as f32, (y & 511) as f32);
            let pipe = if semi { 1 + mode as usize } else { 0 };
            let sc = a.full_scissor();
            a.quad(pipe, sc, [[x, y], [x + 1.0, y], [x + 1.0, y + 1.0], [x, y + 1.0]], [r as f32, g as f32, b as f32], [[0.0; 2]; 4], [0; 4], [0; 2]);
            a.mark_dirty(x as i32, y as i32, x as i32, y as i32);
        }
    }

    /// The key of the texture page the next textured primitive reads (its texels and its palette, as the program holds them
    /// now), the layer of its replacement picture plus one (0: none), and, when the dump is asked for, the page as a picture.
    fn page_layer(&mut self, clut: i32) -> u32 {
        if self.repl_files.is_empty() && self.dump_dir.is_none() { return 0; }
        let depth = self.tp_depth & 3;
        let id = (self.tp_x, self.tp_y, depth, clut);
        let key = match self.repl_cache.iter().find(|e| e.0 == id) {
            Some(e) => e.1,
            None => {
                let per = match depth { 0 => 64, 1 => 128, _ => 256 };
                let mut h: u64 = 0xCBF29CE484222325;
                let mut mix = |v: u16| { h ^= v as u64; h = h.wrapping_mul(0x100000001B3); };
                mix(depth as u16);
                for r in 0..256 {
                    let y = ((self.tp_y + r) & 511) as usize;
                    for c in 0..per {
                        mix(self.vram[y * 1024 + ((self.tp_x + c) & 1023) as usize]);
                    }
                }
                if depth < 2 {
                    let n = if depth == 0 { 16 } else { 256 };
                    let cy = (((clut >> 6) & 0x1FF) & 511) as usize;
                    let cx = (clut & 0x3F) * 16;
                    for c in 0..n { mix(self.vram[cy * 1024 + ((cx + c) & 1023) as usize]); }
                }
                if self.repl_cache.len() > 64 { self.repl_cache.clear(); }
                self.repl_cache.push((id, h));
                h
            }
        };
        // collecting: a page is written once it has been read in 30 different frames (a page that changes all the time - a
        // movie frame, something drawn into video memory - never gets that far)
        if self.dump_dir.is_some() && !self.dumped.contains(&key) {
            let stamp = self.zstamp as u32;
            if self.repl_seen.len() > 200_000 { self.repl_seen.clear(); }
            let e = self.repl_seen.entry(key).or_insert((u32::MAX, 0));
            if e.0 != stamp { e.0 = stamp; e.1 += 1; }
            if e.1 >= 30 { self.dump_page(key, clut); }
        }
        if !self.repl_on || !self.repl_files.contains_key(&key) { return 0; }
        self.replacement_slot(key)
    }

    /// Takes the cached page keys that a change of video memory in this rectangle could have changed.
    fn repl_invalidate(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        if self.repl_cache.is_empty() { return; }
        self.repl_cache.retain(|e| {
            let (tx, ty, d, _c) = e.0;
            let per = match d { 0 => 64, 1 => 128, _ => 256 };
            let hit_tex = x1 >= tx && x0 <= tx + per - 1 && y1 >= ty && y0 <= ty + 255;
            // the palette is one row of 16 or 256 halfwords: any change of that row at all counts
            let cy = (e.0 .3 >> 6) & 0x1FF;
            let hit_clut = d < 2 && y0 <= cy && y1 >= cy;
            !(hit_tex || hit_clut)
        });
    }

    fn dump_page(&mut self, key: u64, clut: i32) {
        let dir = match &self.dump_dir { Some(d) => d.clone(), None => return };
        self.dumped.insert(key);
        let depth = self.tp_depth & 3;
        let mut rgba = vec![0u8; 256 * 256 * 4];
        let cx = (clut & 0x3F) * 16;
        let cy = (((clut >> 6) & 0x1FF) & 511) as usize;
        for v in 0..256usize {
            let y = ((self.tp_y as usize) + v) & 511;
            for u in 0..256usize {
                let texel: u16 = match depth {
                    0 => {
                        let wd = self.vram[y * 1024 + ((self.tp_x as usize + (u >> 2)) & 1023)];
                        let idx = ((wd >> ((u & 3) * 4)) & 15) as usize;
                        self.vram[cy * 1024 + ((cx as usize + idx) & 1023)]
                    }
                    1 => {
                        let wd = self.vram[y * 1024 + ((self.tp_x as usize + (u >> 1)) & 1023)];
                        let idx = ((wd >> ((u & 1) * 8)) & 255) as usize;
                        self.vram[cy * 1024 + ((cx as usize + idx) & 1023)]
                    }
                    _ => self.vram[y * 1024 + ((self.tp_x as usize + u) & 1023)],
                };
                let o = (v * 256 + u) * 4;
                if texel == 0 { continue; }
                let c5 = |s: u16| { let q = ((texel >> s) & 31) as u32; (q * 8 + q / 4) as u8 };
                rgba[o] = c5(0);
                rgba[o + 1] = c5(5);
                rgba[o + 2] = c5(10);
                rgba[o + 3] = 255;
            }
        }
        let name = dir.join(format!("{:016x}.png", key));
        let _ = std::fs::write(name, crate::png::encode_rgba(256, 256, &rgba));
    }

    /// True when the geometry coprocessor projected a vertex at this screen position in the last frames (so the primitive is part
    /// of the 3D scene, which it has already squeezed, and not of the interface).
    fn gte_made(&self, x: i32, y: i32) -> bool {
        if self.zt.is_null() { return true; }
        let key = ((((y - self.off_y) & 1023) as usize) << 10) | (((x - self.off_x) & 1023) as usize);
        let e = unsafe { *self.zt.add(key) };
        (self.zstamp.wrapping_sub(e >> 16)) & 0xFFFF <= 2
    }

    /// The wide copy of the buffer being drawn into: its number (made when it is new), or none when the scene is not a wide
    /// one or the buffer is not the console's 320 pixels wide.
    fn wide_slot(&mut self) -> Option<usize> {
        if !self.wide_scene || self.acc.is_none() { return None; }
        if self.area_x1 != 0 || self.area_x2 != 319 || self.area_y2 - self.area_y1 >= WIDE_STEP { return None; }
        match self.wide_buf.iter().position(|b| b.0 == self.area_y1) {
            Some(i) => Some(i),
            None if self.wide_buf.len() < 2 => { self.wide_buf.push((self.area_y1, false)); Some(self.wide_buf.len() - 1) }
            None => None,
        }
    }

    /// The scissor of a wide copy.
    fn wide_scissor(&self, slot: usize) -> [u32; 4] {
        let s = self.scale as u32;
        [0, (WIDE_TOP as u32 + slot as u32 * WIDE_STEP as u32) * s, WIDE_W as u32 * s, ((self.area_y2 - self.area_y1 + 1) as u32).min(WIDE_STEP as u32) * s]
    }

    /// Where a primitive of the console's buffer goes in the wide copy: x' = mul * (x - left) + add, y' = row of the copy + y - top.
    fn wide_xf(&self, world: bool, xmin: f32, xmax: f32) -> (f32, f32) {
        // the scene was squeezed to three quarters of its width about the middle by the geometry coprocessor: put back
        if world { return (4.0 / 3.0, WIDE_W as f32 / 2.0 - 160.0 * 4.0 / 3.0); }
        // what spans the whole screen (a fade, a backdrop) is stretched across it; the rest of the interface keeps its size and
        // stays together in the middle, where the console's own screen would have been
        if xmax - xmin >= 316.0 { return (WIDE_W as f32 / 320.0, 0.0); }
        (1.0, WIDE_E as f32)
    }

    /// True when a rectangle (a sprite) sits where the geometry coprocessor projected something: it is part of the scene.
    fn rect_is_world(&self, x: i32, y: i32, w: i32, h: i32) -> bool {
        let pts = [(x, y), (x + w / 2, y + h / 2), (x + w / 2, y), (x + w / 2, y + h), (x, y + h / 2), (x + w, y + h / 2)];
        pts.iter().any(|&(px, py)| self.gte_made(px, py))
    }

    /// state words common to the primitives that read texels
    fn acc_state(&mut self, clut: i32, raw: bool, bb: [i32; 4]) -> ([u32; 4], [u32; 2]) {
        let layer = self.page_layer(clut);
        let win = self.win_mask_x != 0 || self.win_mask_y != 0;
        let clut_x = ((clut & 0x3F) * 16) as u32;
        let clut_y = ((clut >> 6) & 0x1FF) as u32;
        let mut flags = F_TEX;
        if raw { flags |= F_RAW; }
        if win { flags |= F_WIN; }
        flags |= ((self.win_mask_x & 255) as u32) << 8 | ((self.win_mask_y & 255) as u32) << 16 | ((self.win_off_x & 255) as u32) << 24;
        let st = [
            (self.tp_x as u32 & 0xFFFF) | ((self.tp_y as u32) << 16),
            (self.tp_depth as u32 & 3) | (clut_x << 2) | (clut_y << 14),
            flags,
            ((self.win_off_y & 255) as u32) | (layer << 8),
        ];
        (st, pack_bb(bb))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn acc_tri(&mut self, x: [i32; 3], y: [i32; 3], c: [u32; 3], u: [i32; 3], v: [i32; 3], tex: bool, raw: bool, semi: bool, clut: i32) {
        if self.acc.is_none() { return; }
        // wide_pass: 0 the console's picture only, 1 that and the wide copy, 2 the wide copy only (the area clipped it away)
        let pass = self.wide_pass;
        let wide = if pass != 0 { self.wide_slot() } else { None };
        if pass == 2 && wide.is_none() { return; }
        let (fx, fy) = self.vertex_fracs(x, y);
        let depths = if tex { self.vertex_depths(x, y) } else { None };
        let bb = [u[0].min(u[1]).min(u[2]), u[0].max(u[1]).max(u[2]), v[0].min(v[1]).min(v[2]), v[0].max(v[1]).max(v[2])];
        let (st, st2) = if tex { self.acc_state(clut, raw, bb) } else { ([0; 4], [0; 2]) };
        let area = [self.area_x1, self.area_y1, self.area_x2, self.area_y2];
        let mode = self.tp_mode as usize;
        let per = match self.tp_depth { 0 => 64, 1 => 128, _ => 256 };
        let tex_rect = [self.tp_x, self.tp_y, self.tp_x + per - 1, (self.tp_y + 255).min(511)];
        let clut_rect = if self.tp_depth < 2 {
            let cx = (clut & 0x3F) * 16;
            [cx, (clut >> 6) & 0x1FF, cx + if self.tp_depth == 0 { 15 } else { 255 }, (clut >> 6) & 0x1FF]
        } else { [-1, -1, -2, -2] };
        let (xmin, xmax) = (x[0].min(x[1]).min(x[2]) as f32, x[0].max(x[1]).max(x[2]) as f32);
        // at least two of the three corners were projected by the geometry coprocessor (one alone can be a coincidence)
        let scene = (self.wide_scene || self.hud_squeeze) && (0..3).filter(|&i| self.gte_made(x[i], y[i])).count() >= 2;
        let world = wide.is_some() && scene;
        let hud = self.hud_squeeze && !scene;
        let hc = (self.area_x1 + self.area_x2 + 1) as f32 / 2.0;
        let (wmul, wadd) = self.wide_xf(world, xmin, xmax);
        let wsc = wide.map(|k| self.wide_scissor(k));
        let wrow = wide.map(|k| (WIDE_TOP + k as i32 * WIDE_STEP) as f32 - self.area_y1 as f32).unwrap_or(0.0);
        let ax = self.area_x1 as f32;
        let a = self.acc.as_mut().unwrap();
        if tex {
            a.sync_rect(&self.vram, tex_rect);
            if clut_rect[0] >= 0 { a.sync_rect(&self.vram, clut_rect); }
        }
        let sc = a.area_scissor(area);
        let mk = |i: usize| -> Vtx {
            let pz = depths.map(|d| d[i]).unwrap_or(1.0);
            Vtx {
                pos: [x[i] as f32 + fx[i] as f32 / 16.0 + 0.5, y[i] as f32 + fy[i] as f32 / 16.0 + 0.5],
                w: (1.0 / pz) as f32,
                col: [(c[i] & 0xFF) as f32, ((c[i] >> 8) & 0xFF) as f32, ((c[i] >> 16) & 0xFF) as f32],
                uv: [u[i] as f32, v[i] as f32],
                st,
                st2,
            }
        };
        let t = [mk(0), mk(1), mk(2)];
        if pass != 2 {
            let mut tn = t;
            if hud { for vt in tn.iter_mut() { vt.pos[0] = hc + (vt.pos[0] - hc) * 0.75; } }
            emit_tri(a, sc, tn, tex, semi, mode);
        }
        if let Some(ws) = wsc {
            let mut tw = t;
            for vt in tw.iter_mut() {
                vt.pos[0] = wmul * (vt.pos[0] - ax) + wadd;
                vt.pos[1] += wrow;
            }
            emit_tri(a, ws, tw, tex, semi, mode);
        }
        if pass != 2 {
            let (min_x, max_x) = (x[0].min(x[1]).min(x[2]).max(area[0]), x[0].max(x[1]).max(x[2]).min(area[2]));
            let (min_y, max_y) = (y[0].min(y[1]).min(y[2]).max(area[1]), y[0].max(y[1]).max(y[2]).min(area[3]));
            a.mark_dirty(min_x, min_y, max_x, max_y);
            self.repl_invalidate(min_x, min_y, max_x, max_y);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn acc_rect(&mut self, x: i32, y: i32, w: i32, h: i32, j0: i32, j1: i32, i0: i32, i1: i32, colour: u32, tex: bool, raw: bool, semi: bool,
                           u0: i32, v0: i32, clut: i32) {
        if self.acc.is_none() { return; }
        let pass = self.wide_pass;
        let wide = if pass != 0 { self.wide_slot() } else { None };
        if pass == 2 && wide.is_none() { return; }
        let bb = [u0, u0 + w - 1, v0, v0 + h - 1];
        let (st, st2) = if tex { self.acc_state(clut, raw, bb) } else { ([0; 4], [0; 2]) };
        let mode = self.tp_mode as usize;
        let per = match self.tp_depth { 0 => 64, 1 => 128, _ => 256 };
        let tex_rect = [self.tp_x, self.tp_y, self.tp_x + per - 1, (self.tp_y + 255).min(511)];
        let clut_rect = if self.tp_depth < 2 {
            let cx = (clut & 0x3F) * 16;
            [cx, (clut >> 6) & 0x1FF, cx + if self.tp_depth == 0 { 15 } else { 255 }, (clut >> 6) & 0x1FF]
        } else { [-1, -1, -2, -2] };
        let scene = (self.wide_scene || self.hud_squeeze) && w < 300 && self.rect_is_world(x, y, w, h);
        let world = wide.is_some() && scene;
        let hc = (self.area_x1 + self.area_x2 + 1) as f32 / 2.0;
        let sq = self.hud_squeeze;
        let (wmul, wadd) = self.wide_xf(world, x as f32, (x + w) as f32);
        let wsc = wide.map(|k| self.wide_scissor(k));
        let wrow = wide.map(|k| (WIDE_TOP + k as i32 * WIDE_STEP) as f32 - self.area_y1 as f32).unwrap_or(0.0);
        let ax = self.area_x1 as f32;
        let a = self.acc.as_mut().unwrap();
        if tex {
            a.sync_rect(&self.vram, tex_rect);
            if clut_rect[0] >= 0 { a.sync_rect(&self.vram, clut_rect); }
        }
        let sc = a.full_scissor();
        let col = [(colour & 0xFF) as f32, ((colour >> 8) & 0xFF) as f32, ((colour >> 16) & 0xFF) as f32];
        if pass != 2 {
            // the pixels the clipped drawing covers: columns i0..=i1 and rows j0..=j1 of the rectangle
            let (x0, y0) = ((x + i0) as f32, (y + j0) as f32);
            let (x1, y1) = ((x + i1 + 1) as f32, (y + j1 + 1) as f32);
            let (x0, x1) = if sq { (hc + (x0 - hc) * 0.75, hc + (x1 - hc) * 0.75) } else { (x0, x1) };
            // texel positions at the edges (a texel's centre is at its whole number)
            let (ua, ub) = ((u0 + i0) as f32 - 0.5, (u0 + i1) as f32 + 0.5);
            let (va, vb) = ((v0 + j0) as f32 - 0.5, (v0 + j1) as f32 + 0.5);
            emit_quad(a, sc, [[x0, y0], [x1, y0], [x1, y1], [x0, y1]], col, [[ua, va], [ub, va], [ub, vb], [ua, vb]], st, st2, tex, semi, mode);
        }
        if let Some(ws) = wsc {
            // the whole rectangle, whatever the console's area cut off
            let (x0, y0) = (wmul * (x as f32 - ax) + wadd, y as f32 + wrow);
            let (x1, y1) = (wmul * ((x + w) as f32 - ax) + wadd, (y + h) as f32 + wrow);
            let (ua, ub) = (u0 as f32 - 0.5, (u0 + w - 1) as f32 + 0.5);
            let (va, vb) = (v0 as f32 - 0.5, (v0 + h - 1) as f32 + 0.5);
            emit_quad(a, ws, [[x0, y0], [x1, y0], [x1, y1], [x0, y1]], col, [[ua, va], [ub, va], [ub, vb], [ua, vb]], st, st2, tex, semi, mode);
        }
        if pass != 2 {
            a.mark_dirty(x + i0, y + j0, x + i1, y + j1);
            self.repl_invalidate(x + i0, y + j0, x + i1, y + j1);
        }
    }

    /// A fill of video memory seen by the wide copy of the buffer it touches. A fill that covers the buffer is a clear: from then
    /// on the wide copy holds only what is drawn into it.
    fn wide_fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: [f32; 3]) {
        let slot = match self.wide_slot() { Some(k) => k, None => return };
        if y > self.area_y2 || y + h - 1 < self.area_y1 || x > self.area_x2 || x + w - 1 < self.area_x1 { return; }
        let rows = self.area_y2 - self.area_y1 + 1;
        let (mul, add) = self.wide_xf(false, x as f32, (x + w) as f32);
        let ax = self.area_x1 as f32;
        let wrow = (WIDE_TOP + slot as i32 * WIDE_STEP) as f32 - self.area_y1 as f32;
        let ws = self.wide_scissor(slot);
        let (x0, x1) = (mul * (x as f32 - ax) + add, mul * ((x + w) as f32 - ax) + add);
        let (y0, y1) = (y as f32 + wrow, (y + h) as f32 + wrow);
        if w >= 300 && h >= rows - 32 { self.wide_buf[slot].1 = true; }
        if let Some(a) = self.acc.as_mut() {
            a.quad(0, ws, [[x0, y0], [x1, y0], [x1, y1], [x0, y1]], c, [[0.0; 2]; 4], [0; 4], [0; 2]);
        }
    }

    /// Something other than drawing (an image loaded, a rectangle moved) changed a buffer: its wide copy is not true any more.
    pub(super) fn wide_touch(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if x >= 320 || x + w <= 0 { return; }
        for b in self.wide_buf.iter_mut() {
            if y < b.0 + WIDE_STEP && y + h > b.0 { b.1 = false; }
        }
    }

    /// The display from the card's picture.
    pub(super) fn acc_read_display(&mut self, out: &mut Vec<u32>) {
        let (mut w, h) = (self.disp_w, self.disp_h);
        let s = self.scale as usize;
        // a wide scene whose buffer has a complete wide copy is shown from it
        self.wide_shown = false;
        let mut wide_row = -1;
        if !self.wide_scene { self.wide_buf.clear(); }
        else if self.disp_x == 0 && self.disp_w == 320 && h <= WIDE_STEP {
            if let Some(k) = self.wide_buf.iter().position(|b| b.0 == self.disp_y && b.1) {
                wide_row = WIDE_TOP + k as i32 * WIDE_STEP;
                w = WIDE_W;
                self.wide_shown = true;
            }
        }
        let (ow, oh) = (w as usize * s, h as usize * s);
        let scale = if h > 240 { 2 } else { 1 };
        let first = (self.line_top - 16) * scale;
        let last = (self.line_bottom - 16) * scale;
        out.clear();
        out.resize(ow * oh, 0);
        // the rows the picture shows come from video memory at disp_y; the horizontal range may wrap, so it is read in pieces
        let rows = (last - first).clamp(0, h);
        if rows == 0 { return; }
        let y0 = if wide_row >= 0 { wide_row } else { (self.disp_y & 511) as i32 };
        let a = match self.acc.as_mut() { Some(a) => a, None => return };
        let mut tmp: Vec<u32> = Vec::new();
        // column pieces
        let x0 = if wide_row >= 0 { 0 } else { self.disp_x & 1023 };
        let w1 = w.min(1024 - x0);
        let pieces = [(x0, 0, w1), (0, w1, w - w1)];
        for &(px, ox, pw) in &pieces {
            if pw <= 0 { continue; }
            // row pieces (video memory has 512 rows)
            let h1 = if wide_row >= 0 { rows } else { rows.min(512 - y0) };
            for &(py, oy, ph) in &[(y0, 0, h1), (0, h1, rows - h1)] {
                if ph <= 0 { continue; }
                a.read(px, py, pw, ph, &mut tmp);
                let pws = pw as usize * s;
                for j in 0..ph as usize * s {
                    let dst = ((first.max(0) as usize + oy as usize) * s + j) * ow + ox as usize * s;
                    out[dst..dst + pws].copy_from_slice(&tmp[j * pws..(j + 1) * pws]);
                }
            }
        }
        if self.aa {
            let mut scratch = Vec::new();
            crate::post::antialias(out, ow, oh, &mut scratch);
        }
    }
}
