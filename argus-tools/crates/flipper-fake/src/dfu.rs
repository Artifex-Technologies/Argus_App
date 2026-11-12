//! A fake STM32WB55 ROM bootloader (DfuSe over DFU 1.1), for tests only (moved here from flipper-dfu's
//! tests so `argus-cli`'s tests can serve it too): interface 0 with three
//! alternate settings (flash, option bytes, OTP), each described by its interface string, a 1 MiB
//! flash that must be erased (0xFF) before it is written, and the DFU state machine as the device
//! runs it. Every request is recorded in `log`, in a form both the Rust code and the C++ app (over
//! the oracle's line protocol) produce, so the two can be compared request for request.
//!
//! It misbehaves on purpose when told to (`Faults`).

use flipper_dfu::device::{command, request, state, Transport, TransportError};

pub const FLASH: u32 = 0x0800_0000;
pub const FLASH_SIZE: usize = 1 << 20;
pub const PAGE: usize = 4096;
pub const OPTION_BYTES: u32 = 0x1FFF_8000;
pub const OTP: u32 = 0x1FFF_7000;
pub const SRAM2A: u32 = 0x2003_0000;

/// DFU status codes used here.
pub const ERR_TARGET: u8 = 0x01;
pub const ERR_WRITE: u8 = 0x03;
pub const ERR_PROG: u8 = 0x06;
pub const ERR_ADDRESS: u8 = 0x08;
pub const ERR_STALLEDPKT: u8 = 0x0F;

#[derive(Clone, Default)]
pub struct Faults {
    /// `bwPollTimeout` while busy (24 bits; values with a byte of 0x80 or more catch sign mistakes).
    pub busy_poll_ms: u32,
    /// Extra `dfuDNBUSY` answers before a step completes.
    pub extra_busy: usize,
    /// Never leaves `dfuDNBUSY`.
    pub stuck_busy: bool,
    /// After a data block, reports `dfuIDLE` with OK instead of `dfuDNLOAD-IDLE`.
    pub idle_instead_of_dnload_idle: bool,
    /// The nth (from 1) DNLOAD operation fails with this status.
    pub fail_op: Option<(usize, u8)>,
    /// `DFU_GETSTATUS` answers this many bytes instead of 6.
    pub status_len: Option<usize>,
    /// The functional descriptor as given (default: 9 bytes, transfer size 1024).
    pub descriptor: Option<Vec<u8>>,
    /// Interface strings instead of the real ones (by alternate setting).
    pub strings: Option<Vec<Vec<u8>>>,
    /// The device answers uploads with nothing.
    pub empty_upload: bool,
    /// The device starts in an error state (status errTARGET, dfuERROR).
    pub start_in_error: bool,
    /// The device starts in dfuDNLOAD-IDLE.
    pub start_dnload_idle: bool,
    /// The FUS status and error bytes at 0xFFFF0054 (default: idle, no error).
    pub fus: Option<[u8; 2]>,
    /// The device information table (56 bytes) at SRAM2A + 0x100 (default: a FUS table).
    pub info_table: Option<Vec<u8>>,
    /// What SRAM2A's first word points to (default: SRAM2A + 0x100).
    pub table_address: Option<u32>,
    /// The option bytes as the device holds them (default: Flipper's ob.data).
    pub option_bytes: Option<Vec<u8>>,
    /// The FUS as AN5185 describes it: a second state read while the wireless stack runs starts the
    /// FUS (the chip resets), delete and upgrade report "service ongoing" / "upgrade ongoing" for two
    /// reads, the upgrade ends with a reset into the new wireless stack, and the start command resets
    /// into it. A reset brings the chip back in the bootloader when nBOOT0 is 0, else it boots its
    /// firmware (`booted_firmware`) and the bootloader is gone.
    pub fus_model: bool,
    /// With `fus_model`: the FUS never finishes deleting the wireless stack ("service ongoing" for ever).
    pub fus_never_settles: bool,
    /// A data block written over this address fails with errPROG (as a flash that will not program).
    pub fail_write_at: Option<u32>,
    /// The OTP area (1 KiB) as given (default: bytes counting up, no factory information).
    pub otp: Option<Vec<u8>>,
}

