// The window, the keyboard and the sound output: the program runs at the console's 60 vertical blanks a second and its
// display is the picture, kept at 4:3.
//   F1 texture filter    F2 picture size (1 to 4 times)    F3 edge smoothing
//   arrows  directions      X / Space  Cross     Z  Circle    C  Square    V  Triangle
//   A / S   L1 / R1         Q / W      L2 / R2   Return  Start    Tab  Select    Escape  quit

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use gilrs::{Axis, Button, Gilrs};
use minifb::{Key, KeyRepeat, ScaleMode, Window, WindowOptions};
use crate::font;
use crate::present::{Between, Opts, Present};

use crate::machine::{Machine, SOUND_RATE};
use crate::pad;

/// Full screen without a mode change: the window loses its frame and covers its monitor; the same call puts it back.
#[cfg(windows)]
mod fullscreen {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect { l: i32, t: i32, r: i32, b: i32 }
    #[repr(C)]
    struct MonitorInfo { size: u32, monitor: Rect, work: Rect, flags: u32 }
    extern "system" {
        fn GetWindowRect(h: *mut c_void, r: *mut Rect) -> i32;
        fn GetWindowLongPtrW(h: *mut c_void, i: i32) -> isize;
        fn SetWindowLongPtrW(h: *mut c_void, i: i32, v: isize) -> isize;
        fn SetWindowPos(h: *mut c_void, after: *mut c_void, x: i32, y: i32, w: i32, h2: i32, flags: u32) -> i32;
        fn MonitorFromWindow(h: *mut c_void, flags: u32) -> *mut c_void;
        fn GetMonitorInfoW(m: *mut c_void, info: *mut MonitorInfo) -> i32;
    }
    const GWL_STYLE: i32 = -16;
    const WS_OVERLAPPEDWINDOW: isize = 0x00CF0000;
    const WS_POPUP: isize = 0x80000000u32 as i32 as isize;

    pub struct Saved { rect: Rect, style: isize }

    /// Into full screen (returns what is needed to come back), or back out of it.
    pub fn toggle(hwnd: *mut c_void, saved: &mut Option<Saved>) {
        unsafe {
            match saved.take() {
                None => {
                    let mut r = Rect::default();
                    GetWindowRect(hwnd, &mut r);
                    let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
                    let mon = MonitorFromWindow(hwnd, 2);
                    let mut mi = MonitorInfo { size: std::mem::size_of::<MonitorInfo>() as u32, monitor: Rect::default(), work: Rect::default(), flags: 0 };
                    if GetMonitorInfoW(mon, &mut mi) == 0 { return; }
                    SetWindowLongPtrW(hwnd, GWL_STYLE, (style & !WS_OVERLAPPEDWINDOW) | WS_POPUP);
                    let m = mi.monitor;
                    SetWindowPos(hwnd, std::ptr::null_mut(), m.l, m.t, m.r - m.l, m.b - m.t, 0x0020 | 0x0200 | 0x0004);
                    *saved = Some(Saved { rect: r, style });
                }
                Some(sv) => {
                    SetWindowLongPtrW(hwnd, GWL_STYLE, sv.style);
                    SetWindowPos(hwnd, std::ptr::null_mut(), sv.rect.l, sv.rect.t, sv.rect.r - sv.rect.l, sv.rect.b - sv.rect.t, 0x0020 | 0x0200 | 0x0004);
                }
            }
        }
    }
}

/// The buttons the keyboard can be bound to: the name in controls.cfg and the pad bit.
const PAD_BUTTONS: [(&str, i32); 14] = [
    ("Up", pad::UP), ("Down", pad::DOWN), ("Left", pad::LEFT), ("Right", pad::RIGHT),
    ("Cross", pad::CROSS), ("Circle", pad::CIRCLE), ("Square", pad::SQUARE), ("Triangle", pad::TRIANGLE),
    ("L1", pad::L1), ("R1", pad::R1), ("L2", pad::L2), ("R2", pad::R2), ("Start", pad::START), ("Select", pad::SELECT),
];

macro_rules! key_names {
    ($($k:ident),* $(,)?) => { &[$((Key::$k, stringify!($k))),*] };
}

/// The keys that can be bound, by the names controls.cfg uses.
fn key_table() -> &'static [(Key, &'static str)] {
    key_names![A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
        Key0, Key1, Key2, Key3, Key4, Key5, Key6, Key7, Key8, Key9, Up, Down, Left, Right, Enter, Space, Tab, Backspace,
        LeftShift, RightShift, LeftCtrl, RightCtrl, LeftAlt, RightAlt, Comma, Period, Slash, Semicolon, Apostrophe,
        LeftBracket, RightBracket, Minus, Equal, Home, End, PageUp, PageDown, Insert, Delete,
        NumPad0, NumPad1, NumPad2, NumPad3, NumPad4, NumPad5, NumPad6, NumPad7, NumPad8, NumPad9, NumPadEnter]
}

fn default_keymap() -> Vec<(Key, i32)> {
    vec![
        (Key::Up, pad::UP), (Key::Down, pad::DOWN), (Key::Left, pad::LEFT), (Key::Right, pad::RIGHT),
        (Key::X, pad::CROSS), (Key::Z, pad::CIRCLE), (Key::C, pad::SQUARE), (Key::V, pad::TRIANGLE),
        (Key::A, pad::L1), (Key::S, pad::R1), (Key::Q, pad::L2), (Key::W, pad::R2),
        (Key::Enter, pad::START), (Key::Tab, pad::SELECT), (Key::Space, pad::CROSS),
    ]
}

static KEYMAP: Mutex<Vec<(Key, i32)>> = Mutex::new(Vec::new());

/// controls.cfg: one line a button, `cross=X,Space`; a button that is not listed keeps its usual keys.
fn load_keymap(path: Option<&std::path::Path>) -> Vec<(Key, i32)> {
    let mut map = default_keymap();
    let text = match path.and_then(|p| std::fs::read_to_string(p).ok()) { Some(t) => t, None => return map };
    for line in text.lines() {
        let (name, keys) = match line.split_once('=') { Some(x) => x, None => continue };
        let bit = match PAD_BUTTONS.iter().find(|b| b.0.eq_ignore_ascii_case(name.trim())) { Some(b) => b.1, None => continue };
        map.retain(|e| e.1 != bit);
        for k in keys.split(',') {
            if let Some(e) = key_table().iter().find(|e| e.1.eq_ignore_ascii_case(k.trim())) { map.push((e.0, bit)); }
        }
    }
    map
}

