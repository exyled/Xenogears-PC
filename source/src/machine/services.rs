// The game's own sound driver, the sound library (libspu) and the picture decoder's library calls, as native services: the
// functions of the program that used to write the sound chip's and the decoder's ports are answered here, on the same
// state (the game's variables in memory, the chip's register file), so that the game's code reaches no device at all.
// Every function below is the one at its address, written out: same arguments, same writes, same results. A service only
// runs while the words of the function are in memory at its address (the overlays share addresses); otherwise the code
// runs as it is.

use super::*;
use crate::mem::STATE_OLD;

const SPU: u32 = 0x1F801C00;

/// (service number, address, the function's first six words)
pub(super) static SERVICES: &[(u16, u32, [u32; 6])] = &[
    (1, 0x8003E900, [0x3C0B8006, 0x256B252C, 0x00007021, 0x00006821, 0x00006021, 0x00005021]),
    (2, 0x8003EB5C, [0x3C038006, 0x8C639554, 0x3C068005, 0x8CC608E4, 0x10600010, 0x00000000]),
    (3, 0x8003F484, [0x3C028005, 0x8C4208E4, 0x00000000, 0xA444018C, 0x00042402, 0x03E00008]),
    (4, 0x8003F5BC, [0x00042100, 0x00063083, 0x3C028005, 0x8C4208E4, 0x00063140, 0x00822021]),
    (5, 0x80040690, [0x3083FFFF, 0x28620003, 0x10400008, 0x00031900, 0x3C028005, 0x8C426400]),
    (6, 0x800405F4, [0x3088FFFF, 0x29020003, 0x14400003, 0x24070048, 0x080101A2, 0x00001021]),
    (10, 0x8004C6DC, [0x27BDFFE0, 0xAFB00010, 0x00808021, 0x3C048006, 0x8C848E18, 0xAFBF0018]),
    (11, 0x8004C970, [0x3C028006, 0x8C428E08, 0x3C038006, 0x94638E20, 0x27BDFFD0, 0xAFB1001C]),
    (12, 0x8004CB3C, [0x3C028006, 0x8C428E58, 0x27BDFFE8, 0x14400003, 0xAFBF0010, 0x0C013482]),
    (13, 0x8004CBFC, [0x3C028006, 0x8C428E08, 0x27BDFFE0, 0xAFB10014, 0x00808821, 0xAFB00010]),
    (14, 0x8004CCA8, [0xAFA40000, 0xAFA50004, 0xAFA60008, 0xAFA7000C, 0x27BDFFE8, 0xAFB00010]),
    (15, 0x8004D028, [0x14C00008, 0x00041040, 0x3C038006, 0x8C638E08, 0x00000000, 0x00431021]),
    (16, 0x8004D070, [0x3C028006, 0x8C428E2C, 0x00000000, 0x10400011, 0x00803021, 0x3C048006]),
    (17, 0x8004D1B0, [0x3C048006, 0x8C848E1C, 0x3C03F0FF, 0x8C820000, 0x3463FFFF, 0x00431024]),
    (18, 0x8004D1DC, [0x3C048006, 0x8C848E1C, 0x3C03F0FF, 0x8C820000, 0x3463FFFF, 0x00431024]),
    (19, 0x8004D364, [0x00801021, 0x04410003, 0x00402821, 0x080134E2, 0x00002821, 0x28A20040]),
    (20, 0x8004D3B4, [0x27BDFFE8, 0xAFB00010, 0x00808021, 0x12000006, 0xAFBF0014, 0x24020001]),
    (21, 0x8004D590, [0x27BDFFE8, 0x24020005, 0x10A20006, 0xAFBF0010, 0x24020006, 0x10A20006]),
    (22, 0x8004D600, [0x27BDFFE8, 0x10800004, 0xAFBF0010, 0x24020003, 0x14820021, 0x24020001]),
    (23, 0x8004D7A8, [0x00041100, 0x3C038006, 0x8C638E08, 0x3442000C, 0x00431021, 0x94430000]),
    (24, 0x8004DD1C, [0x27BDFF98, 0xAFB00058, 0x00808021, 0xAFB20060, 0x00009021, 0x32020100]),
    (25, 0x8004DEF8, [0x8C850000, 0x00000000, 0x2CA60001, 0x14C00003, 0x30A20001, 0x10400006]),
    (26, 0x8004E3C8, [0x27BDFFC8, 0xAFB00018, 0x00808021, 0xAFB5002C, 0x2E02000A, 0xAFBF0030]),
    (27, 0x8004E574, [0x3C028006, 0x8C428E08, 0x00000000, 0xA4440184, 0xA4450186, 0x3C018006]),
    (40, 0x801D47FC, [0x27BDFFE8, 0x00802821, 0x10A00006, 0xAFBF0010, 0x24020001, 0x10A2001C]),
    (41, 0x801D48F8, [0x27BDFFE0, 0xAFB10014, 0x00808821, 0xAFB00010, 0xAFBF0018, 0x0C075287]),
    (42, 0x801D498C, [0x27BDFFE0, 0xAFB10014, 0x00808821, 0xAFB00010, 0xAFBF0018, 0x0C0752AD]),
    (43, 0x801D4A1C, [0x27BDFFE0, 0x3C03801D, 0x8C637824, 0x3C020010, 0xAFBF0018, 0xAFA20010]),
    (44, 0x801D4AB4, [0x27BDFFE0, 0x3C03801D, 0x8C637804, 0x3C020010, 0xAFBF0018, 0xAFA20010]),
    (45, 0x801D4B4C, [0x3C02801D, 0x8C427824, 0x00000000, 0x8C420000, 0x03E00008, 0x00000000]),
    (28, 0x8004D988, [0x27BDFFF0, 0x00003021, 0x8C890000, 0x00000000, 0x2D2A0001, 0x15400006]),
    (50, 0x8004B8D8, [0x27BDFFE8, 0xAFB00010, 0x3C108005, 0x261078A4, 0xAFBF0014, 0x96020000]),
    (51, 0x8004BB9C, [0x27BDFFD8, 0xAFB10014, 0x00808821, 0xAFB20018, 0x00A09021, 0x3C058005]),
    (52, 0x8004BF20, [0x27BDFFE8, 0x3C048006, 0x24848940, 0x3C038006, 0x8C638964, 0x24020107]),
    (53, 0x8004C048, [0x27BDFFE8, 0x3C048006, 0x2484896C, 0xAFBF0010, 0x0C0130B1, 0x24050008]),
    (54, 0x8004293C, [0x27BDFFD0, 0xAFB20020, 0x00809021, 0x2404FFFF, 0xAFBF0028, 0xAFB30024]),
    (55, 0x800406FC, [0x3084FFFF, 0x00042080, 0x3C058005, 0x8CA563FC, 0x3C018005, 0x00240821]),
    (56, 0x80037EE4, [0x00002821, 0x34061FDF, 0x3C048005, 0x8C8408E4, 0x3C028006, 0x9442957C]),
    (57, 0x800406C8, [0x3082FFFF, 0x00022080, 0x3C058005, 0x8CA563FC, 0x3C018005, 0x00240821]),
];