pub const FUS_STATUS: u32 = 0xFFFF_0054;

/// Flipper's `scripts/ob.data` as the chip holds it: every word followed by its complement, RDP
/// 0xAA (level 0), and a pattern in the bits no field covers (so a write that does not keep them
/// shows).
pub fn flipper_option_bytes() -> Vec<u8> {
    use flipper_dfu::wb55::OptionBytes;
    let mut raw = vec![0xA5u8; 128];
    for w in (0..128).step_by(8) {
        // complement words start as the complement of the pattern
        raw[w + 4..w + 8].copy_from_slice(&[0x5A; 4]);
    }
    raw[0] = 0xAA;
    raw[4] = 0x55;
    let mut ob = OptionBytes::from_bytes(&raw).unwrap();
    for (name, value) in [
        ("BOR_LEV", 0x4),
        ("nBOOT0", 1),
        ("nBOOT1", 1),
        ("nSWBOOT0", 1),
        ("SRAM2RST", 0),
        ("SRAM2PE", 1),
        ("nRST_STOP", 1),
        ("nRST_STDBY", 1),
        ("nRSTSHDW", 1),
        ("WWDGSW", 1),
        ("IWDGSTDBY", 1),
        ("IWDGSTOP", 1),
        ("IWDGSW", 1),
        ("IPCCDBA", 0),
        ("ESE", 1),
        ("SFSA", 0xD7),
        ("FSD", 0),
        ("DDS", 1),
        ("C2OPT", 1),
        ("NBRSD", 0),
        ("SNBRSA", 0xB),
        ("BRSD", 0),
        ("SBRSA", 0x12),
        ("SBRV", 0x35C00),
        ("PCROP1A_STRT", 0x1FF),
        ("PCROP1A_END", 0),
        ("PCROP_RDP", 1),
        ("PCROP1B_STRT", 0x1FF),
        ("PCROP1B_END", 0),
        ("WRP1A_STRT", 0xFF),
        ("WRP1A_END", 0),
        ("WRP1B_STRT", 0xFF),
        ("WRP1B_END", 0),
        ("AGC_TRIM", 0),
    ] {
        ob.set(name, value).unwrap();
    }
    ob.bytes().to_vec()
}

/// A FUS device information table: FUS 1.2.0, wireless stack 1.17.3.
pub fn fus_info_table() -> Vec<u8> {
    let mut t = vec![0u8; 56];
    t[..4].copy_from_slice(&flipper_dfu::wb55::DIT_FUS_MAGIC.to_le_bytes());
    t[12..16].copy_from_slice(&[0x00, 0, 2, 1]);
    t[20..24].copy_from_slice(&[0x00, 3, 17, 1]);
    t
}

/// An OTP area holding valid factory information (format 2, as the factory writes it) for the made-up
/// device of `FakeFlipper::standard`: name "Fakeflip", hardware 12, target 7, body 9, connect 6, color 1,
/// `region` (0 is the development region). The C++ app names a device in its bootloader by it, and must find the same name as on the
/// firmware side to see "the same device back". The rest of the OTP is unprogrammed (0xFF).
pub fn factory_otp(region: u8) -> Vec<u8> {
    let mut otp = vec![0xFF; 1024];
    let mut info = Vec::with_capacity(32);
    info.extend(0xBABEu16.to_le_bytes());
    info.extend([2, 0]); // format 2, reserved
    info.extend(1_700_000_000u32.to_le_bytes());
    info.extend([12, 7, 9, 6, 1, 0]); // version, target, body, connect, display, reserved
    info.extend(0u16.to_le_bytes());
    info.extend([1, region]); // color, region
    info.extend(0u16.to_le_bytes());
    info.extend(0u32.to_le_bytes());
    info.extend(b"Fakeflip");
    otp[..32].copy_from_slice(&info);
    otp
}

