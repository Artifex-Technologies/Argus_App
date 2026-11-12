//! The DfuSe protocol against the fake bootloader (tests/common/fake.rs): a whole firmware file
//! flashed and read back, the option bytes, FUS commands, leaving, and every fault bounded. No test
//! here, or anywhere in this crate, reaches a real USB device: `Dfu` only ever gets the fake.

mod common;

use common::fake::{self, FakeDfu, Faults, FLASH, OPTION_BYTES, PAGE};
use common::{file, Rng, T};
use flipper_dfu::device::{Dfu, Error, MAX_POLLS, MAX_POLL_MS};
use flipper_dfu::dfuse;

fn dfu(faults: Faults) -> Dfu<FakeDfu> {
    Dfu::new(FakeDfu::new(faults))
}

/// Requests that would change memory (data blocks or erase/command downloads), from the log.
fn writes(log: &[String]) -> usize {
    log.iter().filter(|l| l.starts_with("OUT 1 ") && !l.ends_with(" -") && !l.starts_with("OUT 1 0 21")).count()
}

#[test]
fn flashes_a_firmware_file_and_reads_it_back() {
    let mut rng = Rng(42);
    let data = rng.bytes(3 * PAGE + 100);
    let f = dfuse::parse(&file(&[T::new(0, vec![(FLASH + 0x1000, data.clone())])])).unwrap();
    let mut d = dfu(Faults { busy_poll_ms: 25, ..Faults::default() });
    d.download_file(&f).unwrap();
    let back = d.upload(FLASH + 0x1000, data.len(), 0).unwrap();
    assert_eq!(back, data);
    let fake = d.into_inner();
    assert_eq!(fake.erased_pages, [FLASH + 0x1000, FLASH + 0x2000, FLASH + 0x3000, FLASH + 0x4000]);
    assert_eq!(&fake.flash[0x1000..0x1000 + data.len()], &data[..]);
    assert!(fake.flash[..0x1000].iter().all(|&b| b == 0xFF));
    // 1024-byte blocks from block 2, after one set-address
    let blocks: Vec<&String> = fake.log.iter().filter(|l| l.starts_with("OUT 1 ") && !l.starts_with("OUT 1 0 ")).collect();
    assert_eq!(blocks.len(), data.len().div_ceil(1024));
    assert!(blocks[0].starts_with("OUT 1 2 ") && blocks.last().unwrap().starts_with("OUT 1 14 "));
    assert!(fake.log.iter().any(|l| l == "SLEEP 25"));
}

#[test]
fn several_elements_and_targets() {
    let mut rng = Rng(3);
    let a = rng.bytes(10);
    let b = rng.bytes(PAGE);
    let ob = rng.bytes(16);
    // an element for the option bytes is outside flash: alternate setting 0's layout refuses it
    // before anything is erased or written
    let f = dfuse::parse(&file(&[T::new(0, vec![(FLASH, a.clone()), (FLASH + 0x8000, b.clone())]), T::new(1, vec![(OPTION_BYTES, ob)])]))
        .unwrap();
    let mut d = dfu(Faults::default());
    assert!(matches!(d.download_file(&f), Err(Error::Layout(_))));
    let fake = d.into_inner();
    assert_eq!(fake.erased_pages, [FLASH, FLASH + 0x8000]);
    assert!(fake.flash.iter().all(|&x| x == 0xFF));
    // flash only: both written
    let f = dfuse::parse(&file(&[T::new(0, vec![(FLASH, a.clone()), (FLASH + 0x8000, b.clone())])])).unwrap();
    let mut d = dfu(Faults::default());
    d.download_file(&f).unwrap();
    let fake = d.into_inner();
    assert_eq!(&fake.flash[..10], &a[..]);
    assert_eq!(&fake.flash[0x8000..0x9000], &b[..]);
}

#[test]
fn writing_unerased_flash_fails() {
    let mut d = dfu(Faults::default());
    d.download(&[1, 2, 3], FLASH, 0).unwrap();
    let e = d.download(&[4, 5, 6], FLASH, 0).unwrap_err();
    assert!(matches!(e, Error::Device { status: fake::ERR_PROG, .. }), "{e:?}");
    // the next step clears the error and goes on
    d.erase(FLASH, 3).unwrap();
    d.download(&[4, 5, 6], FLASH, 0).unwrap();
    assert_eq!(&d.into_inner().flash[..3], &[4, 5, 6]);
}

#[test]
fn option_bytes_otp_and_short_reads() {
    let mut d = dfu(Faults::default());
    let ob = d.upload(OPTION_BYTES, 128, 1).unwrap();
    assert_eq!(ob.len(), 128);
    assert_eq!(ob[0], 0xAA);
    // past the end: the device answers short, and that is the end
    assert_eq!(d.upload(OPTION_BYTES, 4096, 1).unwrap().len(), 128);
    assert_eq!(d.upload(fake::OTP, 1024, 2).unwrap(), (0..1024).map(|i| i as u8).collect::<Vec<_>>());
    assert_eq!(d.partition_origin(0).unwrap(), FLASH);
    assert_eq!(d.partition_origin(1).unwrap(), OPTION_BYTES);
    // option bytes written in place (no erase on that memory); the chip then resets, which a plain
    // download reports as an error and download_may_reset as done
    let mut new = ob.clone();
    new[3] = 0;
    assert!(matches!(d.download(&new, OPTION_BYTES, 1), Err(Error::Transport { .. })));
    let mut d = dfu(Faults::default());
    d.download_may_reset(&new, OPTION_BYTES, 1).unwrap();
    let fake = d.into_inner();
    assert!(fake.left);
    assert_eq!(fake.option_bytes, new);
    // an error status before the reset is still an error
    let mut d = dfu(Faults { fail_op: Some((2, fake::ERR_WRITE)), ..Faults::default() });
    assert!(matches!(d.download_may_reset(&new, OPTION_BYTES, 1), Err(Error::Device { .. })));
}

