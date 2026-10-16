// Xenogears: the whole game, run from the game's own program on a software model of the console it ran on.
//
//   xenogears [--data <GameData folder>] [--res 1..4] [--filter 0..2] [--aa 0|1]   play (res 3, filter 1, aa 1)
//   xenogears --extract <disc 1 .bin> [<disc 2 .bin>] [--out <folder>]   take the game's data off your own discs
//   xenogears --headless <frames> <every> <prefix> [schedule] run without a window and write the display every N frames
//
// The pad schedule (headless) is a list of `frame:button[:hold]` entries (button: start, cross, circle, square, triangle,
// up, down, left, right, l1, r1, l2, r2, select), or `mash:button:N` to press the button for 3 frames every N frames.

#![windows_subsystem = "windows"]

mod archive;
mod card;
mod gpu;
pub use xgblocks::gte;
mod machine;
mod mdec;
mod movie;
mod mem;
mod spu;
mod xa;
pub use xgblocks::snap;
mod host;
mod present;
mod font;
mod png;
mod post;
mod extract;

use std::path::{Path, PathBuf};

use archive::Archive;
use machine::Machine;

pub mod pad {
    pub const SELECT: i32 = 0x0001;
    pub const START: i32 = 0x0008;
    pub const UP: i32 = 0x0010;
    pub const RIGHT: i32 = 0x0020;
    pub const DOWN: i32 = 0x0040;
    pub const LEFT: i32 = 0x0080;
    pub const L2: i32 = 0x0100;
    pub const R2: i32 = 0x0200;
    pub const L1: i32 = 0x0400;
    pub const R1: i32 = 0x0800;
    pub const TRIANGLE: i32 = 0x1000;
    pub const CIRCLE: i32 = 0x2000;
    pub const CROSS: i32 = 0x4000;
    pub const SQUARE: i32 = 0x8000;
}

fn find_data(arg: Option<String>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(a) = arg { candidates.push(PathBuf::from(a)); }
    if let Ok(e) = std::env::var("XENOGEARS_DATA") { candidates.push(PathBuf::from(e)); }
    if let Ok(exe) = std::env::current_exe() {
        let mut d = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..4 {
            if let Some(dir) = d {
                candidates.push(dir.join("GameData"));
                candidates.push(dir.join("project").join("GameData"));
                d = dir.parent().map(|p| p.to_path_buf());
            } else { break; }
        }
    }
    candidates.push(PathBuf::from("GameData"));
    candidates.push(PathBuf::from("C:/Users/exyle/Xenogears/GameData"));
    candidates.push(PathBuf::from("I:/1. Claude Code/Xenogears/project/GameData"));
    candidates.into_iter().find(|c| c.join("archive_table.bin").exists())
}

fn executable_name(archive: &Archive) -> String {
    if let Some(cnf) = archive.store.read(&Path::new("system").join("SYSTEM.CNF")).map(|b| String::from_utf8_lossy(&b).to_string()) {
        for line in cnf.lines() {
            let l = line.trim();
            if l.to_ascii_uppercase().starts_with("BOOT") {
                if let Some(at) = l.rfind('\\') {
                    let name = &l[at + 1..];
                    return name.split(';').next().unwrap_or(name).trim().to_string();
                }
            }
        }
    }
    "SLUS_006.64".to_string()
}

fn card_paths(data: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let card1 = match std::env::var("XENOGEARS_CARD") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            let abs = std::fs::canonicalize(data).unwrap_or_else(|_| data.to_path_buf());
            let beside = abs.parent().unwrap_or(&abs).join("Saves");
            // a game installed where the user cannot write (Program Files) keeps its saves in the user's own folder
            let writable = std::fs::create_dir_all(&beside).is_ok() && {
                let probe = beside.join(".write-test");
                let ok = std::fs::write(&probe, b"x").is_ok();
                let _ = std::fs::remove_file(&probe);
                ok
            };
            let dir = if writable { beside } else {
                match std::env::var("APPDATA").or_else(|_| std::env::var("HOME")) {
                    Ok(h) => PathBuf::from(h).join("Xenogears").join("Saves"),
                    Err(_) => beside,
                }
            };
            dir.join("card1.mcd")
        }
    };
    let card2 = match std::env::var("XENOGEARS_CARD2") {
        Ok(p) => PathBuf::from(p),
        Err(_) => card1.parent().map(|d| d.join("card2.mcd")).unwrap_or_else(|| PathBuf::from("card2.mcd")),
    };
    (Some(card1), Some(card2))
}