impl Machine {
    #[inline] fn g32(&self, a: u32) -> u32 { self.mem.read32(a) }
    #[inline] fn s32(&mut self, a: u32, v: u32) { self.mem.write32(a, v); }
    #[inline] fn g16(&self, a: u32) -> u32 { self.mem.read16(a) as u32 }
    #[inline] fn s16(&mut self, a: u32, v: u32) { self.mem.write16(a, v as u16); }
    #[inline] fn sp_r(&mut self, off: u32) -> u32 { self.spu.read(SPU.wrapping_add(off), 2) & 0xFFFF }
    #[inline] fn sp_w(&mut self, off: u32, v: u32) { self.io_write(SPU.wrapping_add(off), v & 0xFFFF, 2); }

    /// Is the service's function the code that is in memory at its address?
    fn service_code_present(&self, at: u32) -> bool {
        match SERVICES.iter().find(|s| s.1 == at) {
            Some(s) => (0..6).all(|k| self.mem.read32(at.wrapping_add(4 * k as u32)) == s.2[k]),
            None => false,
        }
    }

    /// Runs a decompiled function (xgdec) in place of the code at the current pc. false: the code in memory is not the one
    /// it was made from.
    fn run_decompiled(&mut self, index: usize) -> bool {
        if self.ref_run { return false; }
        // several modules can have a function at this address: the one whose first words are in memory runs
        let mut found = None;
        let candidates: Vec<usize> = match self.dec_by_addr.get(&self.pc) { Some(v) => v.clone(), None => vec![index] };
        for ix in candidates {
            if let Some((at, sig, f)) = xgdec::entry(ix) {
                if self.pc != at { continue; }
                let mut same = true;
                for k in 0..6 {
                    if self.mem.read32(at.wrapping_add(4 * k as u32)) != sig[k] { same = false; break; }
                }
                if same { found = Some((ix, at, f)); break; }
            }
        }
        let (index, at, f) = match found { Some(x) => x, None => return false };
        if self.dec_check > 0 && (!self.in_check || NESTED.load(std::sync::atomic::Ordering::Relaxed)) {
            let seen = self.dec_seen.entry(index).or_insert(0);
            if *seen < self.dec_check {
                *seen += 1;
                return self.run_decompiled_checked(index, at, f);
            }
        }
        let mut g = xgdec::G {
            r: xgdec::Regs(self.r.as_mut_ptr()),
            mem: self.mem.w.as_mut_ptr(),
            flag: self.mem.flag.as_ptr(),
            gte: &mut self.gte,
            sp: self.r[29],
            m: self as *mut Machine as *mut u8,
            irq: &self.irq_pending,
            fuel: &mut self.guest_fuel, hi: &mut self.hi, lo: &mut self.lo,
            ops: if self.dec_check > 0 { &DEC_OPS_CHK } else { &DEC_OPS },
        };
        let (ra, a0, a1, a2, a3) = (self.r[31], self.r[4], self.r[5], self.r[6], self.r[7]);
        let saved_in = self.r;
        let v0 = unsafe { f(&mut g, ra, a0, a1, a2, a3) };
        if SAVED_CHECK.load(std::sync::atomic::Ordering::Relaxed) {
            // a function must hand the saved registers back as it got them
            for r in (16..24).chain([28usize, 30]) {
                if self.r[r] != saved_in[r] && ra != SENTINEL_RA {
                    eprintln!("SAVED REG {} changed by {:08X}: {:08X} -> {:08X}", r, at, saved_in[r], self.r[r]);
                }
            }
        }
        self.r[2] = v0;
        self.r[29] = g.sp;
        true
    }

    fn run_decompiled_checked(&mut self, index: usize, at: u32, f: xgdec::DecFn) -> bool {
        let (ra, a0, a1, a2, a3) = (self.r[31], self.r[4], self.r[5], self.r[6], self.r[7]);
        let sp = self.r[29];
        let _ = index;
        let v0 = self.check_call(at, f, ra, [a0, a1, a2, a3], sp);
        self.r[2] = v0;
        self.r[31] = ra;
        self.r[29] = sp;
        true
    }

    /// Differential test of one call: the decompiled function and the translated code run on the same machine state (the
    /// state is restored in between) and the memory they leave is compared; the translated code's result is kept.
    pub(super) fn check_call(&mut self, at: u32, f: xgdec::DecFn, ra: u32, args: [u32; 4], sp0: u32) -> u32 {
        let (a0, a1, a2, a3) = (args[0], args[1], args[2], args[3]);
        self.r[29] = sp0;
        let from: i64 = std::env::var("XG_DEC_CHECK_FROM").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        let frames_now = self.frames;
        let seen = self.dec_seen_addr.entry(at).or_insert(0);
        if *seen >= self.dec_check.max(1) || frames_now < from || self.dec_trace.is_some() {
            // enough looked at: the decompiled code alone
            if std::env::var("XG_CHK_LOG").is_ok() { eprintln!("UNCHECKED {:08X} frame {}", at, self.frames); }
            let mut g = xgdec::G {
                r: xgdec::Regs(self.r.as_mut_ptr()), mem: self.mem.w.as_mut_ptr(), flag: self.mem.flag.as_ptr(), gte: &mut self.gte,
                sp: sp0, m: self as *mut Machine as *mut u8, irq: &self.irq_pending, fuel: &mut self.guest_fuel, hi: &mut self.hi, lo: &mut self.lo, ops: if self.dec_check > 0 { &DEC_OPS_CHK } else { &DEC_OPS },
            };
            let v = unsafe { f(&mut g, ra, a0, a1, a2, a3) };
            self.r[29] = g.sp;
            return v;
        }
        *seen += 1;
        // no interrupt comes in the middle of a checked call (the two runs would see them at different instructions)
        let in_irq0 = self.in_irq;
        self.in_irq = true;
        let was_in_check = self.in_check;
        let was_ref = self.ref_run;
        self.in_check = true;
        let v = self.check_call_inner(at, f, ra, args, sp0);
        self.in_check = was_in_check;
        self.ref_run = was_ref;
        self.in_irq = in_irq0;
        v
    }

