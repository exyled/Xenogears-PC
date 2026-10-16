// The picture settings on the title screen: three more entries in the column to the right of New Game / Continue / Sound,
// drawn like the game's own (a marble and a line of text, the red marble and arrow showing the selection). Right moves to
// them from the menu, Up / Down choose, Cross changes the setting (the fourth opens the settings screen), Left or Circle goes back; while they have the focus the
// program sees no buttons at all. They are drawn by adding packets to the list that draws the menu, using the game's own
// marble pictures and palette; the text is drawn with a small pixel font into free video memory.

use super::*;
use crate::pad;

// the game's own menu pieces, as its packets show them
const TPAGE_TEXT: u32 = 0x0007;
const TPAGE_MARBLE: u32 = 0x0087;
const CLUT_TEXT: u32 = 0x7F51;
const CLUT_RED: u32 = 0x7BC0;
const CLUT_BROWN: u32 = 0x7C00;
const CLUT_ARROW: u32 = 0x7FD2;
const UV_NEW_GAME: u32 = 0xA600; // v 166, u 0
const UV_CONTINUE: u32 = 0xA62C; // v 166, u 44
const UV_ARROW: u32 = 0x10C0; // v 16, u 192
const MARBLE_RED_UV: (u32, u32) = (40, 132);
const MARBLE_BROWN_UV: (u32, u32) = (24, 132);

// where ours go, and where their text is kept (a 4-bit texture page in video memory the title screen does not use)
const ROW_X: [i32; 1] = [222];
const ROW_Y: [i32; 1] = [196];
const LABEL_HW: i32 = 24; // halfwords per label row (96 texels)
const ROW_W: i32 = 70; // the width a picture entry's text takes beside its marble
const LABEL_H: i32 = 11;
const LABEL_X: i32 = 512;
const LABEL_Y: i32 = 256;
const TPAGE_LABEL: u32 = 0x0018; // page x 512, y 256, 4-bit

pub struct TitleMenu {
    pub focus: bool,
    pub row: usize,
    seen_now: bool,
    last_seen: u32,
    ever_seen: bool,
    prev_pad: i32,
}

impl TitleMenu {
    pub fn new() -> TitleMenu {
        TitleMenu { focus: false, row: 0, seen_now: false, last_seen: 0, ever_seen: false, prev_pad: 0 }
    }
}

// A label as 4-bit texels in the game's own text colours (15 white, 1 for the dark edge around it, 0 clear), packed four
// to a halfword, LABEL_HW x LABEL_H halfwords.
fn label_bitmap(text: &str) -> Vec<u16> {
    let w = (LABEL_HW * 4) as usize;
    let h = LABEL_H as usize;
    let mut ink = vec![false; w * h];
    for (n, c) in text.chars().enumerate() {
        for j in 0..8 {
            for i in 0..5 {
                if crate::font::pixel(c, i, j) {
                    let (x, y) = (1 + n * 6 + i, 1 + j);
                    if x < w && y < h { ink[y * w + x] = true; }
                }
            }
        }
    }
    let mut px = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            if ink[y * w + x] { px[y * w + x] = 15; continue; }
            'near: for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (xx, yy) = (x as i32 + dx, y as i32 + dy);
                    if xx >= 0 && yy >= 0 && (xx as usize) < w && (yy as usize) < h && ink[yy as usize * w + xx as usize] {
                        px[y * w + x] = 1;
                        break 'near;
                    }
                }
            }
        }
    }
    let mut out = vec![0u16; LABEL_HW as usize * h];
    for y in 0..h {
        for k in 0..LABEL_HW as usize {
            let mut v = 0u16;
            for t in 0..4 { v |= (px[y * w + k * 4 + t] as u16) << (t * 4); }
            out[y * LABEL_HW as usize + k] = v;
        }
    }
    out
}

fn quad(op: u32, x: i32, y: i32, w: i32, h: i32, u: u32, v: u32, clut: u32, tpage: u32) -> [u32; 9] {
    let xy = |a: i32, b: i32| ((b as u32) << 16) | (a as u32 & 0xFFFF);
    [
        op << 24,
        xy(x, y), (clut << 16) | (v << 8) | u,
        xy(x + w, y), (tpage << 16) | (v << 8) | (u + w as u32),
        xy(x, y + h), (v + h as u32) << 8 | u,
        xy(x + w, y + h), (v + h as u32) << 8 | (u + w as u32),
    ]
}

impl Gpu {
    /// True while the title menu is on the screen.
    pub fn title_showing(&self) -> bool { self.title_active() }

    fn title_active(&self) -> bool {
        self.title.ever_seen && (self.zstamp.wrapping_sub(self.title.last_seen) & 0xFFFF) <= 1
    }

    fn label(&self, _row: usize) -> String {
        "Settings".to_string()
    }

    /// The buttons the program is to see this frame. On the title menu, Right moves to the picture entries and they take
    /// the buttons while they have the focus.
    pub fn title_pad(&mut self, raw: i32) -> i32 {
        let pressed = raw & !self.title.prev_pad;
        self.title.prev_pad = raw;
        if !self.title_active() {
            self.title.focus = false;
            return raw;
        }
        if !self.title.focus {
            if pressed & pad::RIGHT != 0 {
                self.title.focus = true;
                self.title.row = 0;
                return 0;
            }
            return raw;
        }
        if pressed & pad::CROSS != 0 {
            // the picture's own settings are all on the settings screen now
            self.settings_request = true;
        }
        if pressed & (pad::LEFT | pad::CIRCLE) != 0 { self.title.focus = false; }
        0
    }

