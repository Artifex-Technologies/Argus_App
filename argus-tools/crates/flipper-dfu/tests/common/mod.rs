//! Test helpers: a DfuSe file writer (the tests' own, so the parser is not checked against itself)
//! and a small seeded generator.
#![allow(dead_code)]

use flipper_dfu::dfuse::crc32;

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// One target as written: its alternate setting, name, and elements (address, data). `size` and
/// `count` override what is written in its prefix.
#[derive(Clone)]
pub struct T {
    pub alt: u8,
    pub named: u32,
    pub name: Vec<u8>,
    pub elements: Vec<(u32, Vec<u8>)>,
    pub size: Option<u32>,
    pub count: Option<u32>,
}

impl T {
    pub fn new(alt: u8, elements: Vec<(u32, Vec<u8>)>) -> T {
        T { alt, named: 1, name: b"ST...".to_vec(), elements, size: None, count: None }
    }
    pub fn bytes(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for (a, d) in &self.elements {
            body.extend(a.to_le_bytes());
            body.extend((d.len() as u32).to_le_bytes());
            body.extend(d);
        }
        let mut out = b"Target".to_vec();
        out.push(self.alt);
        out.extend(self.named.to_le_bytes());
        let mut name = self.name.clone();
        name.resize(255, 0);
        out.extend(name);
        out.extend(self.size.unwrap_or(body.len() as u32).to_le_bytes());
        out.extend(self.count.unwrap_or(self.elements.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }
}

/// A whole file with a correct prefix, suffix and CRC.
pub fn file(targets: &[T]) -> Vec<u8> {
    file_with(targets, 1, None, &[])
}

pub fn file_with(targets: &[T], version: u8, count: Option<u8>, junk: &[u8]) -> Vec<u8> {
    let mut body: Vec<u8> = targets.iter().flat_map(T::bytes).collect();
    body.extend(junk);
    let mut out = b"DfuSe".to_vec();
    out.push(version);
    out.extend(((11 + body.len()) as u32).to_le_bytes());
    out.push(count.unwrap_or(targets.len() as u8));
    out.extend(body);
    out.extend(0xffffu16.to_le_bytes());
    out.extend(0xdf11u16.to_le_bytes());
    out.extend(0x0483u16.to_le_bytes());
    out.extend(0x011au16.to_le_bytes());
    out.extend(b"UFD");
    out.push(16);
    out.extend([0; 4]);
    fix_crc(&mut out);
    out
}

/// Rewrites the last four bytes as the CRC of the rest (so a change gets past the CRC check).
pub fn fix_crc(f: &mut [u8]) {
    if f.len() >= 4 {
        let n = f.len() - 4;
        let crc = crc32(&f[..n]);
        f[n..].copy_from_slice(&crc.to_le_bytes());
    }
}

/// The DfuSe prefix's image size, rewritten to match the file's length.
pub fn fix_image_size(f: &mut [u8]) {
    if f.len() >= 27 {
        let n = (f.len() - 16) as u32;
        f[6..10].copy_from_slice(&n.to_le_bytes());
    }
}
