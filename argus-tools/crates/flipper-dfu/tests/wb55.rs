//! The STM32WB55 layer and the recovery flows on the fake bootloader, on their own (the comparison
//! with the C++ app is tests/qt_wb55.rs). No real device: `Dfu` only ever gets the fake.

mod common;

use common::fake::{flipper_option_bytes, FakeDfu, Faults, FLASH};
use flipper_dfu::device::Dfu;
use flipper_dfu::recovery::{self, OptionBytesOutcome, WirelessStatus};
use flipper_dfu::wb55::{self, Access, OptionBytes, OptionBytesFile, Wb55};

const OB_DATA: &[u8] = b"RDP:0xAA:r\nBOR_LEV:0x4:rw\nnBOOT0:0x1:rw\nIWGDSTDBY:0x1:rw\nSFSA:0xD7:r\nSBRV:0x35C00:r\n";

fn dfu(faults: Faults) -> Dfu<FakeDfu> {
    Dfu::new(FakeDfu::new(faults))
}

#[test]
fn option_bytes_files() {
    let f = OptionBytesFile::parse(OB_DATA).unwrap();
    assert_eq!(f.entries.len(), 6);
    assert_eq!((f.entries[0].0.name, f.entries[0].1, f.entries[0].2), ("RDP", 0xAA, Access::Read));
    // the misspelling is the same field
    assert_eq!(f.entries[3].0.name, "IWDGSTDBY");
    assert_eq!(f.entries[5].1, 0x35C00);
    assert!(OptionBytesFile::parse(b"").unwrap().entries.is_empty());
    assert!(OptionBytesFile::parse(b"nBOOT0:0x1:rw\r\n").is_ok());
    for bad in [
        &b"RDP:0x1CC:r\n"[..],
        b"RDP:0xAA:rw\n",
        b"nBOOT0:0x2:rw\n",
        b"nBOOT0:0x1:RW\n",
        b"nBOOT0:0x1\n",
        b"#nBOOT0:0x1:rw\n",
        b"\n",
        b"nBOOT0:0x1:rw\nnBOOT0:0x1:rw\n",
        b"IWGDSTDBY:0x1:rw\nIWDGSTDBY:0x1:rw\n",
        b"FOO:0x1:rw\n",
        b"nBOOT0:1x:rw\n",
        b"nBOOT0:0x100000000:rw\n",
    ] {
        assert!(OptionBytesFile::parse(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
    }
    assert!(OptionBytesFile::parse(&vec![b'\n'; wb55::MAX_FILE + 1]).is_err());
}

#[test]
fn option_bytes_fields_and_encoding() {
    let raw = flipper_option_bytes();
    let ob = OptionBytes::from_bytes(&raw).unwrap();
    assert_eq!(ob.value("RDP").unwrap(), 0xAA);
    assert_eq!(ob.value("SFSA").unwrap(), 0xD7);
    assert_eq!(ob.value("SBRV").unwrap(), 0x35C00);
    assert_eq!(ob.value("nBOOT0").unwrap(), 1);
    // the bytes as written keep every bit no field covers (the reserved words among them)
    assert_eq!(&ob.encoded()[56..104], &raw[56..104]);
    assert_eq!(ob.encoded().to_vec(), raw);
    let mut changed = ob.clone();
    assert_eq!(changed.set("RDP", 0xAA), Err(wb55::Error::Rdp));
    assert!(matches!(changed.set("nBOOT0", 2), Err(wb55::Error::TooWide { .. })));
    changed.set("nBOOT0", 0).unwrap();
    let b = changed.encoded();
    // bit 27 of word 0 cleared, set in its complement
    assert_eq!(b[3] & 0x08, 0);
    assert_eq!(b[7] & 0x08, 0x08);
    assert_eq!(b[..3], raw[..3]);
    assert!(OptionBytes::from_bytes(&raw[..127]).is_err());
}

#[test]
fn boot_mode_writes_only_the_boot_bits() {
    let mut d = dfu(Faults::default());
    recovery::set_boot_mode(&mut d, false).unwrap();
    let f = d.into_inner();
    assert!(f.left, "the chip resets after its option bytes are written");
    let before = OptionBytes::from_bytes(&flipper_option_bytes()).unwrap();
    let after = OptionBytes::from_bytes(&f.option_bytes).unwrap();
    for (name, value) in before.fields() {
        let want = if name == "nBOOT0" || name == "nSWBOOT0" { 0 } else { value };
        assert_eq!(after.value(name).unwrap(), want, "{name}");
    }
    assert_eq!(&f.option_bytes[8..], &flipper_option_bytes()[8..]);
}

#[test]
fn option_bytes_from_a_file() {
    // nothing differs: the device just leaves the bootloader
    let mut d = dfu(Faults::default());
    assert_eq!(recovery::download_option_bytes(&mut d, OB_DATA).unwrap(), OptionBytesOutcome::Left { differ: vec![] });
    assert!(d.into_inner().left);
    // a read-only field differs: reported, not written
    let mut d = dfu(Faults::default());
    let out = recovery::download_option_bytes(&mut d, b"SFSA:0xD0:r\nRDP:0xBB:r\n").unwrap();
    assert_eq!(out, OptionBytesOutcome::Left { differ: vec![("SFSA", 0xD0), ("RDP", 0xBB)] });
    assert_eq!(d.into_inner().option_bytes, flipper_option_bytes());
    // a rw field differs: written
    let mut d = dfu(Faults::default());
    let out = recovery::download_option_bytes(&mut d, b"BOR_LEV:0x3:rw\nSFSA:0xD7:r\n").unwrap();
    assert_eq!(out, OptionBytesOutcome::Written { fixed: vec![("BOR_LEV", 3)], differ: vec![] });
    assert_eq!(OptionBytes::from_bytes(&d.into_inner().option_bytes).unwrap().value("BOR_LEV").unwrap(), 3);
    // option bytes that cannot be read are never "corrected"
    let mut d = dfu(Faults { empty_upload: true, ..Faults::default() });
    assert!(recovery::download_option_bytes(&mut d, b"BOR_LEV:0x3:rw\n").is_err());
    assert!(d.into_inner().log.iter().all(|l| !l.starts_with("OUT 1 2 ")));
}

#[test]
fn fus_and_versions() {
    let mut d = dfu(Faults::default());
    assert_eq!(d.version_info().unwrap(), wb55::VersionInfo { fus: "1.2.0".into(), wireless: "1.17.3".into() });
    assert!(recovery::check_wireless_stack(&mut d).unwrap());
    assert_eq!(recovery::wireless_status(&mut d).unwrap(), WirelessStatus::FusRunning);
    assert_eq!(d.otp(8).unwrap(), [0, 1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(d.otp(5000).unwrap().len(), 1024);
    let mut d = dfu(Faults { fus: Some([0xFF, 0xFE]), ..Faults::default() });
    assert_eq!(recovery::wireless_status(&mut d).unwrap(), WirelessStatus::WirelessStackRunning);
    recovery::start_fus(&mut d).unwrap();
    let mut d = dfu(Faults { fus: Some([0x1F, 0x00]), ..Faults::default() });
    assert_eq!(d.fus_state().unwrap(), wb55::FusState { status: 0x10, error: 0 });
    assert!(matches!(recovery::start_fus(&mut d), Err(recovery::Error::UnexpectedFusState(_))));
    let mut d = dfu(Faults { table_address: Some(0x1000), ..Faults::default() });
    assert!(matches!(d.version_info(), Err(wb55::Wb55Error::BadTableAddress(0x1000))));
    recovery::delete_wireless_stack(&mut d).unwrap();
    recovery::upgrade_wireless_stack(&mut d).unwrap();
    recovery::start_wireless_stack(&mut d).unwrap();
    assert_eq!(d.into_inner().fus_commands, [0x52, 0x53, 0x5A]);
}

#[test]
fn wireless_stack_address() {
    // right below SFSA (0xD7 pages), rounded down to a page
    assert_eq!(recovery::wireless_stack_address(FLASH, 0xD7, 3 * 4096 + 7).unwrap(), 0x080D_3000);
    assert_eq!(recovery::wireless_stack_address(FLASH, 0xD7, 4096).unwrap(), 0x080D_6000);
    assert_eq!(recovery::wireless_stack_address(FLASH, 0xD7, 0xD7 * 4096).unwrap(), FLASH);
    // the C++ app wrapped these around
    assert!(recovery::wireless_stack_address(FLASH, 0xD7, 0xD7 * 4096 + 1).is_err());
    assert!(recovery::wireless_stack_address(FLASH, 0, 1).is_err());
    assert!(recovery::wireless_stack_address(0xFFFF_0000, 0xFF, 1).is_err());

    let mut rng = common::Rng(9);
    let image = rng.bytes(2 * 4096 + 1);
    let mut d = dfu(Faults::default());
    let at = recovery::download_wireless_stack(&mut d, &image, None).unwrap();
    assert_eq!(at, 0x080D_4000);
    let f = d.into_inner();
    assert_eq!(f.erased_pages, [0x080D_4000, 0x080D_5000, 0x080D_6000]);
    assert_eq!(&f.flash[0xD_4000..0xD_4000 + image.len()], &image[..]);
    let mut d = dfu(Faults::default());
    assert_eq!(recovery::download_wireless_stack(&mut d, &[], None), Err(recovery::Error::EmptyImage));
    assert!(d.into_inner().log.is_empty());
}
