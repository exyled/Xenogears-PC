// Takes the game's data off the owner's own discs into the folder the player runs from (GameData):
//   archive_table.bin, archive_sections.bin   the game's file index (sector 24) and section header (sector 40)
//   archive/NNNN.bin                          every data entry, as stored on the disc
//   streams/NNNN.str                          every video and sound entry, as raw 2352-byte sectors
//   system/                                   the files of the disc's file system (SYSTEM.CNF, the program file)
//   index.csv, disc.txt                       what each entry is; which disc it came from
// The second disc goes into the same folder: its movies after the first disc's last entry, its program file in system/.
// Needs the raw .bin dump of each disc (2352 bytes a sector); a plain .iso has no movie sectors.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const USER: usize = 2048;
const RAW: usize = 2352;
const TABLE_LBA: u32 = 0x18;
const TABLE_SECTORS: usize = 16;
const HEADER_LBA: u32 = 0x28;
const HEADER_BYTES: usize = 0x7A;
const ENTRY: usize = 7;
const SECOND_DISC_MOVIES: usize = 4160;
const MAX_LZSS: usize = 2 * 1024 * 1024;

struct Disc {
    file: File,
    sectors: u32,
    raw: bool,
}

struct Entry {
    index: usize,
    lba: u32,
    size: i32,
}

impl Entry {
    fn empty(&self) -> bool { self.size <= 0 }
    fn sectors(&self) -> u32 { if self.empty() { 0 } else { (self.size as u32 + USER as u32 - 1) / USER as u32 } }
}

impl Disc {
    fn open(path: &Path) -> Result<Disc, String> {
        let mut file = File::open(path).map_err(|e| format!("{}: {}", path.display(), e))?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let mut head = [0u8; 12];
        file.read_exact(&mut head).map_err(|e| format!("{}: {}", path.display(), e))?;
        let sync = [0x00u8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        if head == sync {
            Ok(Disc { file, sectors: (len / RAW as u64) as u32, raw: true })
        } else if len % USER as u64 == 0 {
            Ok(Disc { file, sectors: (len / USER as u64) as u32, raw: false })
        } else {
            Err(format!("{}: not a recognizable disc image (no sync pattern, size not a multiple of 2048)", path.display()))
        }
    }

    fn raw_sector(&mut self, lba: u32) -> Result<[u8; RAW], String> {
        if !self.raw { return Err("the movies need the raw .bin image (2352 bytes a sector)".to_string()); }
        if lba >= self.sectors { return Err(format!("sector {} is past the end of the image", lba)); }
        let mut b = [0u8; RAW];
        self.file.seek(SeekFrom::Start(lba as u64 * RAW as u64)).map_err(|e| e.to_string())?;
        self.file.read_exact(&mut b).map_err(|e| e.to_string())?;
        Ok(b)
    }

    /// The 2048 bytes of user data of one sector.
    fn user_sector(&mut self, lba: u32) -> Result<[u8; USER], String> {
        if lba >= self.sectors { return Err(format!("sector {} is past the end of the image", lba)); }
        let mut out = [0u8; USER];
        if !self.raw {
            self.file.seek(SeekFrom::Start(lba as u64 * USER as u64)).map_err(|e| e.to_string())?;
            self.file.read_exact(&mut out).map_err(|e| e.to_string())?;
            return Ok(out);
        }
        let r = self.raw_sector(lba)?;
        let start = if r[15] == 2 { 24 } else { 16 };
        out.copy_from_slice(&r[start..start + USER]);
        Ok(out)
    }

    fn bytes(&mut self, lba: u32, len: usize) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(len);
        let mut l = lba;
        while out.len() < len {
            let s = self.user_sector(l)?;
            let n = USER.min(len - out.len());
            out.extend_from_slice(&s[..n]);
            l += 1;
        }
        Ok(out)
    }

    /// (is a stream sector run: Mode 2 Form 2, audio or video) and the kind name.
    fn stream_kind(&mut self, lba: u32) -> Option<&'static str> {
        if !self.raw { return None; }
        let mut h = [0u8; 24];
        self.file.seek(SeekFrom::Start(lba as u64 * RAW as u64)).ok()?;
        self.file.read_exact(&mut h).ok()?;
        let (mode, sub) = (h[15], h[18]);
        let form2 = mode == 2 && sub & 0x20 != 0;
        let audio = mode == 2 && sub & 0x04 != 0;
        let video = mode == 2 && sub & 0x02 != 0;
        if !form2 && !audio && !video { return None; }
        Some(if video { "video stream" } else if audio { "XA audio stream" } else { "Form 2 stream" })
    }
}