fn save_keymap(path: &std::path::Path, map: &[(Key, i32)]) {
    let mut out = String::from("# keyboard bindings: button=Key,Key (names as in the settings screen); delete the file for the defaults\n");
    for (name, bit) in PAD_BUTTONS.iter() {
        let keys: Vec<&str> = map.iter().filter(|e| e.1 == *bit).filter_map(|e| key_table().iter().find(|t| t.0 == e.0).map(|t| t.1)).collect();
        out += &format!("{}={}\n", name, keys.join(","));
    }
    let _ = std::fs::write(path, out);
}

fn keys_to_pad(w: &Window) -> i32 {
    // Alt held (Alt+Enter is full screen): no buttons
    if w.is_key_down(Key::LeftAlt) || w.is_key_down(Key::RightAlt) { return 0; }
    let mut p = 0;
    let map = KEYMAP.lock().unwrap();
    for (k, b) in map.iter() {
        if w.is_key_down(*k) { p |= *b; }
    }
    p
}

// A controller as the console's pad: South Cross, East Circle, West Square, North Triangle, the bumpers L1/R1, the
// triggers L2/R2, Select, Start, the direction pad or the left stick the directions.
fn controller_pad(g: &mut Gilrs) -> i32 {
    while g.next_event().is_some() {}
    let mut p = 0;
    for (_, pad_) in g.gamepads() {
        let pairs = [
            (Button::South, pad::CROSS), (Button::East, pad::CIRCLE), (Button::West, pad::SQUARE), (Button::North, pad::TRIANGLE),
            (Button::LeftTrigger, pad::L1), (Button::RightTrigger, pad::R1), (Button::LeftTrigger2, pad::L2), (Button::RightTrigger2, pad::R2),
            (Button::Select, pad::SELECT), (Button::Start, pad::START),
            (Button::DPadUp, pad::UP), (Button::DPadDown, pad::DOWN), (Button::DPadLeft, pad::LEFT), (Button::DPadRight, pad::RIGHT),
        ];
        for (b, bit) in pairs {
            if pad_.is_pressed(b) { p |= bit; }
        }
        let x = pad_.value(Axis::LeftStickX);
        let y = pad_.value(Axis::LeftStickY);
        if x < -0.5 { p |= pad::LEFT; }
        if x > 0.5 { p |= pad::RIGHT; }
        if y > 0.5 { p |= pad::UP; }
        if y < -0.5 { p |= pad::DOWN; }
    }
    p
}

static UNDERRUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct Audio {
    queue: Arc<Mutex<VecDeque<i16>>>,
    _stream: Option<cpal::Stream>,
}

const SINC_TAPS: usize = 32;
const SINC_PHASES: usize = 256;

/// The weights of a Kaiser-windowed sinc, SINC_PHASES + 1 rows of SINC_TAPS: row r is the output position r / SINC_PHASES
/// between the 16th and the 17th of the taps. `cutoff` is the share of the source's own bandwidth that is kept (below 1 when
/// the device's rate is lower than the game's).
fn sinc_table(cutoff: f64) -> Vec<f32> {
    let bessel_i0 = |x: f64| -> f64 {
        let mut sum = 1.0;
        let mut term = 1.0;
        let q = x * x / 4.0;
        for k in 1..40 {
            term *= q / (k as f64 * k as f64);
            sum += term;
            if term < 1e-14 * sum { break; }
        }
        sum
    };
    let beta = 8.6;
    let norm = bessel_i0(beta);
    let half = (SINC_TAPS / 2) as f64;
    let mut t = vec![0f32; (SINC_PHASES + 1) * SINC_TAPS];
    for r in 0..=SINC_PHASES {
        let frac = r as f64 / SINC_PHASES as f64;
        let mut row = [0f64; SINC_TAPS];
        let mut sum = 0.0;
        for k in 0..SINC_TAPS {
            let d = k as f64 - (half - 1.0) - frac;
            let x = std::f64::consts::PI * cutoff * d;
            let sinc = if x.abs() < 1e-12 { 1.0 } else { x.sin() / x };
            let win_arg = (d / half).clamp(-1.0, 1.0);
            let win = bessel_i0(beta * (1.0 - win_arg * win_arg).max(0.0).sqrt()) / norm;
            row[k] = cutoff * sinc * win;
            sum += row[k];
        }
        for k in 0..SINC_TAPS { t[r * SINC_TAPS + k] = (row[k] / sum) as f32; }
    }
    t
}