pub struct FakeDfu {
    pub log: Vec<String>,
    pub flash: Vec<u8>,
    pub option_bytes: Vec<u8>,
    pub otp: Vec<u8>,
    pub sram: Vec<u8>,
    pub fus: Vec<u8>,
    pub erased_pages: Vec<u32>,
    pub fus_commands: Vec<u8>,
    pub left: bool,
    /// Resets so far, and whether the last one booted the firmware.
    pub resets: usize,
    pub booted_firmware: bool,
    /// Where each client connection's requests start in `log` (multi-connection servers only).
    pub sessions: Vec<usize>,
    reset_pending: bool,
    not_running_reads: usize,
    fus_countdown: usize,
    fus_after: Option<([u8; 2], bool)>,
    alt: u8,
    state: u8,
    status: u8,
    pointer: u32,
    pending: Option<(u16, Vec<u8>)>,
    busy_left: usize,
    ops: usize,
    pub faults: Faults,
}

pub fn hex(b: &[u8]) -> String {
    if b.is_empty() {
        return "-".into();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl FakeDfu {
    pub fn new(faults: Faults) -> FakeDfu {
        let option_bytes = faults.option_bytes.clone().unwrap_or_else(flipper_option_bytes);
        let mut sram = vec![0; 0x400];
        // the device information table's address, at the start of SRAM2A, and the table
        sram[..4].copy_from_slice(&faults.table_address.unwrap_or(SRAM2A + 0x100).to_le_bytes());
        let table = faults.info_table.clone().unwrap_or_else(fus_info_table);
        sram[0x100..0x100 + table.len()].copy_from_slice(&table);
        let fus = faults.fus.unwrap_or([0, 0]).to_vec();
        let (state, status) = if faults.start_in_error {
            (state::ERROR, ERR_TARGET)
        } else if faults.start_dnload_idle {
            (state::DNLOAD_IDLE, 0)
        } else {
            (state::DFU_IDLE, 0)
        };
        FakeDfu {
            log: Vec::new(),
            flash: vec![0xFF; FLASH_SIZE],
            option_bytes,
            otp: faults.otp.clone().unwrap_or_else(|| (0..1024).map(|i| i as u8).collect()),
            sram,
            fus,
            erased_pages: Vec::new(),
            fus_commands: Vec::new(),
            left: false,
            resets: 0,
            booted_firmware: false,
            sessions: Vec::new(),
            reset_pending: false,
            not_running_reads: 0,
            fus_countdown: 0,
            fus_after: None,
            alt: 0,
            state,
            status,
            pointer: 0,
            pending: None,
            busy_left: 0,
            ops: 0,
            faults,
        }
    }

    /// The chip resets: it leaves USB, and comes back in the bootloader only when nBOOT0 is 0.
    fn reset(&mut self) {
        self.left = true;
        self.reset_pending = false;
        self.resets += 1;
        self.state = state::DFU_IDLE;
        self.status = 0;
        self.alt = 0;
        self.pending = None;
        // nBOOT0 is bit 27 of the first option word
        self.booted_firmware = self.option_bytes[3] & 0x08 != 0;
    }

    /// A FUS state read, with the FUS model's transitions.
    fn fus_read(&mut self) {
        if self.fus == [0xFF, 0xFE] {
            self.not_running_reads += 1;
            if self.not_running_reads == 2 {
                // the second read starts the FUS: the chip resets into it
                self.not_running_reads = 0;
                self.fus = vec![0, 0];
                self.reset_pending = true;
            }
            return;
        }
        if self.fus_countdown > 0 {
            self.fus_countdown -= 1;
            if self.fus_countdown == 0 {
                if let Some((after, reset)) = self.fus_after.take() {
                    self.fus = after.to_vec();
                    if reset {
                        self.set_wireless_version([1, 17, 3]);
                        self.reset_pending = true;
                    }
                }
            }
        }
    }

    fn set_wireless_version(&mut self, v: [u8; 3]) {
        // the FUS information table at SRAM2A + 0x100: wireless stack version at bytes 20..24
        self.sram[0x100 + 20..0x100 + 24].copy_from_slice(&[0, v[2], v[1], v[0]]);
    }

    fn strings(&self) -> Vec<Vec<u8>> {
        self.faults.strings.clone().unwrap_or_else(|| {
            vec![
                b"@Internal Flash   /0x08000000/256*04Kg".to_vec(),
                b"@Option Bytes   /0x1FFF8000/01*128 e".to_vec(),
                b"@OTP Memory   /0x1FFF7000/01*1Ke".to_vec(),
            ]
        })
    }

    fn stall(&mut self) -> TransportError {
        self.state = state::ERROR;
        self.status = ERR_STALLEDPKT;
        TransportError("stall".into())
    }

    /// The memory an address range of the current alternate setting falls in: (region, offset).
    fn region(&mut self, address: u32, len: usize) -> Option<(&mut Vec<u8>, usize)> {
        let (base, mem) = match self.alt {
            0 if address >= FUS_STATUS => (FUS_STATUS, &mut self.fus),
            0 if address >= SRAM2A => (SRAM2A, &mut self.sram),
            0 => (FLASH, &mut self.flash),
            1 => (OPTION_BYTES, &mut self.option_bytes),
            2 => (OTP, &mut self.otp),
            _ => return None,
        };
        let off = address.checked_sub(base)? as usize;
        if off + len > mem.len() {
            return None;
        }
        Some((mem, off))
    }

    fn transfer_size(&self) -> usize {
        let d = self.faults.descriptor.clone().unwrap_or_else(descriptor);
        if d.len() >= 7 {
            usize::from(u16::from_le_bytes([d[5], d[6]])).max(1)
        } else {
            1024
        }
    }

    /// Runs a block-0 command or writes a data block; returns the status.
    fn execute(&mut self, block: u16, data: &[u8]) -> u8 {
        self.ops += 1;
        if let Some((n, status)) = self.faults.fail_op {
            if n == self.ops {
                return status;
            }
        }
        match block {
            0 => match data {
                [command::SET_ADDRESS, a @ ..] if a.len() == 4 => {
                    let address = u32::from_le_bytes([a[0], a[1], a[2], a[3]]);
                    if self.region(address, 0).is_none() {
                        return ERR_ADDRESS;
                    }
                    self.pointer = address;
                    0
                }
                [command::ERASE, a @ ..] if a.len() == 4 => {
                    let address = u32::from_le_bytes([a[0], a[1], a[2], a[3]]);
                    // by address, whatever alternate setting is selected: upstream's wireless stack update erases
                    // right after reading the option bytes (alternate setting 1 still selected)
                    if !(FLASH..FLASH + FLASH_SIZE as u32).contains(&address) {
                        return ERR_ADDRESS;
                    }
                    let page = (address - FLASH) as usize / PAGE;
                    self.flash[page * PAGE..(page + 1) * PAGE].fill(0xFF);
                    self.erased_pages.push(FLASH + (page * PAGE) as u32);
                    0
                }
                [c @ (command::FUS_FW_DELETE | command::FUS_FW_UPGRADE | command::FUS_START_WS)] => {
                    self.fus_commands.push(*c);
                    if self.faults.fus_model {
                        match *c {
                            command::FUS_FW_DELETE => {
                                self.fus = vec![0x30, 0];
                                self.fus_countdown = if self.faults.fus_never_settles { 0 } else { 2 };
                                self.fus_after = Some(([0, 0], false));
                                self.set_wireless_version([0, 0, 0]);
                            }
                            command::FUS_FW_UPGRADE => {
                                self.fus = vec![0x10, 0];
                                self.fus_countdown = 2;
                                self.fus_after = Some(([0xFF, 0xFE], true));
                            }
                            _ => {
                                self.fus = vec![0xFF, 0xFE];
                                self.reset_pending = true;
                            }
                        }
                    }
                    0
                }
                _ => ERR_TARGET,
            },
            1 => ERR_TARGET,
            n => {
                let address = u64::from(self.pointer) + (u64::from(n) - 2) * self.transfer_size() as u64;
                let Ok(address) = u32::try_from(address) else { return ERR_ADDRESS };
                let flash = self.alt == 0 && address < SRAM2A;
                if self.alt == 0 && address >= FUS_STATUS {
                    return ERR_TARGET;
                }
                if self
                    .faults
                    .fail_write_at
                    .is_some_and(|a| (u64::from(address)..u64::from(address) + data.len() as u64).contains(&u64::from(a)))
                {
                    return ERR_PROG;
                }
                let Some((mem, off)) = self.region(address, data.len()) else { return ERR_ADDRESS };
                if flash && mem[off..off + data.len()].iter().any(|&b| b != 0xFF) {
                    return ERR_PROG;
                }
                mem[off..off + data.len()].copy_from_slice(data);
                0
            }
        }
    }
}

/// A DFU functional descriptor: will-detach, manifestation tolerant, upload, download; transfer size 1024.
pub fn descriptor() -> Vec<u8> {
    vec![9, 0x21, 0x0B, 0xFF, 0x00, 0x00, 0x04, 0x1A, 0x01]
}

impl Transport for FakeDfu {
    fn set_alt(&mut self, alt: u8) -> Result<(), TransportError> {
        self.log.push(format!("ALT {alt}"));
        if self.left || usize::from(alt) >= self.strings().len().max(3) {
            return Err(TransportError("no such alternate setting".into()));
        }
        self.alt = alt;
        Ok(())
    }

    fn control_out(&mut self, req: u8, value: u16, data: &[u8]) -> Result<usize, TransportError> {
        self.log.push(format!("OUT {req} {value} {}", hex(data)));
        if self.reset_pending {
            self.reset();
        }
        if self.left {
            return Err(TransportError("device gone".into()));
        }
        match req {
            request::DNLOAD => {
                if self.state != state::DFU_IDLE && self.state != state::DNLOAD_IDLE {
                    return Err(self.stall());
                }
                if data.is_empty() {
                    self.state = state::MANIFEST_SYNC;
                    return Ok(0);
                }
                self.pending = Some((value, data.to_vec()));
                self.busy_left = 1 + self.faults.extra_busy;
                self.state = state::DNLOAD_SYNC;
                Ok(data.len())
            }
            request::CLRSTATUS => {
                if self.state != state::ERROR {
                    return Err(self.stall());
                }
                self.state = state::DFU_IDLE;
                self.status = 0;
                Ok(0)
            }
            request::ABORT => {
                if self.state == state::ERROR {
                    return Err(self.stall());
                }
                self.pending = None;
                self.state = state::DFU_IDLE;
                Ok(0)
            }
            _ => Err(self.stall()),
        }
    }

    fn control_in(&mut self, req: u8, value: u16, length: u16) -> Result<Vec<u8>, TransportError> {
        self.log.push(format!("IN {req} {value} {length}"));
        if self.reset_pending {
            self.reset();
        }
        if self.left {
            return Err(TransportError("device gone".into()));
        }
        match req {
            request::GETSTATUS => {
                let mut poll = 0;
                match self.state {
                    state::DNLOAD_SYNC | state::DNBUSY => {
                        if self.busy_left > 0 || self.faults.stuck_busy {
                            self.busy_left = self.busy_left.saturating_sub(1);
                            self.state = state::DNBUSY;
                            poll = self.faults.busy_poll_ms;
                        } else {
                            let (block, data) = self.pending.take().unwrap_or_default();
                            let status = self.execute(block, &data);
                            if status == 0 && self.alt == 1 && block >= 2 {
                                // the option bytes are reloaded: the chip resets and leaves USB
                                self.reset();
                                return Err(TransportError("device reset".into()));
                            }
                            if status != 0 {
                                self.state = state::ERROR;
                                self.status = status;
                            } else if self.faults.idle_instead_of_dnload_idle && block >= 2 {
                                self.state = state::DFU_IDLE;
                            } else {
                                self.state = state::DNLOAD_IDLE;
                            }
                        }
                    }
                    state::MANIFEST_SYNC => {
                        // the bootloader leaves: the chip resets
                        self.reset();
                        return Err(TransportError("device gone".into()));
                    }
                    _ => {}
                }
                let p = poll.to_le_bytes();
                let mut out = vec![self.status, p[0], p[1], p[2], self.state, 0];
                out.truncate(self.faults.status_len.unwrap_or(6).min(usize::from(length)));
                Ok(out)
            }
            request::UPLOAD => {
                if self.state != state::DFU_IDLE && self.state != state::UPLOAD_IDLE {
                    return Err(self.stall());
                }
                if self.faults.empty_upload {
                    return Ok(Vec::new());
                }
                if value < 2 {
                    self.state = state::UPLOAD_IDLE;
                    return Ok(vec![0x00, command::SET_ADDRESS, command::ERASE, 0x92]);
                }
                // ST's bootloader: pointer + (block - 2) * its transfer size, whatever this request's length
                let address = u64::from(self.pointer) + (u64::from(value) - 2) * self.transfer_size() as u64;
                let Ok(address) = u32::try_from(address) else { return Err(self.stall()) };
                // short at the end of the region
                let Some((mem, off)) = self.region(address, 0) else { return Err(self.stall()) };
                let n = usize::from(length).min(mem.len() - off);
                let out = mem[off..off + n].to_vec();
                if self.faults.fus_model && self.alt == 0 && address == FUS_STATUS {
                    self.fus_read();
                }
                self.state = if n < usize::from(length) { state::DFU_IDLE } else { state::UPLOAD_IDLE };
                Ok(out)
            }
            _ => Err(self.stall()),
        }
    }

    fn functional_descriptor(&mut self) -> Result<Vec<u8>, TransportError> {
        self.log.push("DESC".into());
        Ok(self.faults.descriptor.clone().unwrap_or_else(descriptor))
    }

    fn interface_string(&mut self, alt: u8) -> Result<Vec<u8>, TransportError> {
        self.log.push(format!("STRING {alt}"));
        self.strings().get(usize::from(alt)).cloned().ok_or_else(|| TransportError("no such string".into()))
    }

    fn sleep_ms(&mut self, ms: u32) {
        self.log.push(format!("SLEEP {ms}"));
    }
}

/// Answers one request line of the line protocol (the C++ oracle's and [`flipper_dfu::line`]'s):
/// `ALT n`, `OUT req value hex`, `IN req value len`, `DESC`, `STRING alt` get an answer line;
/// `SLEEP ms` is recorded and gets none. None also for a line that is not a request.
pub fn answer(f: &mut FakeDfu, line: &str) -> Option<String> {
    let a = answer_request(f, line);
    // a reset the FUS model asked for (a second state read while the wireless stack runs, the end of
    // an upgrade, starting the wireless stack) happens right after this answer
    if f.reset_pending {
        f.reset();
    }
    a
}

fn answer_request(f: &mut FakeDfu, line: &str) -> Option<String> {
    let parts: Vec<&str> = line.split(' ').collect();
    let num = |s: &str| s.parse::<u64>().ok();
    match parts[..] {
        ["SLEEP", ms] => {
            f.log.push(format!("SLEEP {ms}"));
            None
        }
        ["ALT", alt] => Some(if f.set_alt(num(alt)? as u8).is_ok() { "OK".into() } else { "ERR".into() }),
        ["OUT", req, value, data] => Some(match f.control_out(num(req)? as u8, num(value)? as u16, &unhex(data)?) {
            Ok(n) => format!("OK {n}"),
            Err(_) => "ERR".into(),
        }),
        ["IN", req, value, len] => Some(match f.control_in(num(req)? as u8, num(value)? as u16, num(len)? as u16) {
            Ok(d) => format!("DATA {}", hex(&d)),
            Err(_) => "ERR".into(),
        }),
        ["DESC"] => Some(match f.functional_descriptor() {
            Ok(d) => format!("DATA {}", hex(&d)),
            Err(_) => "ERR".into(),
        }),
        ["STRING", alt] => Some(match f.interface_string(num(alt)? as u8) {
            Ok(d) => format!("DATA {}", hex(&d)),
            Err(_) => "ERR".into(),
        }),
        _ => None,
    }
}

/// Hex as the line protocol writes it (`-` for nothing).
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s == "-" {
        return Some(Vec::new());
    }
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len() / 2).map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()).collect()
}