    /// Looks at one packet of the menu's list: the program's own menu text marks the menu as showing, and while the picture
    /// entries have the focus the red marble and the arrow are taken off the program's entry. True when it drew the packet.
    pub(super) fn title_packet(&mut self, m: &Mem, at: u32) -> bool {
        let cmd = m.read32(at);
        if cmd >> 24 != 0x2D { return false; }
        let w2 = m.read32(at.wrapping_add(8));
        let w4 = m.read32(at.wrapping_add(16));
        let (clut, uv0, tpage) = (w2 >> 16, w2 & 0xFFFF, w4 >> 16);
        if clut == CLUT_TEXT && tpage == TPAGE_TEXT && (uv0 == UV_NEW_GAME || uv0 == UV_CONTINUE) {
            self.title.seen_now = true;
            return false;
        }
        if !self.title.focus { return false; }
        let mut words = [0u32; 9];
        for (i, w) in words.iter_mut().enumerate() { *w = m.read32(at.wrapping_add(4 * i as u32)); }
        if tpage == TPAGE_MARBLE && clut == CLUT_RED {
            words[2] = (CLUT_BROWN << 16) | uv0;
            self.execute_words(&words);
            return true;
        }
        if tpage == TPAGE_TEXT && clut == CLUT_ARROW && uv0 == UV_ARROW {
            let x0 = words[1] as i16 as i32;
            let y0 = (words[1] >> 16) as i16 as i32;
            let r = self.title.row;
            let (dx, dy) = (ROW_X[r] + 7 - x0, ROW_Y[r] - 27 - y0);
            for k in [1usize, 3, 5, 7] {
                let x = (words[k] as i16 as i32) + dx;
                let y = ((words[k] >> 16) as i16 as i32) + dy;
                words[k] = ((y as u32) << 16) | (x as u32 & 0xFFFF);
            }
            self.execute_words(&words);
            return true;
        }
        false
    }

    // packets given as words: sent to the workers as they are, and run
    fn execute_words(&mut self, w: &[u32]) {
        if self.threaded {
            self.rec.push(OP_PACKET << 24 | w.len() as u32);
            self.rec.extend_from_slice(w);
        }
        self.execute_slice(w);
    }

    /// After the menu's list: the picture entries (and their text, made fresh into video memory each time).
    pub(super) fn title_overlay(&mut self) {
        if !self.title.seen_now { return; }
        self.title.seen_now = false;
        self.title.ever_seen = true;
        self.title.last_seen = self.zstamp;
        let mut words: Vec<u32> = Vec::new();
        for r in 0..1 {
            let text = self.label(r);
            let bitmap = label_bitmap(&text);
            let ly = LABEL_Y + r as i32 * 12;
            if self.threaded {
                let n = bitmap.len();
                self.rec.extend_from_slice(&[OP_LOAD << 24, LABEL_X as u32, ly as u32, LABEL_HW as u32, LABEL_H as u32]);
                let mut k = 0;
                while k < n {
                    let lo = bitmap[k] as u32;
                    let hi = if k + 1 < n { bitmap[k + 1] as u32 } else { 0 };
                    self.rec.push(lo | hi << 16);
                    k += 2;
                }
            }
            self.load_with(LABEL_X, ly, LABEL_HW, LABEL_H, |k| bitmap[k as usize]);
            let red = self.title.focus && self.title.row == r;
            let (u, v) = if red { MARBLE_RED_UV } else { MARBLE_BROWN_UV };
            let clut = if red { CLUT_RED } else { CLUT_BROWN };
            words.extend_from_slice(&quad(0x2D, ROW_X[r], ROW_Y[r], 16, 16, u, v, clut, TPAGE_MARBLE));
            words.extend_from_slice(&quad(0x2D, ROW_X[r] + 9, ROW_Y[r] + 2, ROW_W, LABEL_H, 0, (r as i32 * 12) as u32, CLUT_TEXT, TPAGE_LABEL));
        }
        // the way in that needs no cursor: the pad's Select button (or F12) opens the settings screen
        {
            let r = 4usize;
            let bitmap = label_bitmap("Sel: Settings");
            let ly = LABEL_Y + r as i32 * 12;
            if self.threaded {
                let n = bitmap.len();
                self.rec.extend_from_slice(&[OP_LOAD << 24, LABEL_X as u32, ly as u32, LABEL_HW as u32, LABEL_H as u32]);
                let mut k = 0;
                while k < n {
                    let lo = bitmap[k] as u32;
                    let hi = if k + 1 < n { bitmap[k + 1] as u32 } else { 0 };
                    self.rec.push(lo | hi << 16);
                    k += 2;
                }
            }
            self.load_with(LABEL_X, ly, LABEL_HW, LABEL_H, |k| bitmap[k as usize]);
            words.extend_from_slice(&quad(0x2D, 211, 177, 80, LABEL_H, 0, (r as i32 * 12) as u32, CLUT_TEXT, TPAGE_LABEL));
        }
        self.execute_words(&words);
    }
}