fn build_stream<T>(device: &cpal::Device, config: &cpal::StreamConfig, queue: Arc<Mutex<VecDeque<i16>>>) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels as usize;
    let step = SOUND_RATE as f64 / config.sample_rate.0 as f64;
    // the last four sound frames; the output lies between hist[1] and hist[2] and is a cubic through all four
    let mut hist = [[0f32; 2]; 4];
    let mut phase = 1.0f64;
    let table = sinc_table(if step > 1.0 { 0.97 / step } else { 0.995 });
    let mut hist_s = [[0f32; 2]; SINC_TAPS];
    // playback starts (and restarts after a gap) only once a little sound is waiting, and a gap fades out instead of cutting
    let prime = SOUND_RATE / 25 * 2;
    let target = (SOUND_RATE / 12 * 2) as f64;
    let mut primed = false;
    device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let mut q = queue.lock().unwrap();
            if !primed && q.len() >= prime { primed = true; }
            // when the program runs a little behind, the sound plays up to 3% slower (and a little faster when it runs ahead)
            // instead of running dry
            let rate = (0.97 + 0.03 * (q.len() as f64 / target).min(2.0)).clamp(0.97, 1.03);
            for frame in data.chunks_mut(channels) {
                while phase >= 1.0 {
                    phase -= 1.0;
                    let last = hist[3];
                    hist.rotate_left(1);
                    hist_s.rotate_left(1);
                    hist[3] = if primed {
                        match (q.pop_front(), q.pop_front()) {
                            (Some(l), Some(r)) => [l as f32 / 32768.0, r as f32 / 32768.0],
                            _ => {
                                primed = false;
                                UNDERRUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                [last[0] * 0.8, last[1] * 0.8]
                            }
                        }
                    } else {
                        [last[0] * 0.8, last[1] * 0.8]
                    };
                    hist_s[SINC_TAPS - 1] = hist[3];
                }
                let t = phase as f32;
                let t2 = t * t;
                let t3 = t2 * t;
                let w = [-0.5 * t3 + t2 - 0.5 * t, 1.5 * t3 - 2.5 * t2 + 1.0, -1.5 * t3 + 2.0 * t2 + 0.5 * t, 0.5 * t3 - 0.5 * t2];
                let (l, r) = if SINC.load(std::sync::atomic::Ordering::Relaxed) {
                    let pos = phase as f32 * SINC_PHASES as f32;
                    let row = (pos as usize).min(SINC_PHASES - 1);
                    let f = pos - row as f32;
                    let (w0, w1) = (&table[row * SINC_TAPS..(row + 1) * SINC_TAPS], &table[(row + 1) * SINC_TAPS..(row + 2) * SINC_TAPS]);
                    let (mut l, mut r) = (0f32, 0f32);
                    for k in 0..SINC_TAPS {
                        let wk = w0[k] + (w1[k] - w0[k]) * f;
                        l += wk * hist_s[k][0];
                        r += wk * hist_s[k][1];
                    }
                    (l.clamp(-1.0, 1.0), r.clamp(-1.0, 1.0))
                } else {
                    ((0..4).map(|k| w[k] * hist[k][0]).sum::<f32>().clamp(-1.0, 1.0),
                     (0..4).map(|k| w[k] * hist[k][1]).sum::<f32>().clamp(-1.0, 1.0))
                };
                for (i, out) in frame.iter_mut().enumerate() {
                    let v = if channels == 1 { (l + r) * 0.5 } else if i == 0 { l } else if i == 1 { r } else { 0.0 };
                    *out = T::from_sample(v);
                }
                phase += step * rate;
            }
        },
        |e| eprintln!("audio: {}", e),
        None,
    )
}

fn open_audio() -> Audio {
    let queue: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));
    let host = cpal::default_host();
    let device = match host.default_output_device() {
        Some(d) => d,
        None => {
            eprintln!("audio output not available: no output device");
            return Audio { queue, _stream: None };
        }
    };
    // the device's own format and rate; the game's 44100 Hz stereo is resampled to it
    let supported = match device.default_output_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("audio output not available: {}", e);
            return Audio { queue, _stream: None };
        }
    };
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let q = queue.clone();
    let stream = match format {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, q),
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, q),
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, q),
        cpal::SampleFormat::I32 => build_stream::<i32>(&device, &config, q),
        other => {
            eprintln!("audio output not available: sample format {:?}", other);
            return Audio { queue, _stream: None };
        }
    };
    match stream {
        Ok(st) => {
            let _ = st.play();
            Audio { queue, _stream: Some(st) }
        }
        Err(e) => {
            eprintln!("audio output not available: {}", e);
            Audio { queue, _stream: None }
        }
    }
}

/// FSR's sharpening, in stops (0 is the sharpest): the values F5 steps through
const SHARP: [f32; 4] = [1.0, 0.5, 0.2, 0.0];
const SHARP_NAMES: [&str; 4] = ["soft", "medium", "firm", "maximum"];
const SCALE_NAMES: [&str; 5] = ["fit", "whole-number", "stretch", "16:9 fill", "16:9 3D"];
/// scanlines, shadow mask
const CRT: [(f32, f32, &str); 4] = [(0.0, 0.0, "off"), (0.35, 0.0, "scanlines"), (0.5, 0.35, "scanlines+mask"), (0.7, 0.5, "strong")];
/// brightness, contrast, saturation, gamma
const COLOUR: [(f32, f32, f32, f32, &str); 4] = [
    (0.0, 1.0, 1.0, 1.0, "neutral"),
    (0.0, 1.08, 1.25, 1.0, "vivid"),
    (0.0, 1.04, 1.05, 0.9, "television"),
    (0.02, 0.94, 0.9, 1.0, "soft"),
];
const FRAME_NAMES: [&str; 3] = ["60", "120 blended", "120 motion"];

/// the sound's resampler: windowed sinc (true) or the cubic one (false); F10 changes it while the sound plays
static SINC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// everything F4 to F10 change; picture.cfg keeps it
#[derive(Clone, Copy)]
struct Look {
    fsr: bool,
    sharp: usize,
    scale: u32,
    crt: usize,
    colour: usize,
    frames: usize,
    sinc: bool,
    pixel: bool,
}

impl Look {
    fn opts(&self, lines: f32, linear: bool) -> Opts {
        let c = CRT[self.crt.min(3)];
        let k = COLOUR[self.colour.min(3)];
        Opts { fsr: self.fsr, sharp: SHARP[self.sharp.min(3)], scale_mode: self.scale, wide_scene: false, scan: c.0, mask: c.1, bright: k.0, contrast: k.1,
               sat: k.2, gamma: k.3, lines, linear, pixel_art: self.pixel }
    }
    fn text(&self) -> String {
        format!("fsr={}\nsharp={}\nscale={}\ncrt={}\ncolour={}\nframes={}\nsinc={}\npixel={}\n", self.fsr as i32, self.sharp, self.scale, self.crt, self.colour, self.frames, self.sinc as i32, self.pixel as i32)
    }
}

fn title(m: &Machine, l: &Look) -> String {
    let f = ["nearest", "balanced", "smooth", "enhanced"][m.gpu.filter.clamp(0, 3) as usize];
    format!("Xenogears - x{} ({}, edges {}) | F4 FSR {} | F5 sharp {} | F6 {} | F7 CRT {} | F8 colour {} | F9 {} | F10 sound {} | F11 pixel-art {} | F12 settings | F1 F2 F3",
        m.gpu.scale, f, if m.gpu.aa { "smoothed" } else { "plain" }, if l.fsr { "on" } else { "off" }, SHARP_NAMES[l.sharp.min(3)],
        SCALE_NAMES[l.scale.min(4) as usize], CRT[l.crt.min(3)].2, COLOUR[l.colour.min(3)].4, FRAME_NAMES[l.frames.min(2)],
        if l.sinc { "sinc" } else { "cubic" }, if l.pixel { "on" } else { "off" })
}