    fn check_call_inner(&mut self, at: u32, f: xgdec::DecFn, ra: u32, args: [u32; 4], sp0: u32) -> u32 {
        let (a0, a1, a2, a3) = (args[0], args[1], args[2], args[3]);
        let keep_pc = (self.pc, self.npc, self.cur_pc);
        let snap = self.save_state();
        let ram_pre: Vec<u32> = self.mem.w.to_vec();
        let frames0 = self.frames;
        // the decompiled version
        let mut g = xgdec::G {
            r: xgdec::Regs(self.r.as_mut_ptr()),
            mem: self.mem.w.as_mut_ptr(),
            flag: self.mem.flag.as_ptr(),
            gte: &mut self.gte,
            sp: self.r[29],
            m: self as *mut Machine as *mut u8,
            irq: &self.irq_pending,
            fuel: &mut self.guest_fuel, hi: &mut self.hi, lo: &mut self.lo,
            ops: if self.dec_check > 0 { &DEC_OPS_CHK } else { &DEC_OPS },
        };
        let want_trace = std::env::var("XG_DEC_TRACE").ok().and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()) == Some(at);
        if want_trace { self.dec_trace = Some(Vec::new()); }
        let fuel0 = self.guest_fuel;
        let regs_in = self.r;
        let v0_dec = unsafe { f(&mut g, ra, a0, a1, a2, a3) };
        let cost_dec = fuel0 as i64 - self.guest_fuel as i64;
        let regs_dec = self.r;
        let gte_dec = (self.gte.d, self.gte.c);
        let scratch_dec = self.mem.scratch;
        let hilo_dec = (self.hi, self.lo);
        let sp_dec = g.sp;
        let dtrace = self.dec_trace.take();
        if self.frames != frames0 {
            // the call waited for a vertical blank: the host went on, so there is nothing to put the translated code against
            self.r[29] = sp_dec;
            return v0_dec;
        }
        let ram_dec: Vec<u32> = self.mem.w.to_vec();
        let state_dec = self.mem.state.clone();
        let nat_dec = (self.mem.ents.clone(), self.mem.objs.clone(), self.mem.obj_next, self.mem.script[..self.mem.script_len].to_vec());
        // back, and the translated code
        if !self.load_state(&snap) {
            self.error = Some("differential test: the state could not be restored".to_string());
            return v0_dec;
        }
        self.in_irq = true;
        let idx = ((at & 0x1FFFFF) >> 2) as usize;
        let hooked = self.hook[idx];
        let was_slow = self.slow[idx];
        self.hook[idx] = H::None;
        self.slow[idx] = false;
        self.native_dirty = true;
        if let Some(t) = &dtrace {
            self.xtrace_set = Some(t.iter().map(|x| x.0).collect());
            self.xtrace_out.clear();
            self.counting = true;
            self.native_on = false;
        }
        self.ref_run = true;
        let steps_t0 = self.steps;
        let ok = self.call(at, a0, a1, a2, a3);
        let cost_tr = (self.steps - steps_t0).max(0);
        if std::env::var("XG_CHK_LOG").is_ok() { eprintln!("CHK {:08X} ok {} pc {:08X} frames {} r31 {:08X} sp {:08X}->{:08X}", at, ok, self.pc, self.frames, self.r[31], sp0, self.r[29]); }
        if std::env::var("XG_DEC_REGS").is_ok() {
            if gte_dec.0 != self.gte.d { let k = (0..32).find(|&k| gte_dec.0[k] != self.gte.d[k]).unwrap(); eprintln!("GTE MISMATCH at {:08X}: data {} translated {:08X} decompiled {:08X}", at, k, self.gte.d[k], gte_dec.0[k]); }
            if gte_dec.1 != self.gte.c { let k = (0..32).find(|&k| gte_dec.1[k] != self.gte.c[k]).unwrap(); eprintln!("GTE MISMATCH at {:08X}: control {} translated {:08X} decompiled {:08X}", at, k, self.gte.c[k], gte_dec.1[k]); }
            if scratch_dec != self.mem.scratch { let k = (0..0x400).find(|&k| scratch_dec[k] != self.mem.scratch[k]).unwrap(); eprintln!("SCRATCH MISMATCH at {:08X}: byte {:03X} translated {:02X} decompiled {:02X}", at, k, self.mem.scratch[k], scratch_dec[k]); }
            if hilo_dec != (self.hi, self.lo) { eprintln!("HILO MISMATCH at {:08X}: translated {:08X}/{:08X} decompiled {:08X}/{:08X}", at, self.hi, self.lo, hilo_dec.0, hilo_dec.1); }
        }
        if std::env::var("XG_DEC_REGS").is_ok() {
            // what the function leaves in the registers is read by whoever called it by address
            let mut bad = Vec::new();
            for r in 1..32 {
                if r == 29 || r == 31 || r == 26 || r == 27 { continue; }
                if regs_dec[r] != self.r[r] { bad.push(format!("r{}: translated {:08X} decompiled {:08X}", r, self.r[r], regs_dec[r])); }
            }
            if !bad.is_empty() { eprintln!("REG MISMATCH at {:08X}: {} [entry r16 {:08X} r17 {:08X} r4 {:08X} r5 {:08X}]", at, bad.join(", "), regs_in[16], regs_in[17], regs_in[4], regs_in[5]); }
        }
        self.ref_run = false;
        if std::env::var("XG_DEC_COST").is_ok() && cost_dec != cost_tr {
            eprintln!("COST MISMATCH at {:08X}: translated {} decompiled {}", at, cost_tr, cost_dec);
        }
        if dtrace.is_some() && std::env::var("XG_DEC_TRACE_DUMP").is_ok() { eprintln!("  translated call: ok {} pc {:08X} steps {} stop {:?} frames {}", ok, self.pc, self.steps, self.stop_reason, self.frames); }
        if dtrace.is_some() { self.counting = false; self.xtrace_set = None; }
        self.hook[idx] = hooked;
        self.slow[idx] = was_slow;
        self.native_dirty = true;
        let v0_orig = self.r[2];
        self.r[29] = sp0;
        if !ok { self.error = Some(format!("differential test: the translated code at {:08X} did not return", at)); }
        if let Some(t) = dtrace {
            let o = std::mem::take(&mut self.xtrace_out);
            // the translated blocks begin at other places than the decompiled ones: walk the two in step
            let mut shown = false;
            let mut k = 0usize;
            for (j, (oa, or)) in o.iter().enumerate() {
                let mut f = None;
                for q in k..t.len().min(k + 96) {
                    if &t[q].0 == oa { f = Some(q); break; }
                }
                let q = match f {
                    Some(q) => q,
                    None => {
                        eprintln!("TRACE: translated block {} at {:08X} is not found after decompiled block {} ({:08X})", j, oa, k, t.get(k).map(|x| x.0).unwrap_or(0));
                        shown = true;
                        break;
                    }
                };
                let (dm, dr) = (t[q].1, &t[q].2);
                let mut bad = Vec::new();
                for r in 1..31 { if dm & (1 << r) != 0 && dr[r] != or[r] { bad.push(format!("r{}: translated {:08X} decompiled {:08X}", r, or[r], dr[r])); } }
                if !bad.is_empty() {
                    eprintln!("TRACE: block {} at {:08X}: {} (previous block {:08X})", q, oa, bad.join(", "), if q > 0 { t[q - 1].0 } else { 0 });
                    if std::env::var("XG_DEC_TRACE_DUMP").is_ok() {
                        eprintln!("  translated({}): {}", j, o.iter().take(j + 6).map(|x| format!("{:08X}", x.0)).collect::<Vec<_>>().join(" "));
                        eprintln!("  decompiled: {}", t.iter().take(q + 4).map(|x| format!("{:08X}", x.0)).collect::<Vec<_>>().join(" "));
                    }
                    shown = true;
                    break;
                }
                k = q + 1;
            }
            if !shown {
                eprintln!("TRACE: {} decompiled blocks, {} translated: no register differs", t.len(), o.len());
                if std::env::var("XG_DEC_TRACE_DUMP").is_ok() {
                    eprintln!("  translated: {}", o.iter().map(|x| format!("{:08X}", x.0)).collect::<Vec<_>>().join(" "));
                    eprintln!("  decompiled: {}", t.iter().take(80).map(|x| format!("{:08X}", x.0)).collect::<Vec<_>>().join(" "));
                }
            }
        }
        // the storage outside the image: the entity table, the objects, the script buffer
        {
            let mut nd: Vec<String> = Vec::new();
            if nat_dec.2 != self.mem.obj_next { nd.push(format!("obj_next translated {} decompiled {}", self.mem.obj_next, nat_dec.2)); }
            if let Some(k) = (0..nat_dec.0.len().min(self.mem.ents.len())).find(|&k| nat_dec.0[k].w != self.mem.ents[k].w) {
                let j = (0..nat_dec.0[k].w.len()).find(|&j| nat_dec.0[k].w[j] != self.mem.ents[k].w[j]).unwrap();
                nd.push(format!("entity {} word {}: translated {:08X} decompiled {:08X}", k, j, self.mem.ents[k].w[j], nat_dec.0[k].w[j]));
            }
            if let Some(k) = (0..nat_dec.1.len().min(self.mem.objs.len())).find(|&k| nat_dec.1[k].w != self.mem.objs[k].w) {
                let j = (0..nat_dec.1[k].w.len()).find(|&j| nat_dec.1[k].w[j] != self.mem.objs[k].w[j]).unwrap();
                nd.push(format!("object {} word {}: translated {:08X} decompiled {:08X}", k, j, self.mem.objs[k].w[j], nat_dec.1[k].w[j]));
            }
            if nat_dec.3[..] != self.mem.script[..self.mem.script_len][..] { nd.push("script buffer differs".to_string()); }
            if let Some(k) = (0..state_dec.len().min(self.mem.state.len())).find(|&k| state_dec[k] != self.mem.state[k]) {
                nd.push(format!("data block {:08X}: translated {:08X} decompiled {:08X}", 0x80000000 + STATE_OLD + 4 * k as u32, self.mem.state[k], state_dec[k]));
            }
            if !nd.is_empty() { eprintln!("NATIVE MISMATCH at {:08X} (frame {}, ra {:08X}, a0 {:08X}): {}", at, self.frames, ra, a0, nd.join("; ")); }
        }
        let mut diffs = Vec::new();
        for (i, (&d, &o)) in ram_dec.iter().zip(self.mem.w.iter()).enumerate() {
            let a = 0x80000000u32 + (i as u32) * 4;
            if (0x801F0000..0x80200000).contains(&a) || (0x8000C000..0x8000F000).contains(&a) { continue; }
            if d != o { diffs.push((a, o, d, ram_pre[i])); if diffs.len() >= 8 { break; } }
        }
        if !diffs.is_empty() || v0_dec != v0_orig || sp_dec != sp0 {
            if std::env::var("XG_DEC_CODE").ok().and_then(|v| u32::from_str_radix(&v, 16).ok()) == Some(at) {
                let w: Vec<String> = (0..48).map(|k| format!("{:08X}", self.mem.read32(at.wrapping_add(4 * k)))).collect();
                eprintln!("  code: {}", w.join(" "));
            }
            eprintln!("DEC MISMATCH at {:08X} (frame {}, ra {:08X}, a0 {:08X} a1 {:08X}): v0 translated {:08X} decompiled {:08X}; sp {:08X} -> {:08X}", at, self.frames, ra, a0, a1, v0_orig, v0_dec, sp0, sp_dec);
            for (a, o, d, p) in diffs { eprintln!("   {:08X}: translated {:08X} decompiled {:08X} (before {:08X})", a, o, d, p); }
        }
        // the translated run left the pc at its return sentinel: the program thread goes on from where it was
        self.pc = keep_pc.0; self.npc = keep_pc.1; self.cur_pc = keep_pc.2;
        v0_orig
    }

    /// Runs the service for the function at the current pc. false: not handled (the code runs as it is).
    pub(super) fn service(&mut self, id: u16) -> bool {
        if id >= 1000 { return self.run_decompiled((id - 1000) as usize); }
        let at = self.pc;
        if !self.service_code_present(at) { return false; }
        let (a0, a1, a2) = (self.r[4], self.r[5], self.r[6]);
        match id {
            // ---- the sound driver (game code): voice parameters into the chip's voice registers ----
            1 => self.drv_update(),
            2 => self.drv_keys(),
            3 => {
                let base = self.g32(0x800508E4).wrapping_sub(SPU);
                self.sp_w(base.wrapping_add(0x18C), a0);
                self.sp_w(base.wrapping_add(0x18E), a0 >> 16);
            }
            4 => {
                let off = a0.wrapping_mul(16).wrapping_add(self.g32(0x800508E4).wrapping_sub(SPU)).wrapping_add(10);
                let cur = self.sp_r(off) & 0xFFC0;
                let v = (cur as i32).wrapping_add(a1 as i16 as i32).wrapping_add((((a2 as i32) >> 2) as i16 as i32).wrapping_mul(0x20));
                self.sp_w(off, v as u32);
            }
            // ---- root counters: the host's timers do not run ----
            5 => self.r[2] = 0,
            6 => self.r[2] = if (a0 & 0xFFFF) < 3 { 1 } else { 0 },
            // ---- libspu ----
            10 => {
                self.spu_init(a0);
                self.r[2] = 0;
            }
            11 => self.spu_write0(a0, a1),
            12 => {
                // the end of a transfer: the control bits are cleared, then the program's own callback (if it set one) runs and
                // returns to the caller of this function
                let c = self.sp_r(0x1AA) & 0xFFCF;
                self.sp_w(0x1AA, c);
                let cb = self.g32(0x80058E40);
                if cb != 0 {
                    self.pc = cb;
                    self.npc = cb.wrapping_add(4);
                    self.return_default = true;
                }
            }
            13 => self.spu_dma_start(a0, a1, a2),
            14 => {
                let r = self.spu_mode(a0, a1, a2);
                self.r[2] = r;
            }
            15 => {
                let sh = self.g32(0x80058E30) & 31;
                let v = if a2 == 0 { a1 } else { a1 >> sh };
                self.sp_w(a0.wrapping_mul(2), v);
            }
            16 => {
                let mut v = a1;
                if self.g32(0x80058E2C) != 0 {
                    let d = self.g32(0x80058E34);
                    if d == 0 { return false; }
                    if v % d != 0 { v = v.wrapping_add(d) & !self.g32(0x80058E38); }
                }
                let u = v >> (self.g32(0x80058E30) & 31);
                if a0 != 0xFFFF_FFFE {
                    if a0 == 0xFFFF_FFFF { v = u & 0xFFFF; } else { self.sp_w(a0.wrapping_mul(2), u); }
                }
                self.r[2] = v;
            }
            17 | 18 => {}
            19 => {
                let mut v = a0 as i32;
                if v < 0 { v = 0; } else if v > 0x3F { v = 0x3F; }
                let c = (self.sp_r(0x1AA) & 0xC0FF) | (((v as u32) & 0x3F) << 8);
                self.sp_w(0x1AA, c);
                self.r[2] = v as u32;
            }
            20 => {
                let c = self.sp_r(0x1AA);
                let v;
                if a0 == 0 {
                    self.s32(0x800589A8, 0);
                    v = c & 0xFF7F;
                } else if a0 != 1 {
                    self.r[2] = self.g32(0x800589A8);
                    return true;
                } else if self.g32(0x800589AC) == 1 || !self.reverb_area_busy(self.g32(0x800589B0)) {
                    v = c | 0x80;
                    self.s32(0x800589A8, 1);
                } else {
                    self.s32(0x800589A8, 0);
                    v = c & 0xFF7F;
                }
                self.sp_w(0x1AA, v);
                self.r[2] = self.g32(0x800589A8);
            }
            21 => {
                let (mut addr, mode) = (a0, a1);
                let (ta, size) = if mode == 5 { (0, 0x20) } else if mode == 6 { addr = addr.wrapping_add(0x800); (0x100, 0x20) } else { (0, 0x40) };
                self.spu_dma_start(addr, ta, size);
                self.r[2] = (self.sp_r(0x1AE) & 0x800 != 0) as u32;
            }
            22 => {
                if a0 == 0 || a0 == 3 {
                    let c = self.sp_r(0x1AA) & 0xFFBF;
                    self.sp_w(0x1AA, c);
                }
                if a0 == 1 || a0 == 3 {
                    let c = self.sp_r(0x1AA) | 0x40;
                    self.sp_w(0x1AA, c);
                }
                self.r[2] = a0;
            }
            23 => {
                let v = self.sp_r((a0 << 4) | 0xC);
                self.mem.write16(a2, v as u16);
                let neg_or_zero = ((v << 16) as i32) < 1;
                let key = self.g32(0x800589A0) & (1u32 << (a0 & 31));
                let out = if key == 0 { if neg_or_zero { 0 } else { 2 } } else if neg_or_zero { 3 } else { 1 };
                self.mem.write32(a1, out);
            }
            24 => {
                let r = self.spu_reverb_type(a0);
                self.r[2] = r;
            }
            25 => self.spu_reverb_regs(a0),
            26 => {
                let r = self.spu_reverb_clear(a0);
                self.r[2] = r;
            }
            27 => {
                self.sp_w(0x184, a0);
                self.sp_w(0x186, a1);
                self.s16(0x800589BC, a0);
                self.s16(0x800589BE, a1);
            }
            28 => self.spu_common_attr(a0),
            // ---- the library's interrupt and callback bookkeeping (the host delivers interrupts itself) ----
            50 => {
                // the library's start: only its "started" flag matters to the rest of it
                if self.g16(0x800578A4) == 0 {
                    self.s16(0x800578A4, 1);
                    self.r[2] = 0x800578A4;
                } else {
                    self.r[2] = 0;
                }
            }
            51 => {
                let slot = 0x800578A8u32.wrapping_add(a0.wrapping_mul(4));
                let old = self.g32(slot);
                if a1 != old && self.g16(0x800578A4) != 0 {
                    let bit = 1u32 << (a0 & 31);
                    if a1 == 0 {
                        self.s32(slot, 0);
                        let v = self.g16(0x800578D4) & !bit & 0xFFFF;
                        self.s16(0x800578D4, v);
                    } else {
                        self.s32(slot, a1);
                        let v = (self.g16(0x800578D4) | bit) & 0xFFFF;
                        self.s16(0x800578D4, v);
                    }
                }
                self.r[2] = old;
            }
            52 => {
                self.s32(0x80058960, 0);
                self.r[2] = 0x8004BFF0;
            }
            53 => self.r[2] = 0x8004C21C,
            54 => {
                // waiting for the disc drive's transfer: it is always done
                let v = self.g32(0x80058960);
                self.s32(0x8005A228, v.wrapping_add(0x3C0));
                self.s32(0x8005A22C, 1);
                self.s32(0x8005A230, 0x80018F30);
                self.r[2] = 0;
            }
            55 => self.r[2] = 1, // StopRCnt-like: clears bits of the interrupt mask, which the host does not use
            56 => {
                // every voice: volume, pitch and the sustain level back to their idle values
                let f = self.g16(0x8005957C) | 0x40;
                self.s16(0x8005957C, f);
                let base = self.g32(0x800508E4).wrapping_sub(SPU);
                for v in 0..24u32 {
                    let reg = base.wrapping_add(v * 16);
                    self.sp_w(reg, 0);
                    let lo = self.sp_r(reg + 8) & 0xFF;
                    self.sp_w(reg + 2, 0);
                    self.sp_w(reg + 4, 0);
                    self.sp_w(reg + 10, 0x1FDF);
                    self.sp_w(reg + 8, lo.wrapping_add(0x7F00));
                }
            }
            57 => self.r[2] = ((a0 & 0xFFFF) < 3) as u32, // StartRCnt-like: sets a bit of the interrupt mask
            // ---- the picture decoder's library (the movie module) ----
            40 => {
                if a0 != 0 && a0 != 1 { return false; }
                self.io_write(0x1F801824, 0x8000_0000, 4);
                self.io_write(0x1F801088, 0, 4);
                self.io_write(0x1F801098, 0, 4);
                self.io_write(0x1F801824, 0x6000_0000, 4);
                if a0 == 0 {
                    self.mdec_in(0x801D76E0, 0x20);
                    self.mdec_in(0x801D7764, 0x20);
                }
            }
            41 => self.mdec_in(a0, a1),
            42 => self.mdec_out(a0, a1),
            43 | 44 => self.r[2] = 0,
            45 => self.r[2] = self.io_read32(0x1F801824),
            _ => return false,
        }
        true
    }

    /// FUN_8003E900: the 24 voices' parameter blocks (pointers at 0x8006252C) into the chip's voice registers.
    fn drv_update(&mut self) {
        let base = self.g32(0x800508E4).wrapping_sub(SPU);
        let mut flags: u32 = 0;
        let (mut u9, mut u10, mut u11) = (0u32, 0u32, 0u32);
        for v in 0..24u32 {
            let p = self.g32(0x8006252C + 4 * v);
            let reg = base.wrapping_add(v * 16);
            if p != 0 {
                let f = self.g16(p + 6);
                if f != 0 {
                    if f & 1 != 0 {
                        let (a, b) = (self.g16(p + 8), self.g16(p + 10));
                        self.sp_w(reg, a);
                        self.sp_w(reg + 2, b);
                    }
                    if f & 4 != 0 {
                        let a = self.g16(p + 0x14);
                        self.sp_w(reg + 4, a);
                    }
                    if f & 8 != 0 {
                        let (a, b) = (self.g32(p + 0x1C) >> 3, self.g32(p + 0x20) >> 3);
                        self.sp_w(reg + 6, a);
                        self.sp_w(reg + 0xE, b);
                    }
                    let b27 = self.mem.read8(p + 0x27) as u32;
                    let b24 = self.mem.read8(p + 0x24) as u32;
                    let b28 = self.mem.read8(p + 0x28) as u32;
                    let b29 = self.mem.read8(p + 0x29) as u32;
                    let b25 = self.mem.read8(p + 0x25) as u32;
                    let b2a = self.mem.read8(p + 0x2A) as u32;
                    let b26 = self.mem.read8(p + 0x26) as u32;
                    let b2b = self.mem.read8(p + 0x2B) as u32;
                    if f & 0x10 != 0 {
                        let cur = self.sp_r(reg + 8) & 0xFF;
                        let v16 = cur.wrapping_add(b27.wrapping_mul(0x100)).wrapping_add((b24 >> 2).wrapping_mul(0u32.wrapping_sub(0x8000)));
                        self.sp_w(reg + 8, v16);
                    }
                    if f & 0x20 != 0 {
                        let cur = self.sp_r(reg + 8);
                        let v16 = (cur & 0xFF0F).wrapping_add(b28.wrapping_mul(0x10));
                        self.sp_w(reg + 8, v16);
                    }
                    if f & 0x40 != 0 {
                        let cur = self.sp_r(reg + 10);
                        let v16 = (cur & 0x3F).wrapping_add(b29.wrapping_mul(0x40)).wrapping_add((b25 >> 1).wrapping_mul(0x4000));
                        self.sp_w(reg + 10, v16);
                    }
                    if f & 0x80 != 0 {
                        let cur = self.sp_r(reg + 10);
                        let v16 = (cur & 0xFFC0).wrapping_add(b2a).wrapping_add((b26 >> 2).wrapping_mul(0x20));
                        self.sp_w(reg + 10, v16);
                    }
                    if f & 0x100 != 0 {
                        let cur = self.sp_r(reg + 8);
                        let v16 = b2b.wrapping_add(cur & 0xFFF0);
                        self.sp_w(reg + 8, v16);
                    }
                    flags |= f & 0x7000;
                    self.mem.write16(p + 6, 0);
                }
                let f2 = self.g16(p + 2);
                u9 |= ((f2 >> 4) & 1) << v;
                u10 |= ((f2 >> 5) & 1) << v;
                u11 |= ((f2 >> 6) & 1) << v;
            }
        }
        if flags != 0 {
            if flags & 0x1000 != 0 {
                self.sp_w(base + 0x190, u9);
                self.sp_w(base + 0x192, u9 >> 16);
            }
            if flags & 0x2000 != 0 {
                self.sp_w(base + 0x194, u10);
                self.sp_w(base + 0x196, u10 >> 16);
            }
            if flags & 0x4000 != 0 {
                self.sp_w(base + 0x198, u11);
                self.sp_w(base + 0x19A, u11 >> 16);
            }
        }
        let k = self.g32(0x800594FC);
        if k != 0 {
            self.sp_w(base + 0x188, k);
            self.sp_w(base + 0x18A, k >> 16);
            self.s32(0x800594FC, 0);
        }
    }

    /// FUN_8003EB5C: the pending key-offs.
    fn drv_keys(&mut self) {
        let base = self.g32(0x800508E4).wrapping_sub(SPU);
        let u3 = self.g32(0x80059554);
        if u3 != 0 {
            for v in 0..24u32 {
                if u3 & (1 << v) != 0 {
                    let off = base + 10 + v * 16;
                    let c = (self.sp_r(off) & 0xFFC0) | 6;
                    self.sp_w(off, c);
                }
            }
        }
        let k = self.g32(0x80059550) | self.g32(0x80059554);
        if k != 0 {
            self.sp_w(base + 0x18C, k);
            self.sp_w(base + 0x18E, k >> 16);
            self.s32(0x80059554, 0);
            self.s32(0x80059550, 0);
        }
    }

    /// FUN_8004D988: master volumes, CD and external input volumes and mixing switches from an attribute block.
    fn spu_common_attr(&mut self, p: u32) {
        let mask = self.g32(p);
        let all = mask == 0;
        let mode_base = |n: u32| -> u32 { match n & 0xFFFF { 1 => 0x8000, 2 => 0x9000, 3 => 0xA000, 4 => 0xB000, 5 => 0xC000, 6 => 0xD000, 7 => 0xE000, _ => 0 } };
        let clamp = |v: u32| -> u32 {
            let s = v as u16 as i16;
            (if s < 0 { 0 } else if s >= 0x80 { 0x7F } else { s }) as u32
        };
        let (mut u3, mut u4);
        // left master volume
        if all || mask & 1 != 0 {
            let with_mode = all || mask & 4 != 0;
            let m = if with_mode { mode_base(self.g16(p + 8)) } else { 0 };
            u3 = if m != 0 { clamp(self.g16(p + 4)) } else { self.g16(p + 4) };
            self.sp_w(0x180, (u3 & 0x7FFF) | m);
        }
        // right master volume
        u4 = 0;
        if all || mask & 2 != 0 {
            let with_mode = all || mask & 8 != 0;
            u3 = if with_mode { mode_base(self.g16(p + 10)) } else { 0 };
            if u3 == 0 { u4 = self.g16(p + 6); } else { u4 = clamp(self.g16(p + 6)); }
            self.sp_w(0x182, (u4 & 0x7FFF) | u3);
        }
        if all || mask & 0x40 != 0 { let v = self.g16(p + 0x10); self.sp_w(0x1B0, v); }
        if all || mask & 0x80 != 0 { let v = self.g16(p + 0x12); self.sp_w(0x1B2, v); }
        if all || mask & 0x400 != 0 { let v = self.g16(p + 0x1C); self.sp_w(0x1B4, v); }
        if all || mask & 0x800 != 0 { let v = self.g16(p + 0x1E); self.sp_w(0x1B6, v); }
        for (bitmask, word, bit) in [(0x100u32, 0x14u32, 4u32), (0x200, 0x18, 1), (0x1000, 0x20, 8), (0x2000, 0x24, 2)] {
            if all || mask & bitmask != 0 {
                let c = self.sp_r(0x1AA);
                let v = if self.g32(p + word) == 0 { c & !bit } else { c | bit };
                self.sp_w(0x1AA, v & 0xFFFF);
            }
        }
        let _ = u4;
    }

    /// FUN_8004C6DC (the library's start-up): the chip reset.
    fn spu_init(&mut self, mode: u32) {
        self.s32(0x80058E24, 0);
        self.s32(0x80058E28, 0);
        self.s16(0x80058E20, 0);
        self.sp_w(0x180, 0);
        self.sp_w(0x182, 0);
        self.sp_w(0x1AA, 0);
        self.sp_w(0x180, 0);
        self.sp_w(0x182, 0);
        self.s32(0x80058E2C, 2);
        self.s32(0x80058E30, 3);
        self.s32(0x80058E34, 8);
        self.s32(0x80058E38, 7);
        self.sp_w(0x1AC, 4);
        self.sp_w(0x184, 0);
        self.sp_w(0x186, 0);
        self.sp_w(0x18C, 0xFFFF);
        self.sp_w(0x18E, 0xFFFF);
        self.sp_w(0x198, 0);
        self.sp_w(0x19A, 0);
        for i in 0..10u32 { self.s16(0x8005A4C8 + 2 * i, 0); }
        if mode == 0 {
            self.s16(0x80058E20, 0x200);
            for o in [0x190u32, 0x192, 0x194, 0x196, 0x1B0, 0x1B2, 0x1B4, 0x1B6] { self.sp_w(o, 0); }
            self.spu_write0(0x80058E48, 0x10);
            for v in 0..24u32 {
                let b = v * 16;
                self.sp_w(b, 0);
                self.sp_w(b + 2, 0);
                self.sp_w(b + 4, 0x3FFF);
                self.sp_w(b + 6, 0x200);
                self.sp_w(b + 8, 0);
                self.sp_w(b + 10, 0);
            }
            self.sp_w(0x188, 0xFFFF);
            self.sp_w(0x18A, 0xFF);
            self.sp_w(0x18C, 0xFFFF);
            self.sp_w(0x18E, 0xFF);
        }
        self.s32(0x80058E3C, 1);
        self.sp_w(0x1AA, 0xC000);
        self.s32(0x80058E40, 0);
        self.s32(0x80058E44, 0);
    }

    /// FUN_8004C970: bytes from memory into the chip through its data port, 64 at a time.
    fn spu_write0(&mut self, mut src: u32, mut size: u32) {
        let t = self.g16(0x80058E20);
        self.sp_w(0x1A6, t);
        while size != 0 {
            let n = if size < 0x41 { size } else { 0x40 };
            let mut i = 0;
            while i < n {
                let h = self.g16(src);
                src = src.wrapping_add(2);
                i += 2;
                self.sp_w(0x1A8, h);
            }
            let c = (self.sp_r(0x1AA) & 0xFFCF) | 0x10;
            self.sp_w(0x1AA, c);
            size -= n;
        }
        let c = self.sp_r(0x1AA) & 0xFFCF;
        self.sp_w(0x1AA, c);
    }

    /// FUN_8004CBFC: a block of memory to the chip's memory by DMA.
    fn spu_dma_start(&mut self, addr: u32, ta: u32, size: u32) {
        self.sp_w(0x1A6, ta);
        let c = self.sp_r(0x1AA) | 0x30;
        self.sp_w(0x1AA, c);
        self.io_write(0x1F8010C0, addr, 4);
        self.io_write(0x1F8010C4, (size << 16) | 0x10, 4);
        self.s32(0x80058E58, 1);
        self.io_write(0x1F8010C8, 0x0100_0200, 4);
    }

    /// FUN_8004CCA8: transfer mode, address and start.
    fn spu_mode(&mut self, op: u32, a1: u32, a2: u32) -> u32 {
        match op {
            1 => {
                self.s32(0x80058E58, 0);
                if self.sp_r(0x1A6) != self.g16(0x80058E20) { return 0xFFFF_FFFE; }
                let c = (self.sp_r(0x1AA) & 0xFFCF) | 0x20;
                self.sp_w(0x1AA, c);
            }
            0 => {
                self.s32(0x80058E58, 1);
                if self.sp_r(0x1A6) != self.g16(0x80058E20) { return 0xFFFF_FFFE; }
                let c = self.sp_r(0x1AA) | 0x30;
                self.sp_w(0x1AA, c);
            }
            2 => {
                let v = (a1 >> (self.g32(0x80058E30) & 31)) & 0xFFFF;
                self.s16(0x80058E20, v);
                self.sp_w(0x1A6, v);
            }
            3 => {
                let want = if self.g32(0x80058E58) == 1 { 0x30 } else { 0x20 };
                if (self.sp_r(0x1AA) & 0x30) != want { return 0xFFFF_FFFE; }
                let blocks = (a2 >> 6) + ((a2 & 0x3F) != 0) as u32;
                self.s32(0x80058E60, blocks);
                self.s32(0x80058E5C, a1);
                self.io_write(0x1F8010C0, a1, 4);
                self.io_write(0x1F8010C4, (blocks << 16) | 0x10, 4);
                let chcr = if self.g32(0x80058E58) == 1 { 0x0100_0200 } else { 0x0100_0201 };
                self.io_write(0x1F8010C8, chcr, 4);
            }
            _ => {}
        }
        0
    }

    /// FUN_8004D504: does the shifted address lie inside one of the allocated blocks?
    fn reverb_area_busy(&self, a0: u32) -> bool {
        let a = self.g32(0x80058E30) << (a0 & 31);
        let mut p = self.g32(0x80058E6C);
        if p == 0 { return false; }
        loop {
            let w = self.g32(p);
            if w & 0x8000_0000 != 0 {
                p = p.wrapping_add(8);
                continue;
            }
            if w & 0x4000_0000 != 0 { return false; }
            let s = w & 0x0FFF_FFFF;
            if s >= a { return true; }
            let e = s.wrapping_add(self.g32(p.wrapping_add(4)));
            if a < e { return true; }
            p = p.wrapping_add(8);
        }
    }

    /// FUN_8004DD1C: a reverb type.
    fn spu_reverb_type(&mut self, a0: u32) -> u32 {
        let had = a0 & 0x100 != 0;
        let t = a0 & !0x100;
        if t < 10 {
            let entry = self.g32(0x80058E70 + 4 * t);
            if !self.reverb_area_busy(entry) {
                let src = 0x80058EC0 + t * 0x44;
                self.s32(0x800589B0, entry);
                if t == 7 {
                    self.s32(0x800589C4, 0x7F);
                    self.s32(0x800589C0, 0x7F);
                } else if t == 8 {
                    self.s32(0x800589C4, 0);
                    self.s32(0x800589C0, 0x7F);
                } else {
                    self.s32(0x800589C4, 0);
                    self.s32(0x800589C0, 0);
                }
                let on = (self.sp_r(0x1AA) >> 7) & 1;
                self.s32(0x800589B8, t);
                if on != 0 {
                    let c = self.sp_r(0x1AA) & 0xFF7F;
                    self.sp_w(0x1AA, c);
                }
                self.sp_w(0x184, 0);
                self.sp_w(0x186, 0);
                self.s16(0x800589BC, 0);
                self.s16(0x800589BE, 0);
                self.spu_reverb_regs_all(src);
                if had { let _ = self.spu_reverb_clear(t); }
                let b0 = self.g32(0x800589B0);
                self.sp_w(0xD1 * 2, b0);
                if on != 0 {
                    let c = self.sp_r(0x1AA) | 0x80;
                    self.sp_w(0x1AA, c);
                }
                return 0;
            }
        }
        0xFFFF_FFFF
    }

    /// FUN_8004DEF8: reverb registers from a parameter block (a mask word, then 32 halfwords).
    fn spu_reverb_regs(&mut self, p: u32) {
        let mask = self.g32(p);
        for i in 0..32u32 {
            if mask == 0 || mask & (1u32 << i) != 0 {
                let v = self.g16(p.wrapping_add(4).wrapping_add(2 * i));
                self.sp_w(0x1C0 + 2 * i, v);
            }
        }
    }

    /// the same with the mask taken as zero (every register is written)
    fn spu_reverb_regs_all(&mut self, p: u32) {
        for i in 0..32u32 {
            let v = self.g16(p.wrapping_add(4).wrapping_add(2 * i));
            self.sp_w(0x1C0 + 2 * i, v);
        }
    }

    /// FUN_8004E3C8: clears the reverb work area of a type (chip memory), 1 KiB at a time.
    fn spu_reverb_clear(&mut self, t: u32) -> u32 {
        if t >= 10 { return 0xFFFF_FFFF; }
        let entry = self.g32(0x80058E70 + 4 * t);
        if self.reverb_area_busy(entry) { return 0xFFFF_FFFF; }
        let unit = self.g32(0x80058E30);
        let (mut len, mut start) = if t == 0 {
            (unit << 16, unit << 16)
        } else {
            (unit << (0x10000u32.wrapping_sub(entry) & 31), unit << (entry & 31))
        };
        let saved_transfer = self.g32(0x80058E24);
        let mut restore24 = false;
        if saved_transfer == 1 {
            self.s32(0x80058E24, 0);
            restore24 = true;
        }
        let cb = self.g32(0x80058E40);
        if cb != 0 { self.s32(0x80058E40, 0); }
        loop {
            let (n, last) = if len > 1024 { (1024, false) } else { (len, true) };
            self.spu_mode(2, start, 0);
            self.spu_mode(1, 0, 0);
            self.spu_mode(3, 0x80058A00, n);
            len = len.wrapping_sub(1024);
            start = start.wrapping_add(1024);
            if last { break; }
        }
        if restore24 { self.s32(0x80058E24, saved_transfer); }
        if cb != 0 { self.s32(0x80058E40, cb); }
        0
    }

    /// FUN_801D48F8: a command word and a block into the decoder.
    fn mdec_in(&mut self, buf: u32, size: u32) {
        self.io_write(0x1F801080, buf.wrapping_add(4), 4);
        self.io_write(0x1F801084, ((size >> 5) << 16) | 0x20, 4);
        let cmd = self.g32(buf);
        self.io_write(0x1F801820, cmd, 4);
        self.io_write(0x1F801088, 0x0100_0201, 4);
    }

    /// FUN_801D498C: the decoded macroblocks from the decoder into memory.
    fn mdec_out(&mut self, dst: u32, size: u32) {
        self.io_write(0x1F801098, 0, 4);
        self.io_write(0x1F801090, dst, 4);
        self.io_write(0x1F801094, ((size >> 5) << 16) | 0x20, 4);
        self.io_write(0x1F801098, 0x0100_0200, 4);
    }
}

