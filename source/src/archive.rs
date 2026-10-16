// The game's own file index (how it finds its data, by number) read from the exported game-data folder:
// archive_table.bin (7-byte entries: 24-bit start sector, 32-bit size), archive_sections.bin (61 u16 values),
// index.csv (which entries are streams), archive/NNNN.bin and streams/NNNN.str. A layout.csv beside them (entry,archive|stream,
// path relative to the data folder[,the path it had before]) says where each file was put when the folder was sorted into named folders; entries it
// does not list are looked for in archive/ and streams/ as before.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Where the game's files are kept: a folder of ordinary files.
pub enum Store {
    Dir(PathBuf),
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, String> { Ok(Store::Dir(path.to_path_buf())) }

    pub fn exists(&self, rel: &Path) -> bool {
        match self { Store::Dir(d) => d.join(rel).exists() }
    }

    pub fn read(&self, rel: &Path) -> Option<Vec<u8>> {
        match self { Store::Dir(d) => std::fs::read(d.join(rel)).ok() }
    }
}

pub const ENTRY_SIZE: usize = 7;
pub const HEADER_BYTES: usize = 0x7A;

#[derive(Clone, Copy)]
pub struct Entry {
    pub lba: u32,
    pub size: i32,
}

impl Entry {
    pub fn is_empty(&self) -> bool { self.size <= 0 }
}

pub struct Archive {
    pub store: Store,
    pub entries: Vec<Entry>,
    pub sections: Vec<u16>,
    streams: HashMap<usize, String>,
    files: HashMap<usize, Vec<PathBuf>>,
    stream_files: HashMap<usize, PathBuf>,
}

impl Archive {
    pub fn open(dir: &Path) -> Result<Archive, String> {
        let store = Store::open(dir)?;
        let table = store.read(Path::new("archive_table.bin")).ok_or("archive_table.bin: not found")?;
        let header = store.read(Path::new("archive_sections.bin")).ok_or("archive_sections.bin: not found")?;
        let mut entries = Vec::new();
        let mut i = 0;
        while (i + 1) * ENTRY_SIZE <= table.len() {
            let o = i * ENTRY_SIZE;
            entries.push(Entry {
                lba: table[o] as u32 | (table[o + 1] as u32) << 8 | (table[o + 2] as u32) << 16,
                size: i32::from_le_bytes([table[o + 3], table[o + 4], table[o + 5], table[o + 6]]),
            });
            i += 1;
        }
        let mut sections = vec![0u16; HEADER_BYTES / 2];
        for (i, s) in sections.iter_mut().enumerate() {
            if i * 2 + 1 < header.len() { *s = header[i * 2] as u16 | (header[i * 2 + 1] as u16) << 8; }
        }
        let mut streams = HashMap::new();
        if let Some(text) = store.read(Path::new("index.csv")).map(|b| String::from_utf8_lossy(&b).to_string()) {
            for line in text.lines() {
                let parts: Vec<&str> = line.splitn(4, ',').collect();
                if parts.len() != 4 { continue; }
                let n: usize = match parts[0].trim().parse() { Ok(n) => n, Err(_) => continue };
                let kind = parts[3];
                if kind.ends_with("(not exported)") {
                    streams.insert(n, kind.replace(" (not exported)", ""));
                } else if let Some(at) = kind.find(" (streams/") {
                    streams.insert(n, kind[..at].to_string());
                }
            }
        }
        let mut files = HashMap::new();
        let mut stream_files = HashMap::new();
        if let Some(text) = store.read(Path::new("layout.csv")).map(|b| String::from_utf8_lossy(&b).to_string()) {
            for line in text.lines() {
                let parts: Vec<&str> = line.splitn(4, ',').collect();
                if parts.len() < 3 { continue; }
                let n: usize = match parts[0].trim().parse() { Ok(n) => n, Err(_) => continue };
                // an entry kept as several files lists them with | between, in the order they are joined
                let paths: Vec<PathBuf> = parts[2].trim().split('|').map(PathBuf::from).collect();
                match parts[1].trim() {
                    "archive" => { files.insert(n, paths); }
                    "stream" => { stream_files.insert(n, paths.into_iter().next().unwrap_or_default()); }
                    _ => {}
                }
            }
        }
        Ok(Archive { store, entries, sections, streams, files, stream_files })
    }

    pub fn is_stream(&self, entry: usize) -> bool {
        !self.entries[entry].is_empty() && self.streams.contains_key(&entry)
    }

    fn pack_files(&self, entry: usize) -> Option<Vec<PathBuf>> {
        let list: Vec<PathBuf> = match self.files.get(&entry) {
            Some(rels) => rels.clone(),
            None => vec![PathBuf::from("archive").join(format!("{:04}.bin", entry))],
        };
        if list.iter().all(|p| self.store.exists(p)) { Some(list) } else { None }
    }

    pub fn can_read(&self, entry: usize) -> bool {
        !self.entries[entry].is_empty() && self.pack_files(entry).is_some()
    }

    pub fn read(&self, entry: usize) -> Vec<u8> {
        if self.entries[entry].is_empty() { return Vec::new(); }
        match self.pack_files(entry) {
            Some(list) => {
                let mut out = Vec::new();
                for p in list { out.extend(self.store.read(&p).unwrap_or_default()); }
                out
            }
            None => Vec::new(),
        }
    }

    pub fn read_raw_sectors(&self, entry: usize) -> Option<Vec<u8>> {
        if self.entries[entry].is_empty() { return None; }
        let p = match self.stream_files.get(&entry) {
            Some(rel) => rel.clone(),
            None => PathBuf::from("streams").join(format!("{:04}.str", entry)),
        };
        self.store.read(&p)
    }
}