// ---- the settings screen (opened from the title's fourth entry, or F12) ----
const ITEMS: usize = 16;

fn item_text(i: usize, m: &Machine, l: &Look) -> (&'static str, String) {
    let onoff = |b: bool| if b { "On".to_string() } else { "Off".to_string() };
    match i {
        0 => ("Picture size", if m.gpu.size_locked { format!("x{}", m.gpu.scale) } else { "Auto".to_string() }),
        1 => ("Texture filter", ["Sharp", "Mixed", "Smooth", "Enhanced"][m.gpu.filter.clamp(0, 3) as usize].to_string()),
        2 => ("Edge smoothing", onoff(m.gpu.aa)),
        3 => ("Pixel-art scaler", onoff(l.pixel)),
        4 => ("Enhanced textures", if m.gpu.repl_files.is_empty() { if m.gpu.scale > 1 { "No folder".to_string() } else { "Needs size x2+".to_string() } } else { onoff(m.gpu.repl_on) }),
        5 => ("Collect textures", onoff(m.gpu.dump_dir.is_some())),
        6 => ("Upscaler (FSR 1.0)", onoff(l.fsr)),
        7 => ("Sharpening", SHARP_NAMES[l.sharp.min(3)].to_string()),
        8 => ("Placement", SCALE_NAMES[l.scale.min(4) as usize].to_string()),
        9 => ("CRT look", CRT[l.crt.min(3)].2.to_string()),
        10 => ("Colour", COLOUR[l.colour.min(3)].4.to_string()),
        11 => ("Frame rate", FRAME_NAMES[l.frames.min(2)].to_string()),
        12 => ("Sound resampler", if l.sinc { "Sinc".to_string() } else { "Cubic".to_string() }),
        13 => ("Keyboard controls", "Change".to_string()),
        14 => ("Reset to defaults", String::new()),
        _ => ("Back", String::new()),
    }
}

/// Changes item `i` by `dir` (1 or -1). True when the screen should close.
fn item_change(i: usize, dir: i32, m: &mut Machine, l: &mut Look) -> bool {
    let step = |v: usize, n: usize| -> usize { ((v as i32 + dir).rem_euclid(n as i32)) as usize };
    match i {
        0 => {
            // Auto, x1, x2, x3, x4
            let cur = if m.gpu.size_locked { m.gpu.scale as usize } else { 0 };
            let next = step(cur, 5);
            if next == 0 { m.gpu.size_locked = false; } else { m.gpu.set_scale(next as i32); m.gpu.size_locked = true; }
        }
        1 => { let f = step(m.gpu.filter.clamp(0, 3) as usize, 4) as i32; m.gpu.set_filter(f); }
        2 => m.gpu.aa = !m.gpu.aa,
        3 => l.pixel = !l.pixel,
        4 => m.gpu.repl_on = !m.gpu.repl_on,
        5 => { let on = m.gpu.dump_dir.is_none(); m.gpu.set_dump(on); }
        6 => l.fsr = !l.fsr,
        7 => l.sharp = step(l.sharp, 4),
        8 => l.scale = step(l.scale as usize, 5) as u32,
        9 => l.crt = step(l.crt, 4),
        10 => l.colour = step(l.colour, 4),
        11 => l.frames = step(l.frames, 3),
        12 => { l.sinc = !l.sinc; SINC.store(l.sinc, std::sync::atomic::Ordering::Relaxed); }
        14 => {
            *l = Look { fsr: true, sharp: 2, scale: 0, crt: 0, colour: 0, frames: 0, sinc: true, pixel: false };
            SINC.store(true, std::sync::atomic::Ordering::Relaxed);
            m.gpu.set_filter(1);
            m.gpu.aa = true;
            m.gpu.size_locked = false;
        }
        13 => return false,
        _ => return true,
    }
    m.gpu.settings_changed = true;
    false
}

