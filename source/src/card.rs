// A memory card as the kernel's file calls see it, kept as the standard 128 KB card image (the .mcd layout other tools
// read): frame 0 the "MC" header, frames 1-15 the directory, blocks 1-15 of 8 KB the files themselves. Every change is
// written back at once.

use std::path::PathBuf;

pub const SIZE: usize = 0x20000;
pub const BLOCK: usize = 0x2000;
pub const FRAME: usize = 0x80;
pub const BLOCKS: usize = 15;

pub struct Card {
    pub image: Vec<u8>,
    path: Option<PathBuf>,
}

impl Card {
    pub fn new(path: Option<PathBuf>) -> Card {
        let mut c = Card { image: vec![0; SIZE], path };
        let loaded = match &c.path {
            Some(p) => match std::fs::read(p) {
                Ok(d) if d.len() == SIZE => { c.image.copy_from_slice(&d); true }
                _ => false,
            },
            None => false,
        };
        if !loaded { c.format(); }
        c
    }

    pub fn format(&mut self) {
        self.image.iter_mut().for_each(|b| *b = 0);
        self.image[0] = b'M';
        self.image[1] = b'C';
        self.check(0);
        for b in 1..=BLOCKS {
            let f = b * FRAME;
            self.image[f] = 0xA0;
            self.image[f + 8] = 0xFF;
            self.image[f + 9] = 0xFF;
            self.check(f);
        }
        for i in 16..36 {
            let f = i * FRAME;
            for k in 0..4 { self.image[f + k] = 0xFF; }
            self.image[f + 8] = 0xFF;
            self.image[f + 9] = 0xFF;
            self.check(f);
        }
        self.save();
    }

    fn check(&mut self, frame: usize) {
        let mut x = 0u8;
        for i in 0..FRAME - 1 { x ^= self.image[frame + i]; }
        self.image[frame + FRAME - 1] = x;
    }

    fn save(&self) {
        if let Some(p) = &self.path {
            if let Some(dir) = p.parent() { let _ = std::fs::create_dir_all(dir); }
            let mut tmp = p.clone().into_os_string();
            tmp.push(".new");
            let tmp = PathBuf::from(tmp);
            if std::fs::write(&tmp, &self.image).is_ok() {
                let _ = std::fs::remove_file(p);
                let _ = std::fs::rename(&tmp, p);
            }
        }
    }

    fn dir(block: usize) -> usize { block * FRAME }
    fn state(&self, block: usize) -> u8 { self.image[Card::dir(block)] }
    fn name(&self, block: usize) -> String {
        let f = Card::dir(block) + 0x0A;
        let mut n = 0;
        while n < 20 && self.image[f + n] != 0 { n += 1; }
        self.image[f..f + n].iter().map(|&b| b as char).collect()
    }
    fn next(&self, block: usize) -> i32 {
        let v = self.image[Card::dir(block) + 8] as i32 | (self.image[Card::dir(block) + 9] as i32) << 8;
        if v == 0xFFFF { -1 } else { v + 1 }
    }
    pub fn file_size(&self, first: i32) -> i32 {
        let d = Card::dir(first as usize) + 4;
        i32::from_le_bytes([self.image[d], self.image[d + 1], self.image[d + 2], self.image[d + 3]])
    }

    /// The first block of the file of that name, or -1.
    pub fn find(&self, name: &str) -> i32 {
        for b in 1..=BLOCKS {
            if self.state(b) == 0x51 && self.name(b) == name { return b as i32; }
        }
        -1
    }

    fn chain(&self, first: i32) -> Vec<usize> {
        let mut list = Vec::new();
        let mut b = first;
        let mut guard = 0;
        while b > 0 && guard < BLOCKS {
            list.push(b as usize);
            b = self.next(b as usize);
            guard += 1;
        }
        list
    }