pub fn start(data: &Path) -> Result<Machine, String> {
    let archive = Archive::open(data)?;
    let exe_rel = Path::new("system").join(executable_name(&archive));
    let exe = archive.store.read(&exe_rel).ok_or_else(|| format!("{}: not found", exe_rel.display()))?;
    let (c1, c2) = card_paths(data);
    Machine::new(archive, &exe, c1, c2)
}

fn button(n: &str) -> i32 {
    match n {
        "start" => pad::START,
        "circle" => pad::CIRCLE,
        "square" => pad::SQUARE,
        "triangle" => pad::TRIANGLE,
        "select" => pad::SELECT,
        "l1" => pad::L1,
        "r1" => pad::R1,
        "l2" => pad::L2,
        "r2" => pad::R2,
        "up" => pad::UP,
        "down" => pad::DOWN,
        "left" => pad::LEFT,
        "right" => pad::RIGHT,
        _ => pad::CROSS,
    }
}

// The position of an entity's object in field units (16.16 fixed point at +0x20 and +0x28 of the object its record points to).
fn entity_xz(m: &mut Machine, n: u32) -> Option<(f64, f64)> {
    let base = m.peek(0x800AFB10, 4);
    if base == 0 { return None; }
    let obj = m.peek(base.wrapping_add(n * 0x5C + 0x4C), 4);
    if obj < 0x80000000 || obj >= 0x80200000 { return None; }
    let x = m.peek(obj + 0x20, 4) as i32 as f64 / 65536.0;
    let z = m.peek(obj + 0x28, 4) as i32 as f64 / 65536.0;
    Some((x, z))
}

fn object_of(m: &mut Machine, n: u32) -> Option<u32> {
    let base = m.peek(0x800AFB10, 4);
    if base == 0 { return None; }
    let obj = m.peek(base.wrapping_add(n * 0x5C + 0x4C), 4);
    if obj < 0x80000000 || obj >= 0x80200000 { None } else { Some(obj) }
}

fn teleport(m: &mut Machine, ent: u32, dx: f64, dz: f64) {
    let player = m.peek(0x800B226C, 4);
    let (po, eo) = match (object_of(m, player), object_of(m, ent)) { (Some(a), Some(b)) => (a, b), _ => { println!("teleport: no objects"); return; } };
    let x = m.peek(eo + 0x20, 4) as i32 as f64 + dx * 65536.0;
    let y = m.peek(eo + 0x24, 4);
    let z = m.peek(eo + 0x28, 4) as i32 as f64 + dz * 65536.0;
    m.poke(po + 0x20, x as i32 as u32, 4);
    m.poke(po + 0x24, y, 4);
    m.poke(po + 0x28, z as i32 as u32, 4);
    println!("teleported the player to entity {} ({:.1}, {:.1})", ent, x / 65536.0, z / 65536.0);
}

fn walk_pad(m: &mut Machine, ent: u32, frame: i64) -> i32 {
    let player = m.peek(0x800B226C, 4);
    let (px, pz) = match entity_xz(m, player) { Some(v) => v, None => return 0 };
    let (ex, ez) = match entity_xz(m, ent) { Some(v) => v, None => return 0 };
    let (dx, dz) = (ex - px, ez - pz);
    let up = (dz - dx) / 2.0;
    let right = (dx + dz) / 2.0;
    let mut p = 0;
    if frame % 60 == 0 { println!("frame {}: player ({:.1}, {:.1}) -> entity {} ({:.1}, {:.1})", frame, px, pz, ent, ex, ez); }
    if up.abs() > 1.2 { p |= if up > 0.0 { pad::UP } else { pad::DOWN }; }
    if right.abs() > 1.2 { p |= if right > 0.0 { pad::RIGHT } else { pad::LEFT }; }
    if frame % 20 < 2 { p |= pad::CROSS; }
    p
}