// what the decompiled code asks of the machine
unsafe fn dec_read32(m: *mut u8, a: u32) -> u32 { (&mut *(m as *mut Machine)).read32(a) }
unsafe fn dec_read16(m: *mut u8, a: u32) -> u16 { (&mut *(m as *mut Machine)).read16(a) }
unsafe fn dec_read8(m: *mut u8, a: u32) -> u8 { (&mut *(m as *mut Machine)).read8(a) }
unsafe fn dec_write32(m: *mut u8, a: u32, v: u32) { (&mut *(m as *mut Machine)).write32(a, v) }
unsafe fn dec_write16(m: *mut u8, a: u32, v: u16) { (&mut *(m as *mut Machine)).write16(a, v) }
unsafe fn dec_write8(m: *mut u8, a: u32, v: u8) { (&mut *(m as *mut Machine)).write8(a, v) }

/// code that is not decompiled (or whose address holds another module's code): the machine runs it and returns v0
/// XG_DEC_NESTED: the calls a checked function makes are checked too (the state is then the translated code's from there on)
pub static NESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub static SAVED_CHECK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const SENTINEL_RA: u32 = 0xFFFF_FFF0;

pub static CALL_LOG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

unsafe fn dec_call(m: *mut u8, addr: u32, _ra: u32, args: [u32; 4], sp: u32) -> u32 {
    if CALL_LOG.load(std::sync::atomic::Ordering::Relaxed) {
        static SEEN: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());
        let mut v = SEEN.lock().unwrap();
        if !v.contains(&addr) { v.push(addr); eprintln!("CALLLOG: translated code runs at {:08X} (from decompiled code), frame {}", addr, (&*(m as *const Machine)).frames); }
    }
    DBG_LAST.store(addr, std::sync::atomic::Ordering::Relaxed);
    DBG_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let me = &mut *(m as *mut Machine);
    if let Ok(mut r) = RING.lock() { let i = r.1 % 64; r.0[i] = (addr, _ra, me.frames as u64, me.mode as u32); r.1 += 1; }
    let keep = (me.pc, me.npc, me.cur_pc, me.hi, me.lo, me.r[31], me.r[29]);
    me.r[29] = sp;
    let steps_before = me.steps;
    let ok = me.call(addr, args[0], args[1], args[2], args[3]);
    // the translated code's instructions are time too
    let used = (me.steps - steps_before).max(0).min(i32::MAX as i64) as i32;
    me.guest_fuel = me.guest_fuel.saturating_sub(used);
    let v0 = me.r[2];
    me.pc = keep.0; me.npc = keep.1; me.cur_pc = keep.2; me.hi = keep.3; me.lo = keep.4; me.r[31] = keep.5;
    if ok { me.r[29] = keep.6.max(0) ; me.r[29] = sp; }
    v0
}

