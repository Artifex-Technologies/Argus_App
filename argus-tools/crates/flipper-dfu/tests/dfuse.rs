//! The DfuSe parser and the memory layout on their own: well-formed files read back exactly, every
//! refusal, and property tests (any bytes, and any damage to a good file, never panic, never read
//! past the input, and what is accepted keeps the rules).

mod common;

use std::time::Instant;

use common::{file, file_with, fix_crc, fix_image_size, Rng, T};
use flipper_dfu::dfuse::{self, crc32, Error, MAX_FILE};
use flipper_dfu::layout::{self, MemoryLayout, PageBank};
use proptest::prelude::*;

fn firmware(rng: &mut Rng) -> (Vec<u8>, Vec<u8>) {
    let data = rng.bytes(1 << 20);
    let f = file(&[T::new(0, vec![(0x0800_0000, data.clone())])]);
    (f, data)
}

#[test]
fn crc_is_the_dfu_suffix_crc() {
    // CRC-32 of "123456789" is 0xCBF43926 after the final inversion, which DFU leaves out
    assert_eq!(crc32(b"123456789"), !0xCBF4_3926u32);
}

#[test]
fn a_firmware_file_reads_back_exactly() {
    let mut rng = Rng(7);
    let (f, data) = firmware(&mut rng);
    let parsed = dfuse::parse(&f).unwrap();
    assert_eq!(parsed.version, 1);
    assert_eq!((parsed.suffix.id_vendor, parsed.suffix.id_product, parsed.suffix.bcd_dfu), (0x0483, 0xdf11, 0x011a));
    assert_eq!(parsed.targets.len(), 1);
    let t = &parsed.targets[0];
    assert_eq!((t.alt, t.named, t.name_bytes(), t.size as usize), (0, 1, &b"ST..."[..], data.len() + 8));
    assert_eq!(t.elements.len(), 1);
    assert_eq!(t.elements[0].address, 0x0800_0000);
    assert_eq!(t.elements[0].data, data);
}

#[test]
fn several_targets_and_touching_elements() {
    let a = T::new(0, vec![(0x0800_0000, vec![1; 10]), (0x0800_000A, vec![2; 6]), (0x0800_1000, vec![3])]);
    let b = T::new(1, vec![(0x0800_0000, vec![4; 16])]); // same addresses, other alternate setting: fine
    let parsed = dfuse::parse(&file(&[a, b])).unwrap();
    assert_eq!(parsed.targets.iter().map(|t| t.elements.len()).collect::<Vec<_>>(), [3, 1]);
    assert_eq!(parsed.targets[1].elements[0].end(), 0x0800_0010);
    // the last byte of the address space
    assert!(dfuse::parse(&file(&[T::new(0, vec![(0xFFFF_FFF0, vec![0; 16])])])).is_ok());
}

#[test]
fn every_refusal() {
    let good = || T::new(0, vec![(0x0800_0000, vec![0xAA; 64])]);
    let check = |f: Vec<u8>, want: fn(&Error) -> bool| {
        let e = dfuse::parse(&f).unwrap_err();
        assert!(want(&e), "{e:?}");
    };
    check(vec![0; 26], |e| matches!(e, Error::TooShort));
    check(vec![0; MAX_FILE + 1], |e| matches!(e, Error::TooLarge(_)));
    let mut f = file(&[good()]);
    f[0] = b'd';
    check(f, |e| matches!(e, Error::Signature));
    check(file_with(&[good()], 2, None, &[]), |e| matches!(e, Error::Version(2)));
    let mut f = file(&[good()]);
    f.insert(20, 0);
    check(f, |e| matches!(e, Error::ImageSize { .. }));
    check(file_with(&[], 1, None, &[]), |e| matches!(e, Error::NoTargets));
    check(file_with(&[good()], 1, Some(2), &[]), |e| matches!(e, Error::TargetCut(1)));
    check(file_with(&[good()], 1, None, b"junk"), |e| matches!(e, Error::TrailingBytes(4)));
    let mut t = good();
    t.count = Some(u32::MAX);
    check(file(&[t]), |e| matches!(e, Error::TooManyElements { .. }));
    let mut t = good();
    t.count = Some(2);
    check(file(&[t]), |e| matches!(e, Error::ElementCut { .. }));
    let mut t = good();
    t.size = Some(5);
    check(file(&[t]), |e| matches!(e, Error::TargetSize { prefix: 5, actual: 72, .. }));
    check(file(&[T::new(0, vec![(0x0800_0000, vec![])])]), |e| matches!(e, Error::EmptyElement { .. }));
    check(file(&[T::new(0, vec![(0xFFFF_FFF0, vec![0; 17])])]), |e| matches!(e, Error::AddressOverflow { .. }));
    check(file(&[T::new(0, vec![(0x0800_0000, vec![0; 16]), (0x0800_000F, vec![0; 16])])]), |e| {
        matches!(e, Error::Overlap { alt: 0, first: 0x0800_0000, second: 0x0800_000F })
    });
    // overlap across two targets for the same alternate setting, in reverse order
    check(file(&[T::new(3, vec![(0x0800_0100, vec![0; 16])]), T::new(3, vec![(0x0800_0000, vec![0; 0x101])])]), |e| {
        matches!(e, Error::Overlap { alt: 3, .. })
    });
    let mut f = file(&[good()]);
    let n = f.len();
    f[n - 8] = b'X';
    fix_crc(&mut f);
    check(f, |e| matches!(e, Error::Suffix));
    let mut f = file(&[good()]);
    let n = f.len();
    f[n - 5] = 15;
    fix_crc(&mut f);
    check(f, |e| matches!(e, Error::Suffix));
    let mut f = file(&[good()]);
    f[300] ^= 1;
    check(f, |e| matches!(e, Error::Crc { .. }));
}