fn draw_settings(buf: &mut [u32], w: usize, h: usize, row: usize, m: &Machine, l: &Look, prompt: Option<&str>) {
    if buf.len() != w * h || w < 64 || h < 48 { return; }
    // the picture behind goes dark
    for p in buf.iter_mut() {
        let c = *p;
        *p = ((c >> 16 & 0xFF) * 2 / 5) << 16 | ((c >> 8 & 0xFF) * 2 / 5) << 8 | ((c & 0xFF) * 2 / 5);
    }
    // what is shown: the title, the items, and the controls (three short lines, each one fits on its own)
    let prompt_lines: Vec<String> = prompt.map(|p| vec![p.to_string(), "Esc: stop".to_string(), String::new()]).unwrap_or_default();
    let hints: Vec<&str> = if prompt.is_some() { prompt_lines.iter().map(|x| x.as_str()).collect() } else { vec!["Up/Down: choose", "Left/Right: change", "Circle or Esc: back"] };
    let items: Vec<(&'static str, String)> = (0..ITEMS).map(|i| item_text(i, m, l)).collect();
    // the longest row, label and "< value >" with a gap, in characters
    let mut widest = 8usize;
    for (label, value) in &items {
        let v = if value.is_empty() { 0 } else { value.chars().count() + 4 };
        widest = widest.max(label.chars().count() + 3 + v);
    }
    for hnt in &hints { widest = widest.max(hnt.chars().count()); }
    let lines_total = ITEMS + 2 /* title and its gap */ + hints.len() + 2 /* gaps */;
    // the biggest whole scale at which the panel fits the picture across and down (with a margin of a character)
    let mut sc = (h / 200).max(1);
    while sc > 1 && ((widest + 4) * 6 * sc > w || (lines_total * 9 + 8) * sc > h) { sc -= 1; }
    // the line height: a character and a little air, tighter when the picture is only the console's own size
    let line = (12 * sc).min(h.saturating_sub(10 * sc) / lines_total).max(9 * sc);
    let pw = ((widest + 4) * 6 * sc).min(w.saturating_sub(2 * sc));
    let ph = (lines_total * line + 8 * sc).min(h.saturating_sub(2 * sc));
    let (x0, y0) = ((w - pw) / 2, (h - ph) / 2);
    let fill = |buf: &mut [u32], x: usize, y: usize, rw: usize, rh: usize, col: u32| {
        for yy in y..(y + rh).min(h) { for xx in x..(x + rw).min(w) { buf[yy * w + xx] = col; } }
    };
    fill(buf, x0, y0, pw, ph, 0x101830);
    fill(buf, x0, y0, pw, sc, 0xC8A850);
    fill(buf, x0, y0 + ph - sc, pw, sc, 0xC8A850);
    fill(buf, x0, y0, sc, ph, 0xC8A850);
    fill(buf, x0 + pw - sc, y0, sc, ph, 0xC8A850);
    let title = "SETTINGS";
    font::draw(buf, w, h, x0 + (pw - font::width(title, sc)) / 2, y0 + 4 * sc, sc, title, 0xFFE8A0, Some(0x000000));
    let mut y = y0 + 4 * sc + 2 * line;
    for (i, (label, value)) in items.iter().enumerate() {
        if i == row { fill(buf, x0 + 3 * sc, y - 2 * sc, pw - 6 * sc, line, 0x2C4C90); }
        let col = if i == row { 0xFFFFFF } else { 0xC0C8D8 };
        font::draw(buf, w, h, x0 + 8 * sc, y, sc, label, col, Some(0x000000));
        if !value.is_empty() {
            let text = if i == row { format!("< {} >", value) } else { value.clone() };
            let vw = font::width(&text, sc);
            font::draw(buf, w, h, x0 + pw - 8 * sc - vw, y, sc, &text, if i == row { 0xFFE070 } else { 0xA0D8FF }, Some(0x000000));
        }
        y += line;
    }
    y += line / 2;
    for hnt in &hints {
        font::draw(buf, w, h, x0 + (pw.saturating_sub(font::width(hnt, sc))) / 2, y, sc, hnt, 0x90A0C0, Some(0x000000));
        y += line;
    }
}

// ---- freeze report: a thread that notices the main loop has stopped and writes down where ----
static HEART: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PHASE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static SNAPSHOT: Mutex<String> = Mutex::new(String::new());

fn phase_name(p: u32) -> &'static str {
    match p {
        1 => "reading the keys and the settings screen",
        2 => "running the game's program for a frame",
        3 => "taking the sound",
        4 => "fetching the finished picture",
        5 => "presenting the picture (the graphics card)",
        6 => "frame doubling",
        _ => "idle",
    }
}

fn start_freeze_watch(report: std::path::PathBuf, start: Instant) {
    let _ = std::thread::Builder::new().name("freeze-watch".into()).spawn(move || {
        let mut reported_at: Option<u64> = None;
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let now = start.elapsed().as_millis() as u64;
            let beat = HEART.load(std::sync::atomic::Ordering::Relaxed);
            if now.saturating_sub(beat) > 8000 && reported_at.map(|r| now.saturating_sub(r) > 30000).unwrap_or(true) {
                reported_at = Some(now);
                let (last, calls, polls) = Machine::debug_state();
                let text = format!(
                    "The game stopped answering {:.1} s ago (run time {:.0} s).\nWhere: {}\nThe program's last call into the machine: {:08X} ({} calls, {} interrupt polls so far)\nSettings: {}\n",
                    (now - beat) as f64 / 1000.0, now as f64 / 1000.0, phase_name(PHASE.load(std::sync::atomic::Ordering::Relaxed)),
                    last, calls, polls, SNAPSHOT.lock().map(|s| s.clone()).unwrap_or_default());
                eprintln!("{}", text);
                let _ = std::fs::write(&report, text);
            }
        }
    });
}

/// The settings screen over a picture, for looking at it without a window (XG_SETTINGS=row in a headless run).
pub fn draw_settings_preview(buf: &mut [u32], w: usize, h: usize, row: usize, m: &Machine) {
    let look = Look { fsr: true, sharp: 2, scale: 0, crt: 0, colour: 0, frames: 0, sinc: true, pixel: false };
    draw_settings(buf, w, h, row, m, &look, None);
}

