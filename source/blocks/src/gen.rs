// The translated blocks (84 MB of generated Rust, 91,000 basic blocks of the program's code) are no longer part of the build:
// the program runs as decompiled functions, and the little that is still machine code (a few callbacks, the reference run for
// the differential check) is interpreted. The last generated file is kept at port/archive/gen.rs.translated-blocks and
// tools/recomp.py writes a new one; put it here to get the blocks back (and expect a build of about 50 minutes).
use crate::{BlockFn, Ctx};

pub static WORDS: [u32; 0] = [];
pub static BLOCKS: [(u32, usize, u32, BlockFn); 0] = [];
