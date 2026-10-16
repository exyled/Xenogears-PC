// The large picture on several threads. Every thread holds a full copy of the console's video memory and runs the same
// stream of drawing commands (so each can read any texture), but draws the large copy only for its stripes of video-memory
// rows (16 rows each, dealt out round-robin, so every polygon is shared evenly). The program's own thread records the
// commands as it executes them (packets, image loads, fills, the geometry coprocessor's vertex depths) and sends a batch
// each frame. At the end of the frame the threads each write their rows of the picture, then smooth its edges in bands,
// and the finished picture is handed to the window. The picture therefore shows the frame before the one the program is
// busy with, which costs one frame of delay and lets the program run ahead.

use super::*;
use std::cell::UnsafeCell;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread::JoinHandle;

struct Frame {
    id: u64,
    w: usize,
    h: usize,
    px: Vec<u32>,
}

struct SyncBuf(UnsafeCell<Vec<u32>>);
unsafe impl Sync for SyncBuf {}

struct Shared {
    slot: Mutex<Frame>,
    cv: Condvar,
    barrier: Barrier,
    done: std::sync::atomic::AtomicU64,
    raw: SyncBuf,
    fin: SyncBuf,
}

struct SendGpu(Gpu);
unsafe impl Send for SendGpu {}

pub struct Renderer {
    txs: Vec<SyncSender<Arc<Vec<u32>>>>,
    shared: Arc<Shared>,
    handles: Vec<JoinHandle<()>>,
    n: usize,
    presents: u64,
    taken: u64,
    sent: u64,
    cfg_sent: (i32, bool, bool),
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.txs.clear();
        for h in self.handles.drain(..) { let _ = h.join(); }
    }
}

/// How many worker threads to use for the large picture (0: draw it on the program's own thread).
pub fn worker_count() -> usize {
    if let Ok(v) = std::env::var("XG_THREADS") {
        if let Ok(n) = v.parse::<usize>() { return n.min(8); }
    }
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    if cores <= 1 { 0 } else { (cores - 1).clamp(1, 6) }
}