fn read_entries(table: &[u8]) -> Vec<Entry> {
    let mut v = Vec::new();
    let mut i = 0;
    while (i + 1) * ENTRY <= table.len() {
        let o = i * ENTRY;
        v.push(Entry {
            index: i,
            lba: table[o] as u32 | (table[o + 1] as u32) << 8 | (table[o + 2] as u32) << 16,
            size: i32::from_le_bytes([table[o + 3], table[o + 4], table[o + 5], table[o + 6]]),
        });
        i += 1;
    }
    v
}

struct IsoFile {
    path: String,
    lba: u32,
    size: u32,
}

/// The files of the disc's ISO 9660 file system (primary volume descriptor at sector 16, then the directory tree).
fn iso_files(d: &mut Disc) -> Result<Vec<IsoFile>, String> {
    let pvd = d.user_sector(16)?;
    if pvd[0] != 1 || &pvd[1..6] != b"CD001" { return Err("no ISO 9660 volume descriptor at sector 16".to_string()); }
    let root_lba = u32::from_le_bytes([pvd[158], pvd[159], pvd[160], pvd[161]]);
    let root_size = u32::from_le_bytes([pvd[166], pvd[167], pvd[168], pvd[169]]);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![(String::new(), root_lba, root_size)];
    while let Some((dir, lba, size)) = stack.pop() {
        if !seen.insert(lba) { continue; }
        let sectors = (size as usize + USER - 1) / USER;
        let data = d.bytes(lba, sectors * USER)?;
        let mut pos = 0usize;
        while pos < size as usize {
            let len = data[pos] as usize;
            if len == 0 { pos = (pos / USER + 1) * USER; continue; }
            if pos + len > data.len() || len < 34 { break; }
            let name_len = data[pos + 32] as usize;
            if !(name_len == 1 && (data[pos + 33] == 0 || data[pos + 33] == 1)) {
                let mut name = String::from_utf8_lossy(&data[pos + 33..pos + 33 + name_len]).to_string();
                if let Some(at) = name.find(';') { name.truncate(at); }
                if name.ends_with('.') { name.pop(); }
                let path = if dir.is_empty() { name } else { format!("{}/{}", dir, name) };
                let elba = u32::from_le_bytes([data[pos + 2], data[pos + 3], data[pos + 4], data[pos + 5]]);
                let esize = u32::from_le_bytes([data[pos + 10], data[pos + 11], data[pos + 12], data[pos + 13]]);
                if data[pos + 25] & 0x02 != 0 { stack.push((path, elba, esize)); }
                else { out.push(IsoFile { path, lba: elba, size: esize }); }
            }
            pos += len;
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn serial(cnf: &str) -> String {
    for line in cnf.lines() {
        let l = line.trim();
        if !l.to_ascii_uppercase().starts_with("BOOT") { continue; }
        if let Some(eq) = l.find('=') {
            let mut t = l[eq + 1..].trim().to_string();
            if let Some(s) = t.rfind(|c| c == '\\' || c == ':') { t = t[s + 1..].to_string(); }
            if let Some(s) = t.find(';') { t.truncate(s); }
            let up: String = t.to_uppercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            let split = up.chars().take_while(|c| c.is_ascii_alphabetic()).count();
            return if split > 0 && split < up.len() { format!("{}-{}", &up[..split], &up[split..]) } else { up };
        }
    }
    String::new()
}

fn identity(serial: &str) -> String {
    match serial {
        "SLUS-00664" => "Xenogears (NTSC-U, disc 1) [SLUS-00664]".to_string(),
        "SLUS-00669" => "Xenogears (NTSC-U, disc 2) [SLUS-00669]".to_string(),
        "SLPS-01160" => "Xenogears (NTSC-J, disc 1) [SLPS-01160]".to_string(),
        "SLPS-01161" => "Xenogears (NTSC-J, disc 2) [SLPS-01161]".to_string(),
        "" => "Unrecognized disc [no SYSTEM.CNF]".to_string(),
        s => format!("Unrecognized disc [{}]", s),
    }
}

fn u32le(d: &[u8], o: usize) -> u32 { u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]) }
fn u16le(d: &[u8], o: usize) -> usize { u16::from_le_bytes([d[o], d[o + 1]]) as usize }

/// What a data entry is, for index.csv: a TIM picture, a room file, a compressed file, or plain data.
fn describe(d: &[u8]) -> String {
    if let Some(t) = tim(d) { return format!("tim  {}", t); }
    if let Some(f) = field(d) { return f; }
    if looks_compressed(d) && lzss_ok(d) {
        return format!("lzss -> {} bytes", u32le(d, 0));
    }
    "raw".to_string()
}

fn tim(d: &[u8]) -> Option<String> {
    if d.len() < 8 || u32le(d, 0) != 0x10 { return None; }
    let flags = u32le(d, 4);
    if flags & !0xB != 0 { return None; }
    let bpp = [4, 8, 16, 24][(flags & 3) as usize];
    let has_clut = flags & 8 != 0;
    if bpp <= 8 && !has_clut { return None; }
    let mut pos = 8usize;
    let mut palettes = 0usize;
    if has_clut {
        if pos + 12 > d.len() { return None; }
        let len = u32le(d, pos) as usize;
        let (cw, ch) = (u16le(d, pos + 8), u16le(d, pos + 10));
        if cw == 0 || ch == 0 || cw > 1024 || ch > 512 { return None; }
        if len != 12 + cw * ch * 2 || pos + len > d.len() { return None; }
        let colors = match bpp { 4 => 16, 8 => 256, _ => cw }.min(cw * ch);
        palettes = if colors > 0 { cw * ch / colors } else { 0 };
        pos += len;
    }
    if pos + 12 > d.len() { return None; }
    let ilen = u32le(d, pos) as usize;
    let (vx, vy, w16, h) = (u16le(d, pos + 4), u16le(d, pos + 6), u16le(d, pos + 8), u16le(d, pos + 10));
    if w16 == 0 || h == 0 || w16 > 1024 || h > 512 { return None; }
    if ilen != 12 + w16 * h * 2 || pos + ilen > d.len() { return None; }
    let width = match bpp { 4 => w16 * 4, 8 => w16 * 2, 16 => w16, _ => w16 * 2 / 3 };
    Some(format!("TIM {}x{} {}bpp; {} palette(s); vram ({};{})", width, h, bpp, palettes, vx, vy))
}

/// A room file: nine section sizes at 0x10C, then the offsets of the nine compressed sections at 0x130.
fn field(d: &[u8]) -> Option<String> {
    const NAMES: [&str; 9] = ["textures", "walkmesh", "models", "sprites", "palettes", "scripts", "unknown6", "dialogue", "triggers"];
    if d.len() < 0x154 { return None; }
    let mut checked = 0;
    let mut parts = Vec::new();
    for i in 0..9 {
        let size = i32::from_le_bytes([d[0x10C + i * 4], d[0x10D + i * 4], d[0x10E + i * 4], d[0x10F + i * 4]]);
        let off = i32::from_le_bytes([d[0x130 + i * 4], d[0x131 + i * 4], d[0x132 + i * 4], d[0x133 + i * 4]]);
        if size < 0 || size as usize > MAX_LZSS { return None; }
        if size == 0 { continue; }
        if off < 0x154 || off as usize + 4 > d.len() { return None; }
        let unpacked = u32le(d, off as usize) as i64;
        if unpacked < size as i64 || unpacked > size as i64 + 0x10 { return None; }
        checked += 1;
        parts.push(format!("{} {}", NAMES[i], size));
    }
    if checked < 3 { return None; }
    Some(format!("field: {}", parts.join("; ")))
}

fn looks_compressed(d: &[u8]) -> bool {
    if d.len() < 4 { return false; }
    let size = u32le(d, 0) as i64;
    size > d.len() as i64 && size <= (MAX_LZSS as i64).min(d.len() as i64 * 16)
}

/// The file's compressed stream unpacks completely (a back-reference never points before the start).
fn lzss_ok(d: &[u8]) -> bool {
    let size = u32le(d, 0) as usize;
    if size == 0 || size > MAX_LZSS { return false; }
    let (mut src, mut dst) = (4usize, 0usize);
    while dst < size {
        if src >= d.len() { return false; }
        let mut flags = d[src];
        src += 1;
        for _ in 0..8 {
            if dst >= size { break; }
            if flags & 1 == 0 {
                if src >= d.len() { return false; }
                src += 1;
                dst += 1;
            } else {
                if src + 1 >= d.len() { return false; }
                let (b0, b1) = (d[src] as usize, d[src + 1] as usize);
                src += 2;
                let dist = ((b1 & 0x0F) << 8) | b0;
                let len = (b1 >> 4) + 3;
                if dist == 0 || dist > dst { return false; }
                dst = (dst + len).min(size);
            }
            flags >>= 1;
        }
    }
    true
}

fn write(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(p) = path.parent() { fs::create_dir_all(p).map_err(|e| e.to_string())?; }
    fs::write(path, data).map_err(|e| format!("{}: {}", path.display(), e))
}

/// Exports the first disc into `out`.
pub fn export(image: &Path, out: &Path) -> Result<String, String> {
    let mut d = Disc::open(image)?;
    let table = d.bytes(TABLE_LBA, TABLE_SECTORS * USER)?;
    let header = d.bytes(HEADER_LBA, HEADER_BYTES)?;
    let entries = read_entries(&table);
    fs::create_dir_all(out.join("archive")).map_err(|e| e.to_string())?;
    fs::create_dir_all(out.join("streams")).map_err(|e| e.to_string())?;
    let mut index = String::from("entry,lba,size,kind\n");
    let (mut written, mut streamed, mut missing) = (0usize, 0usize, 0usize);
    let mut bytes = 0u64;
    for e in &entries {
        if e.empty() { continue; }
        if e.lba as u64 + e.sectors() as u64 > d.sectors as u64 {
            index.push_str(&format!("{},{},{},past the end of the image (not exported)\n", e.index, e.lba, e.size));
            missing += 1;
            continue;
        }
        if let Some(kind) = d.stream_kind(e.lba) {
            let mut f = File::create(out.join("streams").join(format!("{:04}.str", e.index))).map_err(|x| x.to_string())?;
            for l in e.lba..e.lba + e.sectors() {
                f.write_all(&d.raw_sector(l)?).map_err(|x| x.to_string())?;
            }
            index.push_str(&format!("{},{},{},{} (streams/{:04}.str)\n", e.index, e.lba, e.size, kind, e.index));
            streamed += 1;
            bytes += e.sectors() as u64 * RAW as u64;
        } else {
            let data = d.bytes(e.lba, e.size as usize)?;
            write(&out.join("archive").join(format!("{:04}.bin", e.index)), &data)?;
            index.push_str(&format!("{},{},{},{}\n", e.index, e.lba, e.size, describe(&data).replace(',', ";")));
            written += 1;
            bytes += data.len() as u64;
        }
        if (written + streamed) % 200 == 0 { println!("  {} entries", written + streamed); }
    }
    write(&out.join("index.csv"), index.as_bytes())?;
    write(&out.join("archive_table.bin"), &table)?;
    write(&out.join("archive_sections.bin"), &header)?;

    let mut named = 0;
    let mut cnf = String::new();
    if let Ok(files) = iso_files(&mut d) {
        for f in files {
            let data = d.bytes(f.lba, f.size as usize)?;
            if f.path.eq_ignore_ascii_case("SYSTEM.CNF") { cnf = String::from_utf8_lossy(&data).to_string(); }
            write(&out.join("system").join(&f.path), &data)?;
            named += 1;
        }
    }
    let who = identity(&serial(&cnf));
    let summary = format!("{}: {} data entries and {} streams exported ({:.0} MB), {} named files; {} entries past the end of the image",
        who, written, streamed, bytes as f64 / 1048576.0, named, missing);
    let info = format!("identity={}\nformat={}\nsectors={}\n{}\n", who, if d.raw { "Raw2352" } else { "Iso2048" }, d.sectors, summary);
    write(&out.join("disc.txt"), info.as_bytes())?;
    Ok(summary)
}

/// Adds the second disc to an exported folder: its movies as entries 4160 + k, its program file in system/.
pub fn add_second_disc(image: &Path, out: &Path) -> Result<String, String> {
    if !out.join("archive_table.bin").exists() { return Err(format!("no exported game data in {} (export the first disc first)", out.display())); }
    let mut d = Disc::open(image)?;
    if !d.raw { return Err("the second disc's movies need the raw .bin image (2352 bytes a sector)".to_string()); }
    let second_table = d.bytes(TABLE_LBA, TABLE_SECTORS * USER)?;
    let entries = read_entries(&second_table);
    let mut table = fs::read(out.join("archive_table.bin")).map_err(|e| e.to_string())?;
    let mut lines: Vec<String> = fs::read_to_string(out.join("index.csv")).unwrap_or_default().lines().map(|s| s.to_string()).collect();
    fs::create_dir_all(out.join("streams")).map_err(|e| e.to_string())?;
    let (mut movies, mut bytes) = (0usize, 0u64);
    for e in &entries {
        if e.empty() || e.lba as u64 + e.sectors() as u64 > d.sectors as u64 { continue; }
        let kind = match d.stream_kind(e.lba) { Some(k) => k, None => continue };
        let target = SECOND_DISC_MOVIES + e.index;
        if (target + 1) * ENTRY > table.len() { return Err(format!("no room in the file index for entry {}", target)); }
        let mut f = File::create(out.join("streams").join(format!("{:04}.str", target))).map_err(|x| x.to_string())?;
        for l in e.lba..e.lba + e.sectors() { f.write_all(&d.raw_sector(l)?).map_err(|x| x.to_string())?; }
        let o = target * ENTRY;
        table[o] = e.lba as u8; table[o + 1] = (e.lba >> 8) as u8; table[o + 2] = (e.lba >> 16) as u8;
        let s = e.size as u32;
        table[o + 3] = s as u8; table[o + 4] = (s >> 8) as u8; table[o + 5] = (s >> 16) as u8; table[o + 6] = (s >> 24) as u8;
        let prefix = format!("{},", target);
        lines.retain(|l| !l.starts_with(&prefix));
        lines.push(format!("{},{},{},{} (streams/{:04}.str)", target, e.lba, e.size, kind, target));
        movies += 1;
        bytes += e.sectors() as u64 * RAW as u64;
        println!("  {} movies", movies);
    }
    write(&out.join("archive_table.bin"), &table)?;
    write(&out.join("index.csv"), (lines.join("\n") + "\n").as_bytes())?;
    let mut named = 0;
    if let Ok(files) = iso_files(&mut d) {
        for f in files {
            let name = f.path.rsplit('/').next().unwrap_or(&f.path).to_string();
            if name.eq_ignore_ascii_case("SYSTEM.CNF") { continue; }
            let data = d.bytes(f.lba, f.size as usize)?;
            write(&out.join("system").join(&name), &data)?;
            named += 1;
        }
    }
    let summary = format!("second disc: {} movies added as entries {}-{} ({:.0} MB), {} named file(s)",
        movies, SECOND_DISC_MOVIES + 1, SECOND_DISC_MOVIES + movies, bytes as f64 / 1048576.0, named);
    let mut info: Vec<String> = fs::read_to_string(out.join("disc.txt")).unwrap_or_default().lines().filter(|l| !l.starts_with("second=")).map(|s| s.to_string()).collect();
    info.push(format!("second={}", summary));
    write(&out.join("disc.txt"), (info.join("\n") + "\n").as_bytes())?;
    Ok(summary)
}

/// `--extract <disc 1 .bin> [<disc 2 .bin>] [--out <folder>]`
pub fn run(args: &[String]) -> i32 {
    let mut images: Vec<PathBuf> = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--out" && i + 1 < args.len() { out = Some(PathBuf::from(&args[i + 1])); i += 2; }
        else { images.push(PathBuf::from(&args[i])); i += 1; }
    }
    if images.is_empty() || images.len() > 2 {
        eprintln!("usage: xenogears --extract <disc 1 .bin> [<disc 2 .bin>] [--out <GameData folder>]");
        return 2;
    }
    let out = out.unwrap_or_else(|| {
        match std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("GameData"))) {
            Some(p) => p,
            None => PathBuf::from("GameData"),
        }
    });
    println!("writing {}", out.display());
    match export(&images[0], &out) {
        Ok(s) => println!("{}", s),
        Err(e) => { eprintln!("{}", e); return 1; }
    }
    if images.len() == 2 {
        match add_second_disc(&images[1], &out) {
            Ok(s) => println!("{}", s),
            Err(e) => { eprintln!("{}", e); return 1; }
        }
    }
    println!("done: run Xenogears.exe with this folder beside it");
    0
}