#[test]
fn fus_commands_and_leaving() {
    let mut d = dfu(Faults::default());
    d.command(&[0x52]).unwrap();
    d.command(&[0x5A]).unwrap();
    assert_eq!(d.command(&[0x99]), Err(Error::Device { during: "command", status: fake::ERR_TARGET, state: 10 }));
    d.leave().unwrap();
    let fake = d.into_inner();
    assert_eq!(fake.fus_commands, [0x52, 0x5A]);
    assert!(fake.left);
    assert_eq!(fake.log.last().unwrap(), "IN 3 0 6");
}

#[test]
fn starts_from_any_state() {
    for faults in [Faults { start_in_error: true, ..Faults::default() }, Faults { start_dnload_idle: true, ..Faults::default() }] {
        let mut d = dfu(faults);
        d.download(&[1], FLASH, 0).unwrap();
        assert_eq!(d.into_inner().flash[0], 1);
    }
}

#[test]
fn every_wait_is_bounded() {
    // a poll timeout of 0x80_80_80 ms (the C++ app read these bytes as signed: about 49 days)
    let mut d = dfu(Faults { busy_poll_ms: 0x80_8080, ..Faults::default() });
    d.erase(FLASH, 1).unwrap();
    let log = d.into_inner().log;
    assert!(log.iter().any(|l| l == &format!("SLEEP {MAX_POLL_MS}")));
    assert!(log.iter().filter_map(|l| l.strip_prefix("SLEEP ")).all(|ms| ms.parse::<u32>().unwrap() <= MAX_POLL_MS));

    // busy for ever
    let mut d = dfu(Faults { stuck_busy: true, ..Faults::default() });
    assert_eq!(d.erase(FLASH, 1), Err(Error::Busy { during: "erase" }));
    assert_eq!(d.into_inner().log.iter().filter(|l| *l == "IN 3 0 6").count(), 1 + MAX_POLLS);

    // OK but never dnload-idle after a block
    let mut d = dfu(Faults { idle_instead_of_dnload_idle: true, ..Faults::default() });
    assert_eq!(d.download(&[1], FLASH, 0), Err(Error::Busy { during: "download" }));

    // many busy answers are fine
    let mut d = dfu(Faults { extra_busy: 50, ..Faults::default() });
    d.download(&[1], FLASH, 0).unwrap();
}

#[test]
fn device_faults_stop_before_writing() {
    // a status of the wrong length
    let mut d = dfu(Faults { status_len: Some(5), ..Faults::default() });
    assert_eq!(d.download(&[1], FLASH, 0), Err(Error::BadStatus { len: 5 }));
    assert_eq!(writes(&d.into_inner().log), 0);
    // a transfer size of 0: both loops in the C++ app never ended
    let mut desc = fake::descriptor();
    desc[5] = 0;
    desc[6] = 0;
    let mut d = dfu(Faults { descriptor: Some(desc), ..Faults::default() });
    assert_eq!(d.download(&[1], FLASH, 0), Err(Error::ZeroTransferSize));
    assert_eq!(d.upload(FLASH, 4, 0), Err(Error::ZeroTransferSize));
    assert_eq!(writes(&d.into_inner().log), 0);
    // no functional descriptor
    let mut d = dfu(Faults { descriptor: Some(vec![9, 0x21]), ..Faults::default() });
    assert_eq!(d.download(&[1], FLASH, 0), Err(Error::NoFunctionalDescriptor));
    // a transfer size of 1 cannot address 64 KiB
    let mut desc = fake::descriptor();
    desc[5] = 1;
    desc[6] = 0;
    let mut d = dfu(Faults { descriptor: Some(desc), ..Faults::default() });
    assert_eq!(d.download(&vec![0; 0x1_0000], FLASH, 0), Err(Error::TooLarge { len: 0x1_0000 }));
    assert_eq!(writes(&d.into_inner().log), 0);
    // an erase the device refuses
    let mut d = dfu(Faults { fail_op: Some((1, fake::ERR_WRITE)), ..Faults::default() });
    assert_eq!(d.erase(FLASH, 1), Err(Error::Device { during: "erase", status: fake::ERR_WRITE, state: 10 }));
    // an upload answered with nothing is an error, not the end of memory
    let mut d = dfu(Faults { empty_upload: true, ..Faults::default() });
    assert_eq!(d.upload(OPTION_BYTES, 128, 1), Err(Error::EmptyUpload { offset: 0 }));
}

#[test]
fn hostile_layouts_erase_nothing() {
    for strings in [
        vec![b"@Internal Flash   /0x08000000/0*04Kg".to_vec()],
        vec![b"@Internal Flash   /0x08000000/1*K".to_vec()],
        vec![b"@Internal Flash   /0x08000000/4294967295*0Kg".to_vec()],
        vec![b"@Internal Flash   /0xZZ/256*04Kg".to_vec()],
        vec![b"@Internal Flash   /0x08000000/1*04Kg".to_vec()], // smaller than what is erased
        vec![],
    ] {
        let mut d = dfu(Faults { strings: Some(strings.clone()), ..Faults::default() });
        assert!(d.erase(FLASH, 0x2000).is_err(), "{strings:?}");
        let fake = d.into_inner();
        assert!(fake.erased_pages.is_empty());
        assert_eq!(writes(&fake.log), 0);
    }
    // an empty range erases nothing either
    let mut d = dfu(Faults::default());
    assert!(matches!(d.erase(FLASH, 0), Err(Error::Layout(_))));
    assert!(d.into_inner().erased_pages.is_empty());
}