unsafe fn dec_checked(m: *mut u8, at: u32, f: xgdec::DecFn, ra: u32, args: [u32; 4], sp: u32) -> u32 {
    let me = &mut *(m as *mut Machine);
    me.check_call(at, f, ra, args, sp)
}

unsafe fn dec_reloc(m: *mut u8, kind: u32, old: u32) -> u32 {
    let me = &mut *(m as *mut Machine);
    match kind {
        // a new entity table: the objects of the scene before it are gone
        // (the 8 bytes before each relocated block stand for the allocator's header: lock/unlock code writes there)
        0 => { me.mem.obj_reset(); crate::mem::ENT_GUEST + 8 }
        2 => me.mem.script_alloc(),
        _ => me.mem.obj_alloc().unwrap_or(old),
    }
}

unsafe fn dec_poll(m: *mut u8) {
    DBG_POLLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    (&mut *(m as *mut Machine)).deliver_interrupts()
}

unsafe fn dec_trace(m: *mut u8, addr: u32, mask: u32, regs: &[u32; 32]) {
    let me = &mut *(m as *mut Machine);
    if let Some(t) = me.dec_trace.as_mut() { t.push((addr, mask, *regs)); }
}

unsafe fn dec_brk(m: *mut u8, code: u32) {
    let me = &mut *(m as *mut Machine);
    me.stop_reason = Some(format!("break 0x{:X} in decompiled code", code));
}

/// the last 64 calls decompiled code made into code that is not decompiled: (address, return address, frame, mode), and how many so far
pub static RING: std::sync::Mutex<([(u32, u32, u64, u32); 64], usize)> = std::sync::Mutex::new(([(0, 0, 0, 0); 64], 0));
pub static DBG_LAST: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
pub static DBG_POLLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static DBG_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

static DEC_OPS: xgdec::Ops = xgdec::Ops {
    read32: dec_read32, read16: dec_read16, read8: dec_read8,
    write32: dec_write32, write16: dec_write16, write8: dec_write8,
    call: dec_call, brk: dec_brk, poll: dec_poll, reloc: dec_reloc, trace: dec_trace, checked: None,
};

static DEC_OPS_CHK: xgdec::Ops = xgdec::Ops {
    read32: dec_read32, read16: dec_read16, read8: dec_read8,
    write32: dec_write32, write16: dec_write16, write8: dec_write8,
    call: dec_call, brk: dec_brk, poll: dec_poll, reloc: dec_reloc, trace: dec_trace, checked: Some(dec_checked),
};