    /// Makes a file of `blocks` blocks; its first block, or -1 (the name taken, or no room).
    pub fn create(&mut self, name: &str, blocks: i32) -> i32 {
        let blocks = blocks.max(1) as usize;
        if self.find(name) >= 0 || name.is_empty() || name.len() > 20 { return -1; }
        let mut free = Vec::new();
        let mut b = 1;
        while b <= BLOCKS && free.len() < blocks {
            if self.state(b) & 0xF0 == 0xA0 { free.push(b); }
            b += 1;
        }
        if free.len() < blocks { return -1; }
        for i in 0..free.len() {
            let f = Card::dir(free[i]);
            for k in 0..FRAME { self.image[f + k] = 0; }
            self.image[f] = if i == 0 { 0x51 } else if i == free.len() - 1 { 0x53 } else { 0x52 };
            if i == 0 {
                let sz = (blocks * BLOCK) as i32;
                self.image[f + 4..f + 8].copy_from_slice(&sz.to_le_bytes());
            }
            let next = if i + 1 < free.len() { (free[i + 1] - 1) as i32 } else { 0xFFFF };
            self.image[f + 8] = next as u8;
            self.image[f + 9] = (next >> 8) as u8;
            let nm = name.as_bytes();
            self.image[f + 0x0A..f + 0x0A + nm.len()].copy_from_slice(nm);
            self.check(f);
            let s = free[i] * BLOCK;
            for k in 0..BLOCK { self.image[s + k] = 0; }
        }
        if free.len() == 1 {
            let f = Card::dir(free[0]);
            self.image[f] = 0x51;
            self.check(f);
        }
        self.save();
        free[0] as i32
    }

    pub fn erase(&mut self, name: &str) -> bool {
        let first = self.find(name);
        if first < 0 { return false; }
        for b in self.chain(first) {
            let f = Card::dir(b);
            self.image[f] = 0xA0 | (self.image[f] & 0x0F);
            self.check(f);
        }
        self.save();
        true
    }

    pub fn rename(&mut self, from: &str, to: &str) -> bool {
        let first = self.find(from);
        if first < 0 || self.find(to) >= 0 || to.is_empty() || to.len() > 20 { return false; }
        for b in self.chain(first) {
            let f = Card::dir(b);
            for k in 0..21 { self.image[f + 0x0A + k] = 0; }
            let nm = to.as_bytes();
            self.image[f + 0x0A..f + 0x0A + nm.len()].copy_from_slice(nm);
            self.check(f);
        }
        self.save();
        true
    }

    /// The names of the files matching a pattern (? one character, * the rest), in directory order.
    pub fn list(&self, pattern: &str) -> Vec<String> {
        let pattern = if pattern.is_empty() { "*" } else { pattern };
        let mut out = Vec::new();
        for b in 1..=BLOCKS {
            if self.state(b) == 0x51 && Card::matches(pattern, &self.name(b)) { out.push(self.name(b)); }
        }
        out
    }

    fn matches(p: &str, s: &str) -> bool {
        let p = p.as_bytes();
        let s = s.as_bytes();
        for i in 0..p.len() {
            if p[i] == b'*' { return true; }
            if i >= s.len() { return false; }
            if p[i] != b'?' && p[i] != s[i] { return false; }
        }
        p.len() == s.len()
    }

    /// Copies a file's bytes [at, at + n) out of (or into) `data`; the bytes moved.
    pub fn transfer(&mut self, first: i32, at: usize, data: &mut [u8], n: usize, write: bool) -> usize {
        let chain = self.chain(first);
        let size = chain.len() * BLOCK;
        let mut done = 0;
        while done < n && at + done < size {
            let pos = at + done;
            let b = chain[pos / BLOCK];
            let in_block = pos % BLOCK;
            let take = (n - done).min(BLOCK - in_block);
            let o = b * BLOCK + in_block;
            if write { self.image[o..o + take].copy_from_slice(&data[done..done + take]); }
            else { data[done..done + take].copy_from_slice(&self.image[o..o + take]); }
            done += take;
        }
        if write && done > 0 { self.save(); }
        done
    }
}