/// Serves one client of the line protocol on `stream` until it closes; returns the fake.
pub fn serve(f: FakeDfu, stream: std::os::unix::net::UnixStream) -> FakeDfu {
    serve_with(f, stream, &mut || {})
}

/// [`serve`], calling `gone` when the chip resets, before the answer to that request goes out: whoever
/// gets that answer and opens the device again finds it gone (as USB reports a device that left).
pub fn serve_with(mut f: FakeDfu, stream: std::os::unix::net::UnixStream, gone: &mut dyn FnMut()) -> FakeDfu {
    use std::io::{BufRead, BufReader, Write};
    let Ok(mut out) = stream.try_clone() else { return f };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        let a = answer(&mut f, &line);
        if f.left {
            gone();
        }
        if let Some(a) = a {
            if writeln!(out, "{a}").is_err() {
                break;
            }
        }
        if f.left {
            // the chip reset: it is off the bus
            break;
        }
    }
    f
}

/// What [`serve_lives`] reports as the chip comes and goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Life {
    /// The bootloader is on the socket.
    Bootloader,
    /// The chip reset and is off the bus.
    Gone,
    /// The chip booted its firmware: the bootloader does not come back.
    Firmware,
}

/// Serves the fake on a socket at `path` across resets, as a chip that leaves USB and comes back:
/// every connection is one session; after a reset the socket goes away (before the answer that saw the
/// reset) for `gap` and comes back if the chip is in its bootloader again, and stays away once it has
/// booted its firmware. The bootloader appears whenever `enter` is set (set it before the call to start in
/// the bootloader; set it later for a firmware that reboots into it). `life` hears each change. Runs until
/// `stop` is set; returns the fake.
pub fn serve_lives(
    mut f: FakeDfu,
    path: &std::path::Path,
    gap: std::time::Duration,
    stop: &std::sync::atomic::AtomicBool,
    enter: &std::sync::atomic::AtomicBool,
    life: &mut dyn FnMut(Life),
) -> FakeDfu {
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::Ordering;
    let bind = || {
        let _ = std::fs::remove_file(path);
        let l = UnixListener::bind(path).expect("bind the fake bootloader's socket");
        l.set_nonblocking(true).expect("non-blocking listener");
        l
    };
    let mut listener: Option<UnixListener> = None;
    while !stop.load(Ordering::SeqCst) {
        if listener.is_none() {
            if enter.swap(false, Ordering::SeqCst) {
                f.left = false;
                f.booted_firmware = false;
                listener = Some(bind());
                life(Life::Bootloader);
            } else {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
        }
        let accepted = match &listener {
            Some(l) => l.accept(),
            None => continue,
        };
        match accepted {
            Ok((s, _)) => {
                let _ = s.set_nonblocking(false);
                f.sessions.push(f.log.len());
                f = serve_with(f, s, &mut || {
                    listener = None;
                    let _ = std::fs::remove_file(path);
                    life(Life::Gone);
                });
                if f.left {
                    if f.booted_firmware {
                        life(Life::Firmware);
                    } else {
                        std::thread::sleep(gap);
                        enter.store(true, Ordering::SeqCst);
                    }
                }
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(2)),
        }
    }
    drop(listener);
    let _ = std::fs::remove_file(path);
    f
}

/// [`serve_lives`] without listening to the changes.
pub fn serve_with_resets(f: FakeDfu, path: &std::path::Path, gap: std::time::Duration, stop: &std::sync::atomic::AtomicBool) -> FakeDfu {
    serve_lives(f, path, gap, stop, &std::sync::atomic::AtomicBool::new(true), &mut |_| {})
}

/// The log with a `SESSION n` line where each connection started.
pub fn log_with_sessions(f: &FakeDfu) -> Vec<String> {
    let mut out = Vec::with_capacity(f.log.len() + f.sessions.len());
    let mut next = f.sessions.iter().enumerate().peekable();
    for (i, line) in f.log.iter().enumerate() {
        while let Some((n, _)) = next.next_if(|(_, &at)| at <= i) {
            out.push(format!("SESSION {}", n + 1));
        }
        out.push(line.clone());
    }
    for (n, _) in next {
        out.push(format!("SESSION {}", n + 1));
    }
    out
}