fn headless(m: &mut Machine, frames: i64, every: i64, prefix: &str, schedule: &str) {
    // XG_SCALE=4 in a headless run asks for the 16:9 3D picture as the window does
    if std::env::var("XG_SCALE").ok().as_deref() == Some("4") { m.wide_opt = true; }
    let entries: Vec<Vec<String>> = schedule
        .split(',')
        .filter(|e| !e.is_empty())
        .map(|e| e.split(':').map(|s| s.to_string()).collect())
        .collect();
    // probes, as the C# runner has them: PROG_JUMP frame:addr:arg, PROG_POKE frame:addr:value[/size],..., PROG_PEEK from:to:addr[/size],...
    let hex = |t: &str| u32::from_str_radix(t, 16).unwrap_or(0);
    let mut jumps: Vec<(i64, u32, u32)> = Vec::new();
    if let Ok(v) = std::env::var("PROG_JUMP") {
        for item in v.split(',') {
            let q: Vec<&str> = item.split(':').collect();
            if q.len() >= 3 { jumps.push((q[0].parse().unwrap_or(-1), hex(q[1]), hex(q[2]))); }
        }
    }
    let mut pokes: Vec<(i64, u32, u32, i32)> = Vec::new();
    if let Ok(v) = std::env::var("PROG_POKE") {
        for pk in v.split(',') {
            let q: Vec<&str> = pk.split(':').collect();
            if q.len() < 3 { continue; }
            let vs: Vec<&str> = q[2].split('/').collect();
            pokes.push((q[0].parse().unwrap_or(-1), hex(q[1]), hex(vs[0]), if vs.len() > 1 { vs[1].parse().unwrap_or(1) } else { 1 }));
        }
    }
    let peek: Option<(i64, i64, Vec<(u32, i32)>)> = std::env::var("PROG_PEEK").ok().and_then(|v| {
        let q: Vec<&str> = v.split(':').collect();
        if q.len() < 3 { return None; }
        let addrs = q[2].split(',').map(|a| {
            let s: Vec<&str> = a.split('/').collect();
            (hex(s[0]), if s.len() > 1 { s[1].parse().unwrap_or(4) } else { 4 })
        }).collect();
        Some((q[0].parse().unwrap_or(0), q[1].parse().unwrap_or(0), addrs))
    });
    // PROG_WALK frame:entity: from that frame the pad walks the player to the entity (greedy, for a camera that looks the way
    // the first room's does) and presses Cross once it is there
    let walk: Option<(i64, u32)> = std::env::var("PROG_WALK").ok().and_then(|v| {
        let q: Vec<&str> = v.split(':').collect();
        if q.len() < 2 { None } else { Some((q[0].parse().unwrap_or(0), q[1].parse().unwrap_or(0))) }
    });
    // PROG_LOAD file: start from a saved machine; PROG_SAVE frame:file: save the machine after that frame
    let mut frame: i64 = 0;
    let bench = std::env::var("XG_MOVIE_BENCH").is_ok();
    let bench_start = std::time::Instant::now();
    if let Ok(path) = std::env::var("PROG_LOAD") {
        match std::fs::read(&path) {
            Ok(d) if m.load_state(&d) => { frame = m.frames; println!("loaded {} at frame {}", path, frame); }
            _ => { eprintln!("not a saved state: {}", path); return; }
        }
    }
    let save: Option<(i64, String)> = std::env::var("PROG_SAVE").ok().and_then(|v| {
        let q: Vec<&str> = v.splitn(2, ':').collect();
        if q.len() < 2 { None } else { Some((q[0].parse().unwrap_or(-1), q[1].to_string())) }
    });
    // PROG_TELE frame:entity[:dx:dz]: at that frame the player's object is put where the entity's is (plus an offset), so the
    // game's own zone and talk checks see it there
    let tele: Option<(i64, u32, f64, f64)> = std::env::var("PROG_TELE").ok().and_then(|v| {
        let q: Vec<&str> = v.split(':').collect();
        if q.len() < 2 { None } else {
            Some((q[0].parse().unwrap_or(-1), q[1].parse().unwrap_or(0),
                q.get(2).and_then(|x| x.parse().ok()).unwrap_or(0.0), q.get(3).and_then(|x| x.parse().ok()).unwrap_or(0.0)))
        }
    });
    // PROG_RANDOM seed:from: from that frame the pad is a random player's: directions held for a while, Cross taps (talk, confirm),
    // now and then Circle, Triangle, Square, the shoulder buttons and Start
    let random: Option<(u64, i64)> = std::env::var("PROG_RANDOM").ok().and_then(|v| {
        let q: Vec<&str> = v.split(':').collect();
        if q.len() < 2 { None } else { Some((q[0].parse().unwrap_or(1), q[1].parse().unwrap_or(0))) }
    });
    let mut rng: u64 = random.map(|r| r.0.wrapping_mul(0x9E3779B97F4A7C15) | 1).unwrap_or(1);
    let mut held = 0i32;
    let mut held_for = 0i32;
    let mut last_map: i64 = -1;
    let mut last_mode = -1;
    let t0 = std::time::Instant::now();
    let mut shown = 0usize;
    let mut fsr_test: Option<present::Present> = None;
    let mut prev_test: Vec<u32> = Vec::new();
    let mut buf: Vec<u32> = Vec::new();
    let wav_path = std::env::var("XG_WAV").ok();
    let wav_from: i64 = std::env::var("XG_WAVFROM").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut wav: Vec<u8> = Vec::new();
    let mut take = vec![0i16; machine::SOUND_RATE * 2];
    // XG_DIGEST file[:every]: one line per frame with a hash of the registers and all of memory, to compare two runs
    let mut digest = std::env::var("XG_DIGEST").ok().map(|v| {
        let q: Vec<&str> = v.splitn(2, ':').collect();
        (std::io::BufWriter::new(std::fs::File::create(q[0]).expect("digest file")), q.get(1).and_then(|e| e.parse::<i64>().ok()).unwrap_or(1))
    });
    while frame < frames {
        let mut p = 0;
        for e in &entries {
            if e[0] == "mash" {
                let n: i64 = e[2].parse().unwrap_or(60);
                if frame % n < 3 { p |= button(&e[1]); }
                continue;
            }
            let at: i64 = e[0].parse().unwrap_or(0);
            let hold: i64 = if e.len() > 2 { e[2].parse().unwrap_or(3) } else { 3 };
            if frame >= at && frame < at + hold { p |= button(&e[1]); }
        }
        if let Some((from, ent)) = walk {
            if frame >= from { p |= walk_pad(m, ent, frame); }
        }
        if let Some((_, from)) = random {
            if frame >= from {
                let mut next = || { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; rng };
                if held_for <= 0 {
                    let r = next();
                    held_for = 10 + (r % 50) as i32;
                    let dirs = [pad::UP, pad::DOWN, pad::LEFT, pad::RIGHT, pad::UP | pad::RIGHT, pad::UP | pad::LEFT, pad::DOWN | pad::RIGHT,
                        pad::DOWN | pad::LEFT, 0];
                    held = dirs[((r >> 8) % dirs.len() as u64) as usize];
                }
                held_for -= 1;
                p |= held;
                let r = next();
                if r % 7 == 0 { p |= pad::CROSS; }
                if r % 97 == 0 { p |= pad::CIRCLE; }
                if r % 211 == 0 { p |= pad::TRIANGLE; }
                if r % 331 == 0 { p |= pad::SQUARE; }
                if r % 509 == 0 { p |= pad::L1; }
                if r % 521 == 0 { p |= pad::R1; }
                if r % 3001 == 0 { p |= pad::START; }
            }
        }
        m.pad = p;
        if let Some((at, ent, dx, dz)) = tele {
            if at == frame { teleport(m, ent, dx, dz); }
        }
        for j in &jumps {
            if j.0 == frame {
                m.jump(j.1, j.2);
                println!("frame {}: jumped to 0x{:X}", frame, j.1);
            }
        }
        for (f, a, v, size) in &pokes {
            if *f == frame {
                m.poke(*a, *v, *size);
                println!("frame {}: poked 0x{:08X} = 0x{:X}", frame, a, v);
            }
        }
        if !m.run_frame() { break; }
        if let Some((f, every)) = digest.as_mut() {
            if frame % *every == 0 {
                use std::io::Write;
                let _ = writeln!(f, "{} {:016x}", frame, m.digest());
            }
        }
        m.gpu.flush_batch();
        if let Some((from, to, addrs)) = &peek {
            if frame + 1 >= *from && frame + 1 <= *to {
                let mut line = format!("frame {}:", frame + 1);
                for (a, size) in addrs { line += &format!(" {:X}={:X}", a, m.peek(*a, *size)); }
                println!("{}", line);
            }
        }
        frame += 1;
        if bench {
            // real time: one frame every 1/60 s, and the movie frame wanted now is asked for and counted
            let due = bench_start + std::time::Duration::from_secs_f64(frame as f64 / 60.0);
            let now = std::time::Instant::now();
            if due > now { std::thread::sleep(due - now); }
            m.movie_probe();
        }
        if let Some((at, path)) = &save {
            if *at == frame {
                let _ = std::fs::write(path, m.save_state());
                println!("saved at frame {}", frame);
            }
        }
        if wav_path.is_some() {
            let got = m.take_sound(&mut take);
            if frame >= wav_from {
                for i in 0..got * 2 { wav.extend_from_slice(&take[i].to_le_bytes()); }
            }
        }
        if random.is_some() {
            let map = m.peek(0x8006F94E, 2) as i64;
            if map != last_map {
                println!("frame {}: map word 0x{:X}{}", frame, map, if m.mode != last_mode { format!(" (mode {})", m.mode) } else { String::new() });
                last_map = map;
                last_mode = m.mode;
            }
            if let Some(e) = &m.error {
                println!("frame {}: error: {}", frame, e);
                m.error = None;
            }
        }
        while shown < m.modes.len() {
            println!("{}", m.modes[shown]);
            shown += 1;
        }
        if every > 0 && frame % every == 0 {
            let (w, h) = if m.gpu.threaded {
                m.gpu.end_frame();
                m.gpu.wait_frame(&mut buf).unwrap_or(m.display_size())
            } else {
                m.gpu.read_display_hi(&mut buf);
                m.display_size()
            };
            m.apply_movie(&mut buf, w, h);
            let mut bytes = Vec::with_capacity(buf.len() * 4);
            for px in &buf {
                bytes.push((px >> 16) as u8);
                bytes.push((px >> 8) as u8);
                bytes.push(*px as u8);
                bytes.push(255);
            }
            let name = format!("{}.{:05}.{}x{}.rgba", prefix, frame, w, h);
            if let Err(e) = std::fs::write(&name, bytes) { eprintln!("{}: {}", name, e); }
            // XG_FSR_TEST=WxH: the same picture through the presentation (FSR, window placement, CRT look, colour; XG_SCALE,
            // XG_CRT, XG_COLOUR, XG_FSR as in the window), as a canvas of that size, for looking at what it does
            if let Ok(t) = std::env::var("XG_FSR_TEST") {
                let q: Vec<&str> = t.split('x').collect();
                if let (Some(dw), Some(dh)) = (q.get(0).and_then(|v| v.parse::<usize>().ok()), q.get(1).and_then(|v| v.parse::<usize>().ok())) {
                    if let Some(r) = std::env::var("XG_SETTINGS").ok().and_then(|v| v.parse::<usize>().ok()) { host::draw_settings_preview(&mut buf, w, h, r, m); }
                    if fsr_test.is_none() { fsr_test = present::Present::new(); }
                    let mut up = Vec::new();
                    let env_f = |k: &str, d: f32| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
                    let crt = env_f("XG_CRT", 0.0) as usize;
                    let scan = [0.0, 0.35, 0.5, 0.7][crt.min(3)];
                    let mask = [0.0, 0.0, 0.35, 0.5][crt.min(3)];
                    let col = env_f("XG_COLOUR", 0.0) as usize;
                    let (bright, contrast, sat, gamma) = [(0.0, 1.0, 1.0, 1.0), (0.0, 1.08, 1.25, 1.0), (0.0, 1.04, 1.05, 0.9), (0.02, 0.94, 0.9, 1.0)][col.min(3)];
                    let lines = if m.gpu.scale > 0 { (h as f32 / m.gpu.scale as f32).round() } else { h as f32 };
                    let opts = present::Opts { fsr: env_f("XG_FSR", 1.0) != 0.0, sharp: env_f("XG_FSR_SHARP", 0.2), scale_mode: env_f("XG_SCALE", 0.0) as u32, wide_scene: m.wide_scene(), pixel_art: env_f("XG_PIXEL", 0.0) != 0.0,
                                               scan, mask, bright, contrast, sat, gamma, lines, linear: true };
                    if let Some(f) = fsr_test.as_mut() {
                        // XG_INTERP=1 (blend) or 2 (motion): the picture halfway between the last frame seen here and this one
                        if let Some(k) = std::env::var("XG_INTERP").ok().and_then(|v| v.parse::<u32>().ok()) {
                            if prev_test.len() == buf.len() && k > 0 {
                                let how = if k == 2 { present::Between::Motion } else { present::Between::Blend };
                                let mut mid = Vec::new();
                                if f.frame(&buf, w, h, Some(&prev_test), Some(how), dw, dh, &opts, &mut mid) {
                                    let mut b3 = Vec::with_capacity(mid.len() * 4);
                                    for px in &mid { b3.extend_from_slice(&[(px >> 16) as u8, (px >> 8) as u8, *px as u8, 255]); }
                                    let _ = std::fs::write(format!("{}.{:05}.mid{}.{}x{}.rgba", prefix, frame, k, dw, dh), b3);
                                }
                            }
                            prev_test = buf.clone();
                        }
                        if f.frame(&buf, w, h, None, None, dw, dh, &opts, &mut up) {
                            let mut b2 = Vec::with_capacity(up.len() * 4);
                            for px in &up { b2.extend_from_slice(&[(px >> 16) as u8, (px >> 8) as u8, *px as u8, 255]); }
                            let _ = std::fs::write(format!("{}.{:05}.fsr.{}x{}.rgba", prefix, frame, dw, dh), b2);
                        }
                    }
                }
            }
        }
    }
    if let Some(path) = &wav_path {
        let rate = machine::SOUND_RATE as u32;
        let len = wav.len() as u32;
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 4).to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&wav);
        let _ = std::fs::write(path, out);
    }
    if let Ok(v) = std::env::var("PROG_DUMP") {
        let q: Vec<&str> = v.split(':').collect();
        if q.len() == 2 {
            let at = u32::from_str_radix(q[0], 16).unwrap_or(0x80000000);
            let n = usize::from_str_radix(q[1], 16).unwrap_or(0);
            let bytes: Vec<u8> = (0..n).map(|i| m.peek(at.wrapping_add(i as u32), 1) as u8).collect();
            let _ = std::fs::write(format!("{}.mem.{:X}.bin", prefix, at), bytes);
        }
    }
    if let Ok(p) = std::env::var("PROG_VRAM") {
        let mut b = Vec::with_capacity(m.gpu.vram.len() * 2);
        for v in &m.gpu.vram { b.extend_from_slice(&v.to_le_bytes()); }
        let _ = std::fs::write(p, b);
    }
    let ms = t0.elapsed().as_millis();
    println!("ran {} frames in {} ms, mode {}", frame, ms, m.mode);
    if bench { println!("movie frames at 60 Hz: not a movie {}, exact {}, earlier one {}, none {}", m.movie_stats[0], m.movie_stats[1], m.movie_stats[2], m.movie_stats[3]); }
    if std::env::var("XG_ENT_INFO").is_ok() {
        let used = m.mem.ents.iter().filter(|e| e.w.iter().any(|&v| v != 0)).count();
        println!("entity table: pointer {:08X}, {} records in use outside the image, overflow {}; {} objects handed out, overflow {}; script buffer {} bytes, overflow {}", m.peek(0x800AFB10, 4), used, m.mem.ent_over, m.mem.obj_next, m.mem.obj_over, m.mem.script_len, m.mem.script_over);
    }
    if let Ok(t) = std::env::var("XG_TRACE") { m.write_trace(&t); }
    if std::env::var("XG_NATIVE_INFO").is_ok() { println!("instructions run as translated blocks: {} of {} ({:.1}%)", m.native_steps, m.native_steps + m.total_steps, 100.0 * m.native_steps as f64 / (m.native_steps + m.total_steps).max(1) as f64); }
    if std::env::var("XG_NATIVE_INFO").is_ok() {
        println!("interpreted because: blocks off {}, interrupt pending {}, pc outside RAM {}, delay slot {}, no block here {}, block present but not run {}",
            m.native_why[0], m.native_why[1], m.native_why[2], m.native_why[3], m.native_why[4], m.native_why[5]);
        let mut pages: Vec<(u32, usize)> = m.native_hist.iter().enumerate().map(|(i, &c)| (c, i)).filter(|x| x.0 > 0).collect();
        pages.sort_by(|a, b| b.cmp(a));
        let mut by_pc: std::collections::HashMap<u32, (u32, u32, u32)> = std::collections::HashMap::new();
        for (&(pc, _port, w), &c) in &m.io_stat {
            let e = by_pc.entry(pc).or_insert((0, 0, 0));
            e.0 += c;
            if w == 1 { e.1 += 1; } else { e.2 += 1; }
        }
        let mut pcs: Vec<(u32, u32, u32, u32)> = by_pc.iter().map(|(&pc, &(c, w, r))| (c, pc, w, r)).collect();
        pcs.sort_by(|a, b| b.cmp(a));
        let total: u64 = pcs.iter().map(|x| x.0 as u64).sum();
        println!("device port accesses: {} from {} code addresses", total, pcs.len());
        for (c, pc, w, r) in pcs.iter().take(60) { println!("  io from 0x{:08X}: {} ({} write ports, {} read ports)", pc, c, w, r); }
        let mut words: Vec<(u32, u32)> = m.native_words_hist.iter().map(|(&a, &c)| (c, a)).collect();
        words.sort_by(|a, b| b.cmp(a));
        for (c, a) in words.iter().take(40) { println!("  word 0x{:08X}: {}", a, c); }
        for (c, i) in pages.iter().take(10) { println!("  interpreted at 0x{:08X}: {}", 0x80000000u32 + ((*i as u32) << 8), c); }
    }
    if let Some(e) = &m.error { println!("error: {}", e); }
    // leave without tearing the machine down: its worker threads are parked inside it, and dropping it under them could fault
    let _ = std::io::Write::flush(&mut std::io::stdout());
    std::process::exit(0);
}