pub fn play(mut m: Machine, cfg_path: Option<std::path::PathBuf>, restart: Option<Box<dyn Fn() -> Option<Machine>>>) {
    let audio = open_audio();
    let mut gamepads = Gilrs::new().ok();
    // the picture's presentation: FSR, window placement, CRT look, colour, frame rate (F4 to F9), the sound's resampler (F10)
    let cfg_text = cfg_path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let cfg_get = |k: &str| -> Option<i32> {
        cfg_text.lines().find_map(|l| l.trim().strip_prefix(k).and_then(|r| r.strip_prefix('=')).and_then(|v| v.trim().parse().ok()))
    };
    let env_i = |k: &str| -> Option<i32> { std::env::var(k).ok().and_then(|v| v.parse().ok()) };
    let mut look = Look {
        fsr: match std::env::var("XG_FSR") { Ok(v) => v != "0", Err(_) => cfg_get("fsr").map(|v| v != 0).unwrap_or(true) },
        sharp: cfg_get("sharp").unwrap_or(2).clamp(0, 3) as usize,
        scale: env_i("XG_SCALE").or(cfg_get("scale")).unwrap_or(0).clamp(0, 4) as u32,
        crt: env_i("XG_CRT").or(cfg_get("crt")).unwrap_or(0).clamp(0, 3) as usize,
        colour: env_i("XG_COLOUR").or(cfg_get("colour")).unwrap_or(0).clamp(0, 3) as usize,
        frames: env_i("XG_FRAMES").or(cfg_get("frames")).unwrap_or(0).clamp(0, 2) as usize,
        sinc: env_i("XG_SINC").or(cfg_get("sinc")).map(|v| v != 0).unwrap_or(true),
        pixel: env_i("XG_PIXEL").or(cfg_get("pixel")).map(|v| v != 0).unwrap_or(false),
    };
    SINC.store(look.sinc, std::sync::atomic::Ordering::Relaxed);
    {
        let report = cfg_path.as_ref().and_then(|p| p.parent().map(|d| d.join("freeze-report.txt"))).unwrap_or_else(|| std::path::PathBuf::from("freeze-report.txt"));
        start_freeze_watch(report, Instant::now());
    }
    let mut present: Option<Present> = None;
    let mut present_failed = false;
    let mut pres_ms = 0.0f64;
    let mut up: Vec<u32> = Vec::new();
    let mut prev_pix: Vec<u32> = Vec::new();
    let mut prev_size = (0usize, 0usize);
    let mut frozen_once = false;
    let mut ended_at = Instant::now();
    let mut reset_tested = false;
    let mut settings_open = false;
    let mut settings_row = 0usize;
    #[cfg(windows)]
    let mut fs_saved: Option<fullscreen::Saved> = None;
    #[cfg(windows)]
    let mut fs_asked = false;
    // binding keys: the button being asked for (index into PAD_BUTTONS)
    let mut remap: Option<usize> = None;
    let mut remap_prompt: Option<String> = None;
    let keys_path = cfg_path.as_ref().and_then(|p| p.parent().map(|d| d.join("controls.cfg")));
    *KEYMAP.lock().unwrap() = load_keymap(keys_path.as_deref());
    let mut prev_raw_pad = 0i32;
    let mut window = match Window::new(
        &title(&m, &look),
        960,
        720,
        WindowOptions { resize: true, scale_mode: ScaleMode::AspectRatioStretch, ..WindowOptions::default() },
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("window: {}", e);
            return;
        }
    };
    window.set_target_fps(0);
    let mut pixels: Vec<u32> = Vec::new();
    let mut take = vec![0i16; SOUND_RATE * 2];
    let start = Instant::now();
    let mut due: f64 = 0.0; // frames owed
    let mut last = start;
    let mut ended = false;
    // the picture's size steps down by itself when the machine cannot hold 60 frames a second at it
    let mut ema_ms: f64 = 0.0;
    let mut slow_frames = 0;
    let mut auto_size = !m.gpu.size_locked;
    let mut preferred = m.gpu.scale;
    let mut fast_frames = 0u32;
    let mut up_wait = 600u32;
    let mut since_up: Option<u32> = None;
    // XG_RUNSECS: leave after that many seconds and say how the run went (for checking the pace and the sound)
    let run_secs: Option<f64> = std::env::var("XG_RUNSECS").ok().and_then(|v| v.parse().ok());
    let mut total_frames = 0u64;
    while window.is_open() {
        HEART.store(start.elapsed().as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
        PHASE.store(1, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut snap) = SNAPSHOT.lock() {
            *snap = format!("x{} filter {} edges {} | {}", m.gpu.scale, m.gpu.filter, m.gpu.aa as i32, look.text().replace('\n', " "));
        }
        // binding keys: the next key pressed goes to the button asked for
        if let Some(bi) = remap {
            let pressed = window.get_keys_pressed(KeyRepeat::No);
            if pressed.contains(&Key::Escape) {
                remap = None;
            } else if let Some(k) = pressed.iter().find(|k| key_table().iter().any(|t| t.0 == **k)) {
                let bit = PAD_BUTTONS[bi].1;
                {
                    let mut map = KEYMAP.lock().unwrap();
                    map.retain(|e| e.0 != *k && e.1 != bit);
                    map.push((*k, bit));
                }
                if bi + 1 < PAD_BUTTONS.len() {
                    remap = Some(bi + 1);
                } else {
                    remap = None;
                    if let Some(p) = &keys_path { save_keymap(p, &KEYMAP.lock().unwrap()); }
                }
            }
            remap_prompt = remap.map(|b| format!("Press a key for {}", PAD_BUTTONS[b].0.to_uppercase()));
            if remap.is_none() { if let Some(p) = &keys_path { save_keymap(p, &KEYMAP.lock().unwrap()); } }
            prev_raw_pad = keys_to_pad(&window);
            HEART.store(start.elapsed().as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(2));
            if remap.is_some() || !pressed.is_empty() { /* the settings screen stays up while binding */ }
        }
        if window.is_key_pressed(Key::Escape, KeyRepeat::No) {
            if settings_open { settings_open = false; } else { break; }
        }
        if window.is_key_pressed(Key::F12, KeyRepeat::No) { settings_open = !settings_open; settings_row = 0; }
        // XG_FULLSCREEN=1: start in full screen
        #[cfg(windows)]
        if total_frames >= 3 && !fs_asked && std::env::var("XG_FULLSCREEN").map(|v| v == "1").unwrap_or(false) {
            fs_asked = true;
            fullscreen::toggle(window.get_window_handle() as *mut std::ffi::c_void, &mut fs_saved);
        }
        // Alt+Enter: full screen and back
        #[cfg(windows)]
        if (window.is_key_down(Key::LeftAlt) || window.is_key_down(Key::RightAlt)) && window.is_key_pressed(Key::Enter, KeyRepeat::No) {
            fullscreen::toggle(window.get_window_handle() as *mut std::ffi::c_void, &mut fs_saved);
        }
        if m.gpu.settings_request { m.gpu.settings_request = false; settings_open = true; settings_row = 0; }
        let raw_pad = keys_to_pad(&window) | gamepads.as_mut().map(controller_pad).unwrap_or(0);
        let pressed_pad = raw_pad & !prev_raw_pad;
        prev_raw_pad = raw_pad;
        if !settings_open && pressed_pad & pad::SELECT != 0 && (m.gpu.title_showing() || m.mode == 5) { settings_open = true; settings_row = 0; }
        if settings_open {
            if remap.is_none() && pressed_pad & pad::UP != 0 { settings_row = (settings_row + ITEMS - 1) % ITEMS; }
            if remap.is_none() && pressed_pad & pad::DOWN != 0 { settings_row = (settings_row + 1) % ITEMS; }
            let dir = if pressed_pad & pad::LEFT != 0 { -1 } else if pressed_pad & (pad::RIGHT | pad::CROSS) != 0 { 1 } else { 0 };
            if remap.is_some() {
                // binding: the pad does nothing here
            } else if settings_row == 13 {
                if dir != 0 { remap = Some(0); remap_prompt = Some(format!("Press a key for {}", PAD_BUTTONS[0].0.to_uppercase())); }
            } else if dir != 0 && item_change(settings_row, dir, &mut m, &mut look) { settings_open = false; }
            if remap.is_none() && pressed_pad & pad::CIRCLE != 0 { settings_open = false; }
            auto_size = !m.gpu.size_locked;
        }
        if let Some(r) = run_secs { if start.elapsed().as_secs_f64() > r { break; } }
        // XG_FREEZE_TEST=1: stalls the loop for twelve seconds after three (to see the freeze report written)
        if std::env::var("XG_FREEZE_TEST").is_ok() && !frozen_once && start.elapsed().as_secs() >= 3 { frozen_once = true; PHASE.store(2, std::sync::atomic::Ordering::Relaxed); std::thread::sleep(Duration::from_secs(12)); }
        if window.is_key_pressed(Key::F1, KeyRepeat::No) {
            let f = (m.gpu.filter + 1) % 4;
            m.gpu.set_filter(f);
            m.gpu.settings_changed = true;
            window.set_title(&title(&m, &look));
        }
        if window.is_key_pressed(Key::F4, KeyRepeat::No) { look.fsr = !look.fsr; m.gpu.settings_changed = true; }
        if window.is_key_pressed(Key::F5, KeyRepeat::No) { look.sharp = (look.sharp + 1) % 4; m.gpu.settings_changed = true; }
        if window.is_key_pressed(Key::F6, KeyRepeat::No) { look.scale = (look.scale + 1) % 5; m.gpu.settings_changed = true; }
        if window.is_key_pressed(Key::F7, KeyRepeat::No) { look.crt = (look.crt + 1) % 4; m.gpu.settings_changed = true; }
        if window.is_key_pressed(Key::F8, KeyRepeat::No) { look.colour = (look.colour + 1) % 4; m.gpu.settings_changed = true; }
        if window.is_key_pressed(Key::F9, KeyRepeat::No) { look.frames = (look.frames + 1) % 3; m.gpu.settings_changed = true; }
        if window.is_key_pressed(Key::F11, KeyRepeat::No) { look.pixel = !look.pixel; m.gpu.settings_changed = true; }
        if window.is_key_pressed(Key::F10, KeyRepeat::No) {
            look.sinc = !look.sinc;
            SINC.store(look.sinc, std::sync::atomic::Ordering::Relaxed);
            m.gpu.settings_changed = true;
        }
        if window.is_key_pressed(Key::F3, KeyRepeat::No) {
            m.gpu.aa = !m.gpu.aa;
            m.gpu.settings_changed = true;
            window.set_title(&title(&m, &look));
        }
        if window.is_key_pressed(Key::F2, KeyRepeat::No) {
            let s = m.gpu.scale % 4 + 1;
            m.gpu.set_scale(s);
            auto_size = false;
            m.gpu.size_locked = true;
            m.gpu.settings_changed = true;
            window.set_title(&title(&m, &look));
        }
        if m.gpu.settings_changed {
            m.gpu.settings_changed = false;
            window.set_title(&title(&m, &look));
            if let Some(p) = &cfg_path {
                let text = format!("res={}\nfilter={}\naa={}\nlocked={}\n{}", m.gpu.scale, m.gpu.filter, m.gpu.aa as i32, m.gpu.size_locked as i32, look.text());
                let _ = std::fs::write(p, text);
            }
            preferred = m.gpu.scale;
        }
        // XG_RESET_TEST=1: acts as if the program had ended, five seconds in (to see the restart work)
        if std::env::var("XG_RESET_TEST").is_ok() && !reset_tested && start.elapsed().as_secs() >= 5 { reset_tested = true; ended = true; ended_at = Instant::now(); eprintln!("reset test: the program is taken as ended"); }
        // the console reset: a program that ended starts again from the beginning after a moment, with the picture settings kept
        if ended && ended_at.elapsed() > Duration::from_millis(1500) {
            if let Some(r) = &restart {
                if let Some(nm) = r() {
                    let (sc, f, aa, locked) = (m.gpu.scale, m.gpu.filter, m.gpu.aa, m.gpu.size_locked);
                    // the old machine's program thread is parked inside it and still points at it: it is left where it is
                    let old = std::mem::replace(&mut m, nm);
                    std::mem::forget(old);
                    m.gpu.set_filter(f);
                    m.gpu.aa = aa;
                    m.gpu.size_locked = locked;
                    m.gpu.set_scale(sc);
                    ended = false;
                    due = 0.0;
                    prev_pix.clear();
                    last = Instant::now();
                    window.set_title(&title(&m, &look));
                }
            }
        }
        let now = Instant::now();
        due += now.duration_since(last).as_secs_f64() * 60.0;
        last = now;
        if due > 4.0 { due = 4.0; }
        let mut did = 0;
        let work = Instant::now();
        while due >= 1.0 && !ended {
            PHASE.store(2, std::sync::atomic::Ordering::Relaxed);
            m.pad = if settings_open { 0 } else { raw_pad };
            if !m.run_frame() {
                ended = true;
                ended_at = Instant::now();
                // the program ended: the console's soft reset (Start + Select held), a program that returned, or an error
                let why = match &m.error { Some(e) => format!("stopped: {}", e), None => "the program ended (a soft reset, or it returned)".to_string() };
                eprintln!("the game's program {}", why);
                if let Some(p) = &cfg_path {
                    if let Some(d) = p.parent() {
                        use std::io::Write;
                        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(d.join("game-log.txt")) {
                            let _ = writeln!(f, "{:.0} s into the run, frame {}: the game's program {}", start.elapsed().as_secs_f64(), m.frames, why);
                            let _ = writeln!(f, "  mode {}, running {}, last calls into code that is not decompiled:", m.mode, m.running);
                            for l in m.call_ring().iter().rev().take(24) { let _ = writeln!(f, "    {}", l); }
                        }
                        // the whole machine as it was, so the end can be reproduced (load it with PROG_LOAD)
                        if m.error.is_some() || true {
                            let _ = std::fs::write(d.join("crash.state"), m.save_state());
                        }
                    }
                }
            }
            due -= 1.0;
            did += 1;
            total_frames += 1;
        }
        let mut fresh: Option<(usize, usize)> = None;
        if did > 0 {
            PHASE.store(3, std::sync::atomic::Ordering::Relaxed);
            let got = m.take_sound(&mut take);
            let mut q = audio.queue.lock().unwrap();
            for i in 0..got * 2 { q.push_back(take[i]); }
            // more than a fifth of a second waiting: the oldest goes, so the sound keeps up with the picture
            let keep = SOUND_RATE / 5 * 2;
            while q.len() > keep { q.pop_front(); }
            drop(q);
            PHASE.store(4, std::sync::atomic::Ordering::Relaxed);
            if m.gpu.threaded {
                // the worker threads make the picture; take the newest one that is finished
                m.gpu.end_frame();
                fresh = m.gpu.poll_frame(&mut pixels);
            } else {
                m.gpu.read_display_hi(&mut pixels);
                fresh = Some(m.display_size());
            }
            if let Some((fw, fh)) = fresh { m.apply_movie(&mut pixels, fw, fh); }
            let per = work.elapsed().as_secs_f64() * 1000.0 / did as f64;
            ema_ms = if ema_ms == 0.0 { per } else { ema_ms * 0.9 + per * 0.1 };
            // the workers more than a few frames behind means they cannot keep up at this size
            if m.gpu.backlog() >= 5 { slow_frames += 4; }
            if ema_ms > 16.2 { slow_frames += 1; } else if m.gpu.backlog() < 3 { slow_frames = 0; }
            if m.gpu.size_locked { auto_size = false; }
            if let Some(n) = since_up.as_mut() { *n += did as u32; if *n > 900 { since_up = None; } }
            // headroom for a long while after a step down: go back up toward the size that was asked for
            if auto_size && m.gpu.scale < preferred && ema_ms > 0.0 && ema_ms < 8.0 { fast_frames += did as u32; } else { fast_frames = 0; }
            if auto_size && fast_frames > up_wait {
                let s = m.gpu.scale + 1;
                m.gpu.set_scale(s);
                window.set_title(&title(&m, &look));
                fast_frames = 0;
                ema_ms = 0.0;
                since_up = Some(0);
            }
            if auto_size && slow_frames > 30 && (m.gpu.scale > 1 || m.gpu.aa) {
                // a step up that fails straight away makes the next one wait longer
                if since_up.is_some() { up_wait = (up_wait * 2).min(7200); since_up = None; }
                // size first (edges stay smoothed), then the edge smoothing, then the last size step
                if m.gpu.scale > 2 || (m.gpu.scale == 2 && !m.gpu.aa) {
                    let s = m.gpu.scale - 1;
                    m.gpu.set_scale(s);
                } else if m.gpu.aa {
                    m.gpu.aa = false;
                } else {
                    m.gpu.set_scale(1);
                }
                window.set_title(&title(&m, &look));
                slow_frames = 0;
                ema_ms = 0.0;
            }
        }
        match fresh {
            Some((w, h)) if pixels.len() == w * h => {
                if settings_open { draw_settings(&mut pixels, w, h, settings_row, &m, &look, remap_prompt.as_deref()); }
                PHASE.store(5, std::sync::atomic::Ordering::Relaxed);
                let (ww, wh) = window.get_size();
                let lines = if m.gpu.scale > 0 { (h as f32 / m.gpu.scale as f32).round().max(1.0) } else { h as f32 };
                m.wide_opt = look.scale == 4;
                let mut opts = look.opts(lines, m.gpu.filter != 0);
                opts.wide_scene = m.wide_scene();
                // the picture goes through the card when something is asked of it, or the window is not its size (FSR alone does
                // nothing at the picture's own size, so then it is shown as it is)
                let wants_card = look.pixel || look.scale != 0 || look.crt != 0 || look.colour != 0 || look.frames != 0 || (ww, wh) != (w, h);
                let mut shown = false;
                if wants_card && !present_failed && ww > 0 && wh > 0 {
                    if present.is_none() {
                        present = Present::new();
                        if present.is_none() { present_failed = true; } else { eprintln!("presentation on the graphics card: {}x{} picture, {}x{} window", w, h, ww, wh); }
                    }
                    if let Some(pr) = present.as_mut() {
                        let t = Instant::now();
                        // the picture between the last one and this one goes first, the new one half a frame later
                        if look.frames != 0 && prev_size == (w, h) && prev_pix.len() == w * h {
                            let how = if look.frames == 2 { Between::Motion } else { Between::Blend };
                            if pr.frame(&pixels, w, h, Some(&prev_pix), Some(how), ww, wh, &opts, &mut up) {
                                let _ = window.update_with_buffer(&up, ww, wh);
                                let spent = t.elapsed();
                                if spent < Duration::from_micros(8000) { std::thread::sleep(Duration::from_micros(8000) - spent); }
                            }
                        }
                        let t = Instant::now();
                        if pr.frame(&pixels, w, h, None, None, ww, wh, &opts, &mut up) {
                            shown = true;
                            let ms = t.elapsed().as_secs_f64() * 1000.0;
                            pres_ms = if pres_ms == 0.0 { ms } else { pres_ms * 0.95 + ms * 0.05 };
                            let _ = window.update_with_buffer(&up, ww, wh);
                        } else {
                            present_failed = true;
                        }
                    }
                }
                if !shown { let _ = window.update_with_buffer(&pixels, w, h); }
                if look.frames != 0 {
                    prev_pix.clear();
                    prev_pix.extend_from_slice(&pixels);
                    prev_size = (w, h);
                }
                // a presentation that costs more than half a frame is more harm than good: it goes back to the plain picture
                if shown && pres_ms > 9.0 && total_frames > 120 {
                    eprintln!("the presentation takes {:.1} ms a frame here: FSR, frame doubling and the CRT look are off (F4 to F9 turn them on again)", pres_ms);
                    look.fsr = false;
                    look.frames = 0;
                    look.crt = 0;
                    pres_ms = 0.0;
                    m.gpu.settings_changed = true;
                }
            }
            _ => window.update(),
        }
        PHASE.store(0, std::sync::atomic::Ordering::Relaxed);
        if did == 0 { std::thread::sleep(Duration::from_millis(2)); }
    }
    if run_secs.is_some() {
        let secs = start.elapsed().as_secs_f64();
        eprintln!("ran {} frames in {:.1} s ({:.1} a second), size {}, sound gaps {}", total_frames, secs, total_frames as f64 / secs,
            m.gpu.scale, UNDERRUNS.load(std::sync::atomic::Ordering::Relaxed));
    }
    // the program thread is parked inside the machine: it is left as it is (dropping the machine under it could fault at exit)
    std::mem::forget(m);
}