fn worker(g: SendGpu, idx: usize, n: usize, rx: Receiver<Arc<Vec<u32>>>, sh: Arc<Shared>) {
    let mut g = g.0;
    while let Ok(batch) = rx.recv() {
        let s = &batch[..];
        let mut p = 0usize;
        while p < s.len() {
            let op = s[p] >> 24;
            let arg = (s[p] & 0xFFFFFF) as usize;
            match op {
                OP_PACKET => {
                    g.execute_slice(&s[p + 1..p + 1 + arg]);
                    p += 1 + arg;
                }
                OP_LOAD => {
                    let (x, y, w, h) = (s[p + 1] as i32, s[p + 2] as i32, s[p + 3] as i32, s[p + 4] as i32);
                    let words = ((w * h).max(0) as usize + 1) / 2;
                    let data = &s[p + 5..p + 5 + words];
                    g.load_with(x, y, w, h, |k| ((data[(k >> 1) as usize] >> ((k & 1) * 16)) & 0xFFFF) as u16);
                    p += 5 + words;
                }
                OP_FILL => {
                    let c = s[p + 5];
                    g.fill_raw(s[p + 1] as i32, s[p + 2] as i32, s[p + 3] as i32, s[p + 4] as i32, (c & 255) as i32, ((c >> 8) & 255) as i32,
                        ((c >> 16) & 255) as i32);
                    p += 6;
                }
                OP_ZWRITES => {
                    for k in 0..arg {
                        let word = s[p + 1 + 2 * k];
                        let key = (word & 0xFFFFF) as usize;
                        if key < g.zown.len() { g.zown[key] = s[p + 2 + 2 * k]; g.zfown[key] = (word >> 24) as u8; }
                    }
                    p += 1 + 2 * arg;
                }
                OP_STAMP => {
                    g.zstamp = s[p + 1];
                    p += 2;
                }
                OP_CONFIG => {
                    g.filter = s[p + 1] as i32;
                    g.aa = s[p + 2] != 0;
                    g.persp = s[p + 3] != 0;
                    p += 4;
                }
                OP_PRESENT => {
                    g.disp_x = s[p + 1] as i32;
                    g.disp_y = s[p + 2] as i32;
                    g.disp_w = s[p + 3] as i32;
                    g.disp_h = s[p + 4] as i32;
                    g.line_top = s[p + 5] as i32;
                    g.line_bottom = s[p + 6] as i32;
                    g.display24 = s[p + 7] != 0;
                    present(&g, idx, n, &sh);
                    p += 8;
                }
                _ => break,
            }
        }
        if idx == 0 { sh.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
    }
}

// The picture of the display as the threads make it together: each writes the rows it drew, then each smooths a band.
fn present(g: &Gpu, idx: usize, n: usize, sh: &Shared) {
    let s = g.scale as usize;
    let (w, h) = (g.disp_w.max(1) as usize, g.disp_h.max(1) as usize);
    let (ow, oh) = (w * s, h * s);
    if idx == 0 {
        unsafe {
            (*sh.raw.0.get()).resize(ow * oh, 0);
            (*sh.fin.0.get()).resize(ow * oh, 0);
        }
    }
    sh.barrier.wait();
    let raw = unsafe { (*sh.raw.0.get()).as_mut_ptr() };
    let fin = unsafe { (*sh.fin.0.get()).as_mut_ptr() };
    if g.display24 {
        if idx == 0 {
            let mut small = Vec::new();
            g.read_display(&mut small);
            crate::post::deblock(&mut small, w, h);
            let mut big = Vec::new();
            crate::post::upscale_cubic(&small, w, h, s, &mut big);
            unsafe { std::ptr::copy_nonoverlapping(big.as_ptr(), raw, big.len().min(ow * oh)); }
        }
    } else {
        let vscale = if h > 240 { 2 } else { 1 };
        let first = (g.line_top - 16) * vscale;
        let last = (g.line_bottom - 16) * vscale;
        let hw = WIDTH as usize * s;
        for y in 0..h as i32 {
            if y < first || y >= last {
                if idx == 0 {
                    for sy in 0..s {
                        unsafe { std::ptr::write_bytes(raw.add((y as usize * s + sy) * ow), 0, ow); }
                    }
                }
                continue;
            }
            let src_y = (g.disp_y + y - first) & 511;
            if !g.owns(src_y) { continue; }
            for sy in 0..s {
                let src = (src_y as usize * s + sy) * hw;
                let dst = (y as usize * s + sy) * ow;
                let dx = g.disp_x as usize & 1023;
                if dx + w <= 1024 {
                    unsafe { std::ptr::copy_nonoverlapping(g.hi.as_ptr().add(src + dx * s), raw.add(dst), ow); }
                } else {
                    for x in 0..w {
                        let sx = ((g.disp_x as usize + x) & 1023) * s;
                        unsafe { std::ptr::copy_nonoverlapping(g.hi.as_ptr().add(src + sx), raw.add(dst + x * s), s); }
                    }
                }
            }
        }
    }
    sh.barrier.wait();
    let (y0, y1) = (idx * oh / n, (idx + 1) * oh / n);
    if g.aa && !g.display24 {
        unsafe { crate::post::antialias_rows(raw as *const u32, fin, ow, oh, y0, y1); }
    } else {
        unsafe { std::ptr::copy_nonoverlapping(raw.add(y0 * ow), fin.add(y0 * ow), (y1 - y0) * ow); }
    }
    sh.barrier.wait();
    if idx == 0 {
        let mut slot = sh.slot.lock().unwrap();
        std::mem::swap(&mut slot.px, unsafe { &mut *sh.fin.0.get() });
        slot.w = ow;
        slot.h = oh;
        slot.id += 1;
        sh.cv.notify_all();
    }
}

impl Gpu {
    // a copy of this one's video memory and drawing state for a worker, drawing the large copy's stripes idx of n
    fn make_replica(&self, idx: usize, n: usize) -> Gpu {
        let mut g = Gpu::new();
        g.vram.copy_from_slice(&self.vram);
        g.tp_x = self.tp_x; g.tp_y = self.tp_y; g.tp_mode = self.tp_mode; g.tp_depth = self.tp_depth; g.dither = self.dither;
        g.win_mask_x = self.win_mask_x; g.win_mask_y = self.win_mask_y; g.win_off_x = self.win_off_x; g.win_off_y = self.win_off_y;
        g.area_x1 = self.area_x1; g.area_y1 = self.area_y1; g.area_x2 = self.area_x2; g.area_y2 = self.area_y2;
        g.off_x = self.off_x; g.off_y = self.off_y;
        g.disp_x = self.disp_x; g.disp_y = self.disp_y; g.disp_w = self.disp_w; g.disp_h = self.disp_h;
        g.line_top = self.line_top; g.line_bottom = self.line_bottom; g.display24 = self.display24;
        g.scale = self.scale;
        g.filter = self.filter;
        g.aa = self.aa;
        g.persp = self.persp;
        g.band_i = idx as i32;
        g.band_n = n as i32;
        g.zown = vec![0u32; 1 << 20];
        g.zt = g.zown.as_ptr();
        g.zfown = vec![0u8; 1 << 20];
        g.zf = g.zfown.as_ptr();
        g.zstamp = self.zstamp;
        g.hi_on = true;
        g.hi = vec![0u32; (WIDTH * self.scale * HEIGHT * self.scale) as usize];
        g.hi_reseed();
        g
    }

    /// Starts `n` worker threads for the large picture from this video memory's present state.
    pub fn start_threads(&mut self, n: usize) {
        self.stop_threads();
        let n = n.max(1);
        let shared = Arc::new(Shared {
            slot: Mutex::new(Frame { id: 0, w: 0, h: 0, px: Vec::new() }),
            cv: Condvar::new(),
            barrier: Barrier::new(n),
            done: std::sync::atomic::AtomicU64::new(0),
            raw: SyncBuf(UnsafeCell::new(Vec::new())),
            fin: SyncBuf(UnsafeCell::new(Vec::new())),
        });
        let mut txs = Vec::new();
        let mut handles = Vec::new();
        for i in 0..n {
            let (tx, rx) = sync_channel::<Arc<Vec<u32>>>(8);
            let g = SendGpu(self.make_replica(i, n));
            let sh = shared.clone();
            handles.push(std::thread::spawn(move || worker(g, i, n, rx, sh)));
            txs.push(tx);
        }
        self.rec.clear();
        self.rend = Some(Box::new(Renderer { txs, shared, handles, n, presents: 0, taken: 0, sent: 0, cfg_sent: (self.filter, self.aa, self.persp) }));
        self.threaded = true;
        self.hi_on = false;
    }

    pub fn stop_threads(&mut self) {
        self.rend = None;
        self.rec.clear();
    }

    /// Rebuilds the workers from this video memory (after a saved machine is loaded).
    pub(super) fn restart_threads(&mut self) {
        let n = self.rend.as_ref().map(|r| r.n).unwrap_or(1);
        self.start_threads(n);
    }

    /// The geometry coprocessor's vertex depths since the last call, for the workers' perspective texturing.
    pub fn queue_z(&mut self, log: &mut Vec<u32>, stamp: u32) {
        if !self.threaded { log.clear(); return; }
        self.rec.push(OP_STAMP << 24);
        self.rec.push(stamp);
        if !log.is_empty() {
            self.rec.push(OP_ZWRITES << 24 | (log.len() / 2) as u32);
            self.rec.extend_from_slice(log);
            log.clear();
        }
    }

    fn send_batch(&mut self) {
        if self.rec.is_empty() { return; }
        let batch = Arc::new(std::mem::take(&mut self.rec));
        if let Some(r) = &mut self.rend {
            r.sent += 1;
            for tx in &r.txs { let _ = tx.send(batch.clone()); }
        }
    }

    /// Sends what has been recorded so far to the workers.
    pub fn flush_batch(&mut self) {
        if !self.threaded { return; }
        self.send_batch();
    }

    /// Ends a frame: the workers are told to make the picture of the display as it is now.
    pub fn end_frame(&mut self) {
        if !self.threaded { return; }
        let cfg = (self.filter, self.aa, self.persp);
        let (d_x, d_y, d_w, d_h, l_t, l_b, d24) =
            (self.disp_x, self.disp_y, self.disp_w, self.disp_h, self.line_top, self.line_bottom, self.display24);
        if let Some(r) = &mut self.rend {
            if r.cfg_sent != cfg {
                r.cfg_sent = cfg;
                self.rec.extend_from_slice(&[OP_CONFIG << 24, cfg.0 as u32, cfg.1 as u32, cfg.2 as u32]);
            }
            r.presents += 1;
        }
        self.rec.extend_from_slice(&[OP_PRESENT << 24, d_x as u32, d_y as u32, d_w as u32, d_h as u32, l_t as u32, l_b as u32, d24 as u32]);
        self.send_batch();
    }

    /// How many batches the workers have not finished (the picture is that many frames behind).
    pub fn backlog(&self) -> u64 {
        match &self.rend {
            Some(r) => r.sent.saturating_sub(r.shared.done.load(std::sync::atomic::Ordering::Relaxed)),
            None => 0,
        }
    }

    /// The newest finished picture if it is a new one (its size is returned), without waiting.
    pub fn poll_frame(&mut self, out: &mut Vec<u32>) -> Option<(usize, usize)> {
        let r = self.rend.as_mut()?;
        let mut slot = r.shared.slot.lock().unwrap();
        if slot.id <= r.taken || slot.w == 0 { return None; }
        r.taken = slot.id;
        std::mem::swap(&mut slot.px, out);
        Some((slot.w, slot.h))
    }

    /// Waits until every picture asked for has been made and gives the last.
    pub fn wait_frame(&mut self, out: &mut Vec<u32>) -> Option<(usize, usize)> {
        let r = self.rend.as_mut()?;
        let mut slot = r.shared.slot.lock().unwrap();
        while slot.id < r.presents {
            slot = r.shared.cv.wait(slot).unwrap();
        }
        r.taken = slot.id;
        out.clear();
        out.extend_from_slice(&slot.px);
        Some((slot.w, slot.h))
    }
}