#[cfg(windows)]
mod winui {
    extern "system" {
        pub fn AttachConsole(pid: u32) -> i32;
        pub fn MessageBoxW(h: *mut u8, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
}

/// The program is a window program (no console box beside the game); run from a command prompt it still writes there.
fn attach_console() {
    #[cfg(windows)]
    unsafe { winui::AttachConsole(0xFFFF_FFFF); }
}

/// An error the person has to see: on the console when there is one, and in a message box on Windows.
fn fatal(text: &str) -> ! {
    eprintln!("{}", text);
    #[cfg(windows)]
    unsafe {
        let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let c: Vec<u16> = "Xenogears".encode_utf16().chain(std::iter::once(0)).collect();
        winui::MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), 0x10);
    }
    std::process::exit(1);
}

fn main() {
    attach_console();
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|a| a.as_str()) == Some("--extract") {
        std::process::exit(extract::run(&args[2..]));
    }
    let mut data_arg: Option<String> = None;
    let mut i = 1;
    let mut headless_args: Option<Vec<String>> = None;
    let mut res: Option<i32> = None;
    let mut filter: Option<i32> = None;
    let mut aa: Option<bool> = None;
    while i < args.len() {
        match args[i].as_str() {
            "--res" if i + 1 < args.len() => { res = args[i + 1].parse().ok(); i += 2; }
            "--aa" if i + 1 < args.len() => { aa = Some(args[i + 1] != "0"); i += 2; }
            "--filter" if i + 1 < args.len() => { filter = args[i + 1].parse().ok(); i += 2; }
            "--data" if i + 1 < args.len() => { data_arg = Some(args[i + 1].clone()); i += 2; }
            "--headless" => { headless_args = Some(args[i + 1..].to_vec()); break; }
            _ => { i += 1; }
        }
    }
    let data = match find_data(data_arg) {
        Some(d) => d,
        None => {
            fatal("The game data was not found.

Put the GameData folder beside Xenogears.exe, or make it from your own discs with:
    Xenogears.exe --extract \"Disc 1.bin\" \"Disc 2.bin\"");
        }
    };
    // the enhanced textures: TextureReplace (what the player loads) and TextureDump (what it collects for tools/enhance_ai.py)
    // are looked for beside the game data, beside the program and in the folders above them; XG_TEX_REPLACE and XG_TEX_DUMP name
    // other folders
    {
        let mut bases: Vec<PathBuf> = Vec::new();
        if let Some(p) = data.parent() { bases.push(p.to_path_buf()); if let Some(pp) = p.parent() { bases.push(pp.to_path_buf()); } }
        if let Ok(exe) = std::env::current_exe() {
            let mut d = exe.parent().map(|p| p.to_path_buf());
            for _ in 0..3 {
                if let Some(dir) = d { bases.push(dir.clone()); d = dir.parent().map(|p| p.to_path_buf()); } else { break; }
            }
        }
        let mut found_root: Option<PathBuf> = None;
        if std::env::var("XG_TEX_REPLACE").is_err() {
            for b in &bases {
                let rep = b.join("TextureReplace");
                if rep.is_dir() { std::env::set_var("XG_TEX_REPLACE", &rep); found_root = Some(b.clone()); break; }
            }
        }
        if std::env::var("XG_MOVIE_REPLACE").is_err() {
            for b in &bases {
                let rep = b.join("MovieReplace");
                if rep.is_dir() { std::env::set_var("XG_MOVIE_REPLACE", &rep); break; }
            }
        }
        let root = found_root.or_else(|| bases.first().cloned());
        if let Some(r) = root { std::env::set_var("XG_TEX_DUMP_DEFAULT", r.join("TextureDump")); }
    }
    let mut m = match start(&data) {
        Ok(m) => m,
        Err(e) => {
            fatal(&format!("The game could not start: {}", e));
        }
    };
    // the picture's size factor: 1 is the console's own; the window defaults to 3 and the headless runner to 1
    let env_res: Option<i32> = std::env::var("XG_RES").ok().and_then(|v| v.parse().ok());
    let env_filter: Option<i32> = std::env::var("XG_FILTER").ok().and_then(|v| v.parse().ok());
    if let Some(f) = filter.or(env_filter) { m.gpu.set_filter(f); }
    let env_aa: Option<bool> = std::env::var("XG_AA").ok().map(|v| v != "0");
    if let Some(a) = aa.or(env_aa) { m.gpu.aa = a; }
    if std::env::var("XG_PERSP").map(|v| v == "0").unwrap_or(false) { m.gpu.persp = false; }
    if let Some(h) = headless_args {
        m.gpu.set_scale(res.or(env_res).unwrap_or(1));
        if h.len() < 3 {
            eprintln!("--headless <frames> <every> <prefix> [schedule]");
            std::process::exit(1);
        }
        headless(&mut m, h[0].parse().unwrap_or(600), h[1].parse().unwrap_or(0), &h[2], h.get(3).map(|s| s.as_str()).unwrap_or(""));
        return;
    }
    // the picture settings last chosen (the title menu or F1 to F3 save them beside the memory cards)
    let cfg_path = card_paths(&data).0.and_then(|p| p.parent().map(|d| d.join("picture.cfg")));
    let saved = cfg_path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let get = |key: &str| -> Option<i32> {
        saved.lines().find_map(|l| l.trim().strip_prefix(key).and_then(|r| r.strip_prefix('=')).and_then(|v| v.trim().parse().ok()))
    };
    if filter.is_none() && env_filter.is_none() { if let Some(f) = get("filter") { m.gpu.set_filter(f); } }
    if aa.is_none() && env_aa.is_none() { if let Some(a) = get("aa") { m.gpu.aa = a != 0; } }
    let saved_res = if res.is_none() && env_res.is_none() { get("res") } else { None };
    if get("locked") == Some(1) && saved_res.is_some() { m.gpu.size_locked = true; }
    m.gpu.set_scale(res.or(env_res).or(saved_res).unwrap_or(3));
    // PROG_LOAD file: start the window from a saved machine (as the headless runner does)
    if let Ok(path) = std::env::var("PROG_LOAD") {
        if let Ok(d) = std::fs::read(&path) { if !m.load_state(&d) { eprintln!("{}: not a saved machine", path); } }
    }
    let restart_data = data.clone();
    host::play(m, cfg_path, Some(Box::new(move || start(&restart_data).ok())));
    std::process::exit(0);
}
