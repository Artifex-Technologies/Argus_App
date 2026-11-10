//! Heatshrink (LZSS), as the Flipper uses it for `resources.ths` in update packages
//! (Argus_Firmware scripts/flipper/assets/tarball.py: a 7-byte header, then the stream).
//!
//! Header: u32 LE magic 0x53445348 ("HSDS"), u8 version 1, u8 window bits, u8 lookahead bits.
//! Stream, MSB first: tag bit 1 + 8 bits = a literal; tag bit 0 + `window` bits (offset - 1) +
//! `lookahead` bits (count - 1) = a back-reference. The window starts zero-filled, as in the C
//! decoder. Written from the format description, not from the C source.

use std::io::{self, Read};

pub const MAGIC: u32 = 0x5344_5348;
pub const HEADER_LEN: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub window_bits: u8,
    pub lookahead_bits: u8,
}

impl Params {
    pub fn valid(&self) -> bool {
        (4..=15).contains(&self.window_bits) && self.lookahead_bits >= 3 && self.lookahead_bits < self.window_bits
    }
}

/// Parse the 7-byte Flipper stream header.
pub fn parse_header(h: &[u8]) -> Option<Params> {
    if h.len() < HEADER_LEN {
        return None;
    }
    let magic = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
    let p = Params { window_bits: h[5], lookahead_bits: h[6] };
    (magic == MAGIC && h[4] == 1 && p.valid()).then_some(p)
}

pub struct Decoder<R: Read> {
    inner: R,
    p: Params,
    window: Vec<u8>,
    pos: usize,
    bitbuf: u32,
    nbits: u32,
    copy_offset: usize,
    copy_left: usize,
    done: bool,
    inbuf: [u8; 4096],
    inpos: usize,
    inlen: usize,
}

impl<R: Read> Decoder<R> {
    pub fn new(inner: R, p: Params) -> Decoder<R> {
        assert!(p.valid());
        Decoder {
            inner,
            p,
            window: vec![0; 1 << p.window_bits],
            pos: 0,
            bitbuf: 0,
            nbits: 0,
            copy_offset: 0,
            copy_left: 0,
            done: false,
            inbuf: [0; 4096],
            inpos: 0,
            inlen: 0,
        }
    }

    fn next_byte(&mut self) -> io::Result<Option<u8>> {
        if self.inpos == self.inlen {
            self.inlen = self.inner.read(&mut self.inbuf)?;
            self.inpos = 0;
            if self.inlen == 0 {
                return Ok(None);
            }
        }
        let b = self.inbuf[self.inpos];
        self.inpos += 1;
        Ok(Some(b))
    }

    fn bits(&mut self, n: u32) -> io::Result<Option<u32>> {
        while self.nbits < n {
            match self.next_byte()? {
                Some(b) => {
                    self.bitbuf = (self.bitbuf << 8) | b as u32;
                    self.nbits += 8;
                }
                None => return Ok(None),
            }
        }
        let v = (self.bitbuf >> (self.nbits - n)) & ((1u32 << n) - 1);
        self.nbits -= n;
        self.bitbuf &= (1u32 << self.nbits).wrapping_sub(1);
        Ok(Some(v))
    }

    fn emit(&mut self, b: u8) {
        let mask = self.window.len() - 1;
        self.window[self.pos & mask] = b;
        self.pos = self.pos.wrapping_add(1);
    }
}

impl<R: Read> Read for Decoder<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut n = 0;
        let mask = self.window.len() - 1;
        while n < out.len() {
            if self.copy_left > 0 {
                let b = self.window[self.pos.wrapping_sub(self.copy_offset) & mask];
                self.emit(b);
                out[n] = b;
                n += 1;
                self.copy_left -= 1;
                continue;
            }
            if self.done {
                break;
            }
            match self.bits(1)? {
                None => self.done = true,
                Some(1) => match self.bits(8)? {
                    Some(b) => {
                        self.emit(b as u8);
                        out[n] = b as u8;
                        n += 1;
                    }
                    None => self.done = true,
                },
                Some(_) => {
                    let idx = self.bits(self.p.window_bits as u32)?;
                    let cnt = self.bits(self.p.lookahead_bits as u32)?;
                    match (idx, cnt) {
                        (Some(i), Some(c)) => {
                            self.copy_offset = i as usize + 1;
                            self.copy_left = c as usize + 1;
                        }
                        _ => self.done = true,
                    }
                }
            }
        }
        Ok(n)
    }
}

/// A plain greedy encoder. Argus never needs to compress for the device; this exists so tests can
/// build `.ths` fixtures (and was cross-checked against the firmware tree's C heatshrink).
pub fn encode(data: &[u8], p: Params) -> Vec<u8> {
    assert!(p.valid());
    let mut out = Vec::new();
    let mut acc: u64 = 0;
    let mut nacc: u32 = 0;
    let mut put = |out: &mut Vec<u8>, v: u32, n: u32| {
        acc = (acc << n) | v as u64;
        nacc += n;
        while nacc >= 8 {
            out.push((acc >> (nacc - 8)) as u8);
            nacc -= 8;
            acc &= (1u64 << nacc) - 1;
        }
    };
    let wsize = 1usize << p.window_bits;
    let maxlen = 1usize << p.lookahead_bits;
    let breakeven = (1 + p.window_bits as usize + p.lookahead_bits as usize) / 9 + 1;
    let mut i = 0;
    while i < data.len() {
        let (mut best_len, mut best_off) = (0, 0);
        let start = i.saturating_sub(wsize);
        for j in (start..i).rev() {
            let mut l = 0;
            while l < maxlen && i + l < data.len() && data[j + l] == data[i + l] {
                l += 1;
            }
            if l > best_len {
                best_len = l;
                best_off = i - j;
                if l == maxlen {
                    break;
                }
            }
        }
        if best_len > breakeven {
            put(&mut out, 0, 1);
            put(&mut out, (best_off - 1) as u32, p.window_bits as u32);
            put(&mut out, (best_len - 1) as u32, p.lookahead_bits as u32);
            i += best_len;
        } else {
            put(&mut out, 1, 1);
            put(&mut out, data[i] as u32, 8);
            i += 1;
        }
    }
    if nacc > 0 {
        out.push((acc << (8 - nacc)) as u8);
    }
    out
}

/// Header + stream, as tarball.py writes `resources.ths`.
pub fn encode_with_header(data: &[u8], p: Params) -> Vec<u8> {
    let mut v = MAGIC.to_le_bytes().to_vec();
    v.extend_from_slice(&[1, p.window_bits, p.lookahead_bits]);
    v.extend(encode(data, p));
    v
}
