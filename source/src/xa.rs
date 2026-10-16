// CD-XA ADPCM audio, as the movies carry it. A 2324-byte sector holds 18 sound groups of 128 bytes: 16 bytes of parameters
// and 112 bytes of samples. The coding byte: bit 0 stereo, bit 2 18.9 kHz (else 37.8), bit 4 8-bit.

static K0: [i32; 4] = [0, 60, 115, 98];
static K1: [i32; 4] = [0, 0, -52, -55];

pub fn is_stereo(coding: u8) -> bool { coding & 1 != 0 }
pub fn sample_rate(coding: u8) -> i32 { if coding & 4 != 0 { 18900 } else { 37800 } }
fn is_8bit(coding: u8) -> bool { coding & 0x10 != 0 }

/// Decodes sectors to 16-bit PCM, interleaved left/right when stereo.
pub fn decode(sectors: &[Vec<u8>], coding: u8) -> Vec<i16> {
    let stereo = is_stereo(coding);
    let eight = is_8bit(coding);
    let units_per_group = if eight { 4 } else { 8 };
    let mut pcm: Vec<i16> = Vec::with_capacity(sectors.len() * 18 * 28 * units_per_group);
    let mut prev = [0i32; 4];
    for sector in sectors {
        for g in 0..18 {
            let gb = g * 128;
            if gb + 128 > sector.len() { break; }
            let mut units = vec![[0i32; 28]; units_per_group];
            for u in 0..units_per_group {
                let param = sector[gb + if eight { u } else if u < 4 { u } else { u + 4 }] as i32;
                let mut shift = param & 0xF;
                let filter = ((param >> 4) & 3) as usize;
                if shift > 12 { shift = 9; }
                let channel = if stereo { u & 1 } else { 0 };
                for i in 0..28 {
                    let word = gb + 16 + i * 4;
                    let mut s: i32;
                    if eight {
                        s = (((sector[word + u] as i8) as i32) << 8) >> shift;
                    } else {
                        let b = sector[word + (u >> 1)] as i32;
                        let nibble = if u & 1 == 0 { b & 0xF } else { b >> 4 };
                        s = ((if nibble >= 8 { nibble - 16 } else { nibble }) << 12) >> shift;
                    }
                    s += (prev[channel * 2] * K0[filter] + prev[channel * 2 + 1] * K1[filter] + 32) >> 6;
                    s = s.clamp(-32768, 32767);
                    prev[channel * 2 + 1] = prev[channel * 2];
                    prev[channel * 2] = s;
                    units[u][i] = s;
                }
            }
            if stereo {
                let mut u = 0;
                while u < units_per_group {
                    for i in 0..28 {
                        pcm.push(units[u][i] as i16);
                        pcm.push(units[u + 1][i] as i16);
                    }
                    u += 2;
                }
            } else {
                for u in 0..units_per_group {
                    for i in 0..28 { pcm.push(units[u][i] as i16); }
                }
            }
        }
    }
    pcm
}
