// The program's own code, translated ahead of time (tools/recomp.py writes gen.rs): each basic block is a Rust function that
// does what the interpreter would do for the same instructions, with the same memory paths and the same step count. The
// blocks live in this crate so that changing the machine does not mean compiling them again; they reach the machine only
// through Ctx: its registers and memory (as pointers), the geometry coprocessor, and a few functions for the slow paths
// (devices, hooks, interrupts) that the machine fills in.

pub mod gte;
pub mod snap;

mod gen;
pub use gen::{BLOCKS, WORDS};

pub const RAM_SIZE: u32 = 0x200000;

/// The main executable's data and bss (0x8004F000..0x8006F000) live outside the image (src/mem.rs `state`): loads from the old
/// addresses are answered by the machine, never from the image words.
pub const STATE_LO: u32 = 0x4F000;
pub const STATE_LEN: u32 = 0x20000;

/// What a block may touch. pc, npc, cur_pc, hi, lo and critical are copied in before a block runs and out after it (and
/// around every slow call); exit is set by the slow calls when the block has to be left (an interrupt became due, the
/// machine stopped, or a write reached a word that blocks stand for).
#[repr(C)]
pub struct Ctx {
    pub r: *mut u32,
    pub w: *mut u32,
    pub flag: *const u64,
    pub gte: *mut gte::Gte,
    pub pc: u32,
    pub npc: u32,
    pub cur_pc: u32,
    pub hi: u32,
    pub lo: u32,
    pub critical: bool,
    pub exit: bool,
    pub m: *mut u8,
    pub f_read32: unsafe fn(&mut Ctx, u32) -> u32,
    pub f_read16: unsafe fn(&mut Ctx, u32) -> u16,
    pub f_read8: unsafe fn(&mut Ctx, u32) -> u8,
    pub f_write32: unsafe fn(&mut Ctx, u32, u32),
    pub f_write16: unsafe fn(&mut Ctx, u32, u16),
    pub f_write8: unsafe fn(&mut Ctx, u32, u8),
}

impl Ctx {
    #[inline(always)] pub unsafe fn read32(&mut self, a: u32) -> u32 { (self.f_read32)(self, a) }
    #[inline(always)] pub unsafe fn read16(&mut self, a: u32) -> u16 { (self.f_read16)(self, a) }
    #[inline(always)] pub unsafe fn read8(&mut self, a: u32) -> u8 { (self.f_read8)(self, a) }
    #[inline(always)] pub unsafe fn write32(&mut self, a: u32, v: u32) { (self.f_write32)(self, a, v) }
    #[inline(always)] pub unsafe fn write16(&mut self, a: u32, v: u16) { (self.f_write16)(self, a, v) }
    #[inline(always)] pub unsafe fn write8(&mut self, a: u32, v: u8) { (self.f_write8)(self, a, v) }
}

pub type BlockFn = unsafe fn(&mut Ctx) -> i32;