/// A 300-byte file claiming four billion elements, or one huge element, costs nothing (the C++ parser
/// used to append empty elements until memory ran out).
#[test]
fn huge_counts_cost_nothing() {
    let start = Instant::now();
    let mut t = T::new(0, vec![(0x0800_0000, vec![1; 8])]);
    t.count = Some(u32::MAX);
    assert!(dfuse::parse(&file(&[t])).is_err());
    let mut f = file(&[T::new(0, vec![(0x0800_0000, vec![1; 8])])]);
    f[11 + 274 + 4..11 + 274 + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    fix_crc(&mut f);
    assert!(matches!(dfuse::parse(&f), Err(Error::ElementCut { .. })));
    let mut f = file_with(&[T::new(0, vec![(0x0800_0000, vec![1; 8])])], 1, Some(255), &[]);
    fix_image_size(&mut f);
    assert!(dfuse::parse(&f).is_err());
    assert!(start.elapsed().as_secs() < 2);
}

#[test]
fn stm32wb55_layouts() {
    let flash = MemoryLayout::parse(b"@Internal Flash   /0x08000000/256*04Kg").unwrap();
    assert_eq!(flash.name, b"@Internal Flash");
    assert_eq!(flash.address, 0x0800_0000);
    assert_eq!(flash.banks, [PageBank { page_count: 256, page_size: 4096, kind: b'g' }]);
    assert_eq!(flash.end(), 0x0810_0000);
    let ob = MemoryLayout::parse(b"@Option Bytes   /0x1FFF8000/01*128 e").unwrap();
    assert_eq!(ob.banks, [PageBank { page_count: 1, page_size: 128, kind: b'e' }]);
    let otp = MemoryLayout::parse(b"@OTP Memory   /0x1FFF7000/01*1Ke").unwrap();
    assert_eq!(otp.banks[0].page_size, 1024);
    assert_eq!(MemoryLayout::parse(b"x/0x0/2*1Mg").unwrap().banks[0].page_size, 1 << 20);
}

#[test]
fn page_addresses() {
    let flash = MemoryLayout::parse(b"@Internal Flash   /0x08000000/256*04Kg").unwrap();
    // one page, from its start
    assert_eq!(flash.page_addresses(0x0800_0000, 0x0800_0040).unwrap(), [0x0800_0000]);
    // mid-page start: that address (the device erases its page), then each later page
    assert_eq!(flash.page_addresses(0x0800_1800, 0x0800_3001).unwrap(), [0x0800_1800, 0x0800_2000, 0x0800_3000]);
    // ends exactly on a boundary: the page there is not erased
    assert_eq!(flash.page_addresses(0x0800_0000, 0x0800_2000).unwrap(), [0x0800_0000, 0x0800_1000]);
    // the whole flash
    assert_eq!(flash.page_addresses(0x0800_0000, 0x0810_0000).unwrap().len(), 256);
    // across banks of different sizes
    let mixed = MemoryLayout::parse(b"x/0x08000000/2*1Kg,4*2Kg").unwrap();
    assert_eq!(mixed.page_addresses(0x0800_0400, 0x0800_2800).unwrap(), [0x0800_0400, 0x0800_0800, 0x0800_1000, 0x0800_1800, 0x0800_2000]);
    // empty, before, after
    for (s, e) in [(0x0800_0000, 0x0800_0000), (0x07FF_FFFF, 0x0800_0010), (0x080F_FFFF, 0x0810_0001), (0x0800_0010, 0x0800_0000)] {
        assert!(matches!(flash.page_addresses(s, e), Err(layout::Error::Range { .. })), "{s:#x}..{e:#x}");
    }
    // up to the very end of the address space
    let top = MemoryLayout::parse(b"x/0xFFFF0000/16*4Kg").unwrap();
    assert_eq!(top.page_addresses(0xFFFF_F000, 1 << 32).unwrap(), [0xFFFF_F000]);
}

#[test]
fn bad_layouts() {
    for d in [
        &b""[..],
        b"x/0x08000000",
        b"x/0x08000000/1*4Kg/0x1/1*1Ka",
        b"x/08000000/1*4Kg",
        b"x/0x/1*4Kg",
        b"x/0x123456789/1*4Kg",
        b"x/0x0800 0000/1*4Kg",
        b"x/0x0/1*K",
        b"x/0x0/1*Kg",
        b"x/0x0/*4Kg",
        b"x/0x0/1*4kg",
        b"x/0x0/1*4Kh",
        b"x/0x0/0*4Kg",
        b"x/0x0/1*0Kg",
        b"x/0x0/1*4Kg,",
        b"x/0x0/11111111111*1 a",
        b"x/0x0/4294967296*1 a",
        b"x/0x0/2*4095Mg",
        b"x/0xFFFF0000/17*4Kg",
        b"\xc3\xa9/0x0/1*1Ka",
    ] {
        assert!(MemoryLayout::parse(d).is_err(), "{:?}", String::from_utf8_lossy(d));
    }
    // a bank of four billion one-byte pages is fine to describe, and cheap to use
    let start = Instant::now();
    let big = MemoryLayout::parse(b"x/0x0/4294967295*1 a").unwrap();
    assert_eq!(big.page_addresses(0xFFFF_0000, 0xFFFF_0004).unwrap(), [0xFFFF_0000, 0xFFFF_0001, 0xFFFF_0002, 0xFFFF_0003]);
    assert!(start.elapsed().as_secs() < 2);
}

fn check_accepted(f: &dfuse::DfuseFile, len: usize) {
    let total: usize = f.targets.iter().flat_map(|t| &t.elements).map(|e| e.data.len() + 8).sum();
    assert!(total + 11 + 274 * f.targets.len() + 16 == len);
    for t in &f.targets {
        assert!(!t.elements.is_empty() || t.size == 0);
        for e in &t.elements {
            assert!(!e.data.is_empty() && e.end() <= 1 << 32);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn any_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..2048), dfuse_header in any::<bool>()) {
        let mut data = data;
        if dfuse_header && data.len() > 11 {
            data[..6].copy_from_slice(b"DfuSe\x01");
            fix_image_size(&mut data);
            fix_crc(&mut data);
        }
        if let Ok(f) = dfuse::parse(&data) {
            check_accepted(&f, data.len());
        }
        let _ = MemoryLayout::parse(&data);
    }

    #[test]
    fn damaged_files_never_panic(seed in any::<u64>(), changes in 1usize..6, recrc in any::<bool>()) {
        let mut rng = Rng(seed | 1);
        let t = T::new(0, vec![(0x0800_0000, rng.bytes(40)), (0x0800_1000, rng.bytes(9))]);
        let u = T::new(1, vec![(0x1FFF_8000, rng.bytes(128))]);
        let mut f = file(&[t, u]);
        for _ in 0..changes {
            match rng.below(4) {
                0 => { let at = rng.below(f.len()); f[at] = rng.next() as u8; }
                1 => { let at = rng.below(f.len()); f.truncate(at.max(1)); }
                2 => { let at = rng.below(f.len()); f.insert(at, rng.next() as u8); }
                _ if f.len() > 4 => { let at = rng.below(f.len() - 4); f[at..at + 4].copy_from_slice(&(rng.next() as u32).to_le_bytes()); }
                _ => {}
            }
        }
        if recrc {
            fix_image_size(&mut f);
            fix_crc(&mut f);
        }
        if let Ok(p) = dfuse::parse(&f) {
            check_accepted(&p, f.len());
        }
    }

    #[test]
    fn page_addresses_stay_inside(count in 1u32..40, size in 1u32..5000, start_off in 0u64..200_000, len in 0u64..200_000) {
        let l = MemoryLayout::parse(format!("x/0x08000000/{count}*{size} g").as_bytes()).unwrap();
        let start = 0x0800_0000u64 + start_off;
        let end = start + len;
        match l.page_addresses(start as u32, end) {
            Ok(pages) => {
                prop_assert!(len > 0 && end <= l.end());
                prop_assert_eq!(u64::from(pages[0]), start);
                for w in pages.windows(2) {
                    prop_assert!(w[0] < w[1]);
                    prop_assert_eq!((u64::from(w[1]) - 0x0800_0000) % u64::from(size), 0);
                }
                prop_assert!(pages.iter().all(|&p| u64::from(p) < end));
                // every page the range touches is listed
                let first_page = (start - 0x0800_0000) / u64::from(size);
                let last_page = (end - 1 - 0x0800_0000) / u64::from(size);
                prop_assert_eq!(pages.len() as u64, last_page - first_page + 1);
            }
            Err(_) => prop_assert!(len == 0 || end > l.end()),
        }
    }
}
