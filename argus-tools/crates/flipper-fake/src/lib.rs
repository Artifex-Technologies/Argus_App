//! A fake Flipper Zero for tests: it prints the CLI banner and prompt, answers
//! `start_rpc_session` the way the firmware does, then serves read-only RPC requests from a
//! scripted device (device info, protobuf version, a small file tree).
//!
//! Its responses follow the firmware's own code (`applications/services/rpc/` in
//! Argus_Firmware): lists go out 8 entries per frame with `has_next`, `/` lists `any`, `int` and
//! `ext`, names that are not ASCII are left out, a property stream ends with an empty frame,
//! and errors carry empty content. Every byte the client sends is recorded so tests can compare
//! what two clients put on the wire.
//!
//! Faults ([`Fault`]) make it misbehave on the first RPC request, for negative tests.

use std::cell::RefCell;
use std::io::{self, Read, Write};

use flipper_rpc::frame::{encode_frame, FrameDecoder};
use flipper_rpc::proto::pb::main::Content;
use flipper_rpc::proto::pb::{CommandStatus, Empty, Main};
use flipper_rpc::proto::{pb_gui, pb_property, pb_storage, pb_system};

pub mod archives;
pub mod dfu;
pub mod firmware;
pub mod storage;
pub mod update_server;

/// Received bytes kept for the transcript.
const MAX_RECORD: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    None,
    /// Never print the prompt.
    NoPrompt,
    /// Switch to RPC, then never answer.
    Silent,
    /// Answer with a complete frame that is not a valid `PB.Main`.
    GarbageFrame,
    /// Announce a 1 MiB frame.
    OversizeFrame,
    /// Answer with this command status and empty content.
    Status(i32),
    /// Send this many frames for another command id before the real answer.
    FloodOtherIds(usize),
    /// Answer every request with a ping response.
    WrongType,
    /// Send half a frame, then close.
    CloseMidFrame,
    /// Every file read sends one byte fewer than its stat said.
    ShortRead,
    /// Every file read sends one byte more than its stat said.
    LongRead,
    /// An empty file's read answer leaves the empty `data` field out (valid protobuf; the
    /// firmware sends it empty instead). Crashed the C++ app before 2026-11-12.
    OmitEmptyData,
    /// The broadcast screen frame after a stream start has no `data` field.
    FrameWithoutData,
    /// A stream start first sends a frame one byte short (then the usual frames).
    ShortFrame,
    /// A stream start first sends a frame one byte too long (then the usual frames).
    LongFrame,
}

/// Screen frame `i` of the fake: a pattern that differs from frame to frame.
pub fn screen_frame(i: usize) -> Vec<u8> {
    (0..1024).map(|j| ((i + 1) * 37 + j * 11) as u8).collect()
}

#[derive(Debug, Clone)]
pub struct Node {
    pub path: String,
    pub dir: bool,
    pub size: u32,
}

impl Node {
    /// The file's bytes: a pattern derived from its path, so tests can recompute them.
    pub fn content(&self) -> Vec<u8> {
        let seed = self.path.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
        (0..self.size).map(|i| (seed.wrapping_add(i.wrapping_mul(131)) % 251) as u8).collect()
    }
}

/// The device clock the fake reports: 2026-11-09 12:34:56, a Monday.
pub const FAKE_DATETIME: (u32, u32, u32, u32, u32, u32, u32) = (2026, 11, 9, 12, 34, 56, 1);

#[derive(Debug, Clone)]
pub struct FakeFlipper {
    pub protobuf_version: (u32, u32),
    /// The `devinfo` property (dotted keys, protobuf 0.14 and later).
    pub devinfo: Vec<(String, String)>,
    /// The older device-info response (underscore keys).
    pub legacy_info: Vec<(String, String)>,
    pub nodes: Vec<Node>,
    pub sd_card: bool,
    pub fault: Fault,
    /// Raw entries added to a folder's listing as they are, bypassing the firmware's
    /// rules: (folder, name, is_dir). For tests of what a hostile device could send.
    pub hostile: Vec<(String, String, bool)>,
    /// Files whose bytes are given instead of [`Node::content`]'s pattern: (path, bytes).
    pub contents: Vec<(String, Vec<u8>)>,
    /// Frames sent after a screen stream starts ([`screen_frame`] 0, 1, ...).
    pub screen_frames: usize,
    /// The storage as clients change it: made from `nodes` and `contents` on the first
    /// request, then kept across sessions (as a device keeps its files across reconnects).
    pub live: RefCell<Option<storage::Storage>>,
    /// The storage refuses every change ("access denied").
    pub read_only: bool,
}

/// What passed over the connection.
#[derive(Debug, Default, Clone)]
pub struct Transcript {
    /// Everything the client sent, handshake included (first [`MAX_RECORD`] bytes).
    pub received: Vec<u8>,
    /// Every RPC request the client sent, decoded, in order.
    pub requests: Vec<Main>,
    /// The session ended because the device left (a reboot or factory reset).
    pub left: bool,
}

impl Default for FakeFlipper {
    fn default() -> Self {
        Self::standard()
    }
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// Names a hostile device could list in `/int`: each would lead outside a local folder or
/// carry control characters. Both apps must refuse them and read nothing under them.
pub const HOSTILE_NAMES: &[&str] = &["..", "../../escape", "a\\b", "line\nfeed", "esc\u{1b}[2J", "del\u{7f}"];

impl FakeFlipper {
    /// [`FakeFlipper::standard`] plus [`HOSTILE_NAMES`] as files in `/int`, and `..` as a folder.
    pub fn with_hostile_names(mut self) -> FakeFlipper {
        for name in HOSTILE_NAMES {
            self.hostile.push(("/int".into(), (*name).into(), false));
        }
        self.hostile.push(("/int".into(), "..".into(), true));
        self
    }

    /// [`FakeFlipper::standard`] plus capture files in the capture folders the apps pull
    /// (real Flipper-format files from argus-tools' test fixtures), a key dictionary under
    /// `assets` (never pulled) and a secret outside the capture folders (never read).
    pub fn with_captures(mut self) -> FakeFlipper {
        let files: [(&str, &[u8]); 8] = [
            ("/ext/nfc/office.nfc", include_bytes!("../../../tests/fixtures/captures/ext/nfc/badge.nfc")),
            ("/ext/nfc/full.nfc", include_bytes!("../../../tests/fixtures/captures/ext/nfc/badge_full.nfc")),
            ("/ext/nfc/assets/mf_classic_dict_user.nfc", b"A0A1A2A3A4A5\n"),
            ("/ext/subghz/garage.sub", include_bytes!("../../../tests/fixtures/captures/ext/subghz/garage.sub")),
            ("/ext/infrared/tv.ir", include_bytes!("../../../tests/fixtures/captures/ext/infrared/tv.ir")),
            ("/ext/lfrfid/door.rfid", include_bytes!("../../../tests/fixtures/captures/ext/lfrfid/door.rfid")),
            ("/ext/wifi_scan/scan_1.csv", b"tick_ms,bssid,ssid,hidden,channel,rssi_dbm,encrypted\n1,,\"test\",0,6,-40,1\n"),
            ("/ext/u2f/key.u2f", b"not for the desktop"),
        ];
        for dir in ["/ext/nfc/assets", "/ext/infrared", "/ext/lfrfid", "/ext/wifi_scan", "/ext/u2f"] {
            self.nodes.push(Node { path: dir.into(), dir: true, size: 0 });
        }
        for (path, bytes) in files {
            self.nodes.push(Node { path: path.into(), dir: false, size: bytes.len() as u32 });
            self.contents.push((path.into(), bytes.to_vec()));
        }
        self
    }

    /// A file's bytes: given in [`FakeFlipper::contents`], or [`Node::content`]'s pattern.
    pub fn data_of(&self, n: &Node) -> Vec<u8> {
        self.contents.iter().find(|(p, _)| *p == n.path).map(|(_, d)| d.clone()).unwrap_or_else(|| n.content())
    }

    /// Runs `f` on the live storage, made from `nodes` and `contents` the first time.
    pub fn with_storage<R>(&self, f: impl FnOnce(&mut storage::Storage) -> R) -> R {
        let mut live = self.live.borrow_mut();
        let s = live.get_or_insert_with(|| {
            let mut s = storage::Storage::new(&self.nodes, |n| self.data_of(n));
            s.read_only = self.read_only;
            s
        });
        f(s)
    }

    /// The storage as it is now (see [`storage::Storage::dump`]).
    pub fn dump(&self) -> String {
        self.with_storage(|s| s.dump())
    }

    /// [`FakeFlipper::standard`] with `/int` changed since a backup of it was made: one file
    /// rewritten, one removed, one added. A restore must bring the first two back and leave the
    /// added one alone.
    pub fn with_changed_int(mut self) -> FakeFlipper {
        self.nodes.retain(|n| n.path != "/int/sub/deep.txt");
        if let Some(n) = self.nodes.iter_mut().find(|n| n.path == "/int/favorites.txt") {
            n.size = 20;
        }
        self.contents.push(("/int/favorites.txt".into(), b"changed since backup".to_vec()));
        self.nodes.push(Node { path: "/int/extra.txt".into(), dir: false, size: 9 });
        self
    }

    /// [`FakeFlipper::standard`] where `/int/sub` is a file, not a folder: a backup with the
    /// folder cannot be restored over it, and must be refused before anything is written.
    pub fn with_int_conflict(mut self) -> FakeFlipper {
        self.nodes.retain(|n| !n.path.starts_with("/int/sub"));
        self.nodes.push(Node { path: "/int/sub".into(), dir: false, size: 3 });
        self
    }

    /// A made-up device on Argus firmware with a few folders. No real device's names or
    /// serial numbers.
    pub fn standard() -> FakeFlipper {
        let devinfo = pairs(&[
            ("format.major", "3"),
            ("format.minor", "3"),
            ("hardware.model", "Flipper Zero"),
            ("hardware.uid", "0000000000000000"),
            ("hardware.ver", "12"),
            ("hardware.target", "7"),
            ("hardware.body", "9"),
            ("hardware.connect", "6"),
            ("hardware.color", "1"),
            ("hardware.region.builtin", "0"),
            ("hardware.name", "Fakeflip"),
            ("firmware.commit.hash", "0123abcd"),
            ("firmware.branch.name", "ARG-1.5.1r2"),
            ("firmware.version", "ARG-1.5.1r2"),
            ("firmware.build.date", "09-11-2026"),
            ("firmware.origin.fork", "Argus"),
            ("firmware.api.major", "88"),
            ("firmware.api.minor", "2"),
            ("protobuf.version.major", "0"),
            ("protobuf.version.minor", "25"),
            ("radio.alive", "true"),
            ("radio.fus.major", "1"),
            ("radio.fus.minor", "2"),
            ("radio.fus.sub", "0"),
            ("radio.stack.major", "1"),
            ("radio.stack.minor", "17"),
            ("radio.stack.sub", "3"),
            ("radio.stack.type", "3"),
        ]);
        let legacy_info = pairs(&[
            ("hardware_name", "Fakeflip"),
            ("hardware_ver", "12"),
            ("hardware_target", "7"),
            ("firmware_version", "0.60.0"),
            ("firmware_branch", "dev"),
            ("firmware_origin_fork", "Official"),
            ("radio_alive", "false"),
        ]);
        let mut nodes = vec![
            Node { path: "/int".into(), dir: true, size: 0 },
            Node { path: "/ext".into(), dir: true, size: 0 },
            Node { path: "/ext/apps".into(), dir: true, size: 0 },
            Node { path: "/ext/nfc".into(), dir: true, size: 0 },
            Node { path: "/ext/subghz".into(), dir: true, size: 0 },
            Node { path: "/ext/empty".into(), dir: true, size: 0 },
            Node { path: "/ext/Manifest".into(), dir: false, size: 48_213 },
            Node { path: "/ext/nfc/badge.nfc".into(), dir: false, size: 512 },
            Node { path: "/ext/nfc/caf\u{e9}.nfc".into(), dir: false, size: 77 },
            Node { path: "/int/.bt.settings".into(), dir: false, size: 1024 },
            Node { path: "/int/favorites.txt".into(), dir: false, size: 37 },
            Node { path: "/int/empty.txt".into(), dir: false, size: 0 },
            Node { path: "/int/sub".into(), dir: true, size: 0 },
            Node { path: "/int/sub/deep.txt".into(), dir: false, size: 1536 },
            Node { path: "/int/sub/inner".into(), dir: true, size: 0 },
        ];
        // 19 files: two full frames of 8 and a last one of 3.
        for i in 0..19 {
            nodes.push(Node { path: format!("/ext/subghz/signal_{i:02}.sub"), dir: false, size: 100 + i });
        }
        FakeFlipper {
            protobuf_version: (0, 25),
            devinfo,
            legacy_info,
            nodes,
            sd_card: true,
            fault: Fault::None,
            hostile: Vec::new(),
            contents: Vec::new(),
            screen_frames: 1,
            live: RefCell::new(None),
            read_only: false,
        }
    }

    /// Serves one connection until the client closes it. Returns what the client sent.
    pub fn serve(&self, io: &mut (impl Read + Write)) -> io::Result<Transcript> {
        let mut t = Transcript::default();
        if self.fault != Fault::NoPrompt {
            io.write_all(b"\r\n  (fake Flipper banner)\r\nWelcome to Flipper Zero Command Line Interface!\r\n\r\n")?;
            io.write_all(b"Firmware version: ARG-1.5.1r2 ARG-1.5.1r2 (0123abcd built on 09-11-2026)\r\n")?;
            io.write_all(b"\r\n>: ")?;
            io.flush()?;
        }
        // The text CLI: echo what is typed, run the line on Enter.
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            if io.read(&mut byte)? == 0 {
                return Ok(t);
            }
            record(&mut t, &byte);
            if byte[0] == b'\r' {
                io.write_all(b"\r\n")?;
                if line == b"start_rpc_session" {
                    break;
                }
                io.write_all(b"could not find command\r\n\r\n>: ")?;
                line.clear();
            } else {
                io.write_all(&byte)?;
                line.push(byte[0]);
                if line.len() > 256 {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "CLI line too long"));
                }
            }
            io.flush()?;
        }
        io.flush()?;

        self.with_storage(|s| s.end_session());
        let mut decoder = FrameDecoder::new();
        let mut chunk = [0u8; 4096];
        let mut first = true;
        loop {
            while let Some(req) = decoder.next_frame().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))? {
                t.requests.push(req.clone());
                let fault = if first { self.fault.clone() } else { Fault::None };
                first = false;
                if !self.answer(io, &req, &fault)? {
                    t.left = matches!(req.content, Some(Content::SystemRebootRequest(_)) | Some(Content::SystemFactoryResetRequest(_)));
                    return Ok(t);
                }
            }
            let n = io.read(&mut chunk)?;
            if n == 0 {
                return Ok(t);
            }
            record(&mut t, &chunk[..n]);
            decoder.push(&chunk[..n]).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        }
    }

    /// Answers one request; `false` ends the connection.
    fn answer(&self, io: &mut impl Write, req: &Main, fault: &Fault) -> io::Result<bool> {
        let id = req.command_id;
        match fault {
            Fault::None
            | Fault::NoPrompt
            | Fault::ShortRead
            | Fault::LongRead
            | Fault::OmitEmptyData
            | Fault::FrameWithoutData
            | Fault::ShortFrame
            | Fault::LongFrame => {}
            Fault::Silent => return Ok(true),
            Fault::GarbageFrame => {
                io.write_all(&[0x02, 0x0f, 0x00])?;
                io.flush()?;
                return Ok(true);
            }
            Fault::OversizeFrame => {
                io.write_all(&[0x80, 0x80, 0x40])?;
                io.flush()?;
                return Ok(true);
            }
            Fault::Status(s) => {
                send(io, Main { command_id: id, command_status: *s, content: Some(Content::Empty(Empty {})), ..Default::default() })?;
                return Ok(true);
            }
            Fault::FloodOtherIds(n) => {
                for _ in 0..*n {
                    send(io, Main { command_id: id.wrapping_add(1000), content: Some(Content::Empty(Empty {})), ..Default::default() })?;
                }
            }
            Fault::WrongType => {
                send(
                    io,
                    Main {
                        command_id: id,
                        content: Some(Content::SystemPingResponse(pb_system::PingResponse { data: vec![1] })),
                        ..Default::default()
                    },
                )?;
                return Ok(true);
            }
            Fault::CloseMidFrame => {
                let f = encode_frame(&Main { command_id: id, content: Some(Content::Empty(Empty {})), ..Default::default() })
                    .map_err(|e| io::Error::other(e.to_string()))?;
                io.write_all(&f[..f.len() / 2])?;
                io.flush()?;
                return Ok(false);
            }
        }

        let ok = |content: Content, has_next: bool| Main { command_id: id, has_next, content: Some(content), ..Default::default() };
        let error = |status: CommandStatus| Main {
            command_id: id,
            command_status: status as i32,
            content: Some(Content::Empty(Empty {})),
            ..Default::default()
        };

        match &req.content {
            Some(Content::SystemProtobufVersionRequest(_)) => {
                let (major, minor) = self.protobuf_version;
                send(io, ok(Content::SystemProtobufVersionResponse(pb_system::ProtobufVersionResponse { major, minor }), false))?;
            }
            Some(Content::PropertyGetRequest(r)) => {
                let (top, sub) = r.key.split_once('.').unwrap_or((r.key.as_str(), ""));
                if top != "devinfo" {
                    send(io, error(CommandStatus::ErrorInvalidParameters))?;
                } else {
                    for (k, v) in self.devinfo.iter().filter(|(k, _)| k.starts_with(sub)) {
                        let resp = pb_property::GetResponse { key: k.clone(), value: v.clone() };
                        send(io, ok(Content::PropertyGetResponse(resp), true))?;
                    }
                    send(io, ok(Content::Empty(Empty {}), false))?;
                }
            }
            Some(Content::SystemDeviceInfoRequest(_)) => {
                let n = self.legacy_info.len();
                for (i, (k, v)) in self.legacy_info.iter().enumerate() {
                    let resp = pb_system::DeviceInfoResponse { key: k.clone(), value: v.clone() };
                    send(io, ok(Content::SystemDeviceInfoResponse(resp), i + 1 < n))?;
                }
            }
            Some(Content::StorageListRequest(r)) => {
                self.storage_request(io, |_| ())?;
                self.list(io, id, &r.path)?
            }
            Some(Content::StorageInfoRequest(r)) => {
                self.storage_request(io, |_| ())?;
                if r.path == "/ext" && !self.sd_card {
                    send(io, error(CommandStatus::ErrorStorageNotReady))?;
                } else if r.path == "/ext" || r.path == "/int" {
                    let (total, free) = if r.path == "/ext" { (31_902_400_512, 30_000_000_000) } else { (262_144, 200_704) };
                    send(io, ok(Content::StorageInfoResponse(pb_storage::InfoResponse { total_space: total, free_space: free }), false))?;
                } else {
                    send(io, error(CommandStatus::ErrorStorageInvalidParameter))?;
                }
            }
            Some(Content::StorageStatRequest(r)) => match self.storage_request(io, |s| s.find(&r.path).cloned())? {
                Some(n) => {
                    let file = pb_storage::File { r#type: n.dir as i32, size: n.size, ..Default::default() };
                    send(io, ok(Content::StorageStatResponse(pb_storage::StatResponse { file: Some(file) }), false))?;
                }
                None => send(io, error(CommandStatus::ErrorStorageNotExist))?,
            },
            Some(Content::StorageReadRequest(r)) => {
                match self.storage_request(io, |s| s.find(&r.path).filter(|n| !n.dir).map(|n| s.data(&n.path)))? {
                    None => send(io, error(CommandStatus::ErrorStorageNotExist))?,
                    Some(mut data) => {
                        match self.fault {
                            Fault::ShortRead if !data.is_empty() => {
                                data.pop();
                            }
                            Fault::LongRead => data.push(0),
                            _ => {}
                        }
                        // As the firmware: 512 bytes per frame, has_next while bytes are left; an
                        // empty file is one frame with empty data.
                        let read = |chunk: &[u8], has_next: bool| {
                            let file = pb_storage::File { data: chunk.to_vec(), ..Default::default() };
                            ok(Content::StorageReadResponse(pb_storage::ReadResponse { file: Some(file) }), has_next)
                        };
                        if data.is_empty() && self.fault == Fault::OmitEmptyData {
                            send(io, read(&[], false))?;
                        } else if data.is_empty() {
                            send_raw(io, &empty_file_read_response(id))?;
                        }
                        let mut chunks = data.chunks(512).peekable();
                        while let Some(c) = chunks.next() {
                            send(io, read(c, chunks.peek().is_some()))?;
                        }
                    }
                }
            }
            Some(Content::StorageMd5sumRequest(r)) => {
                match self.storage_request(io, |s| s.find(&r.path).filter(|n| !n.dir).map(|n| s.data(&n.path)))? {
                    None => send(io, error(CommandStatus::ErrorStorageNotExist))?,
                    Some(data) => {
                        use md5::Digest;
                        let md5sum: String = md5::Md5::digest(data).iter().map(|b| format!("{b:02x}")).collect();
                        send(io, ok(Content::StorageMd5sumResponse(pb_storage::Md5sumResponse { md5sum }), false))?;
                    }
                }
            }
            Some(Content::SystemGetDatetimeRequest(_)) => {
                let (year, month, day, hour, minute, second, weekday) = FAKE_DATETIME;
                let datetime = pb_system::DateTime { hour, minute, second, day, month, year, weekday };
                send(io, ok(Content::SystemGetDatetimeResponse(pb_system::GetDateTimeResponse { datetime: Some(datetime) }), false))?;
            }
            // The C++ app sets the clock on every connection; the fake accepts it (and the
            // transcript records it) but its clock does not change.
            Some(Content::SystemSetDatetimeRequest(_)) => send(io, ok(Content::Empty(Empty {}), false))?,
            // The C++ app starts the screen stream on every connection. The firmware answers OK
            // and then sends frames as broadcasts (command id 0); the fake sends `screen_frames`.
            Some(Content::GuiStartScreenStreamRequest(_)) => {
                send(io, ok(Content::Empty(Empty {}), false))?;
                let broadcast = |io: &mut _, data: Vec<u8>| {
                    let frame = pb_gui::ScreenFrame { data, ..Default::default() };
                    send(io, Main { command_id: 0, content: Some(Content::GuiScreenFrame(frame)), ..Default::default() })
                };
                match self.fault {
                    Fault::ShortFrame => broadcast(io, vec![0xff; 1023])?,
                    Fault::LongFrame => broadcast(io, vec![0xff; 1025])?,
                    _ => {}
                }
                for i in 0..self.screen_frames {
                    broadcast(io, if self.fault == Fault::FrameWithoutData { Vec::new() } else { screen_frame(i) })?;
                }
            }
            Some(Content::GuiStopScreenStreamRequest(_)) => send(io, ok(Content::Empty(Empty {}), false))?,
            // The C++ app shows a status screen on the Flipper during backups and updates.
            Some(Content::GuiStartVirtualDisplayRequest(_)) | Some(Content::GuiStopVirtualDisplayRequest(_)) => {
                send(io, ok(Content::Empty(Empty {}), false))?
            }
            // Frames the host draws on the virtual display get no answer from the firmware.
            Some(Content::GuiScreenFrame(_)) => {}
            Some(Content::StorageMkdirRequest(r)) => answer_all(io, self.with_storage(|s| s.mkdir(id, &r.path)))?,
            Some(Content::StorageDeleteRequest(r)) => answer_all(io, self.with_storage(|s| s.remove(id, &r.path, r.recursive)))?,
            Some(Content::StorageRenameRequest(r)) => answer_all(io, self.with_storage(|s| s.rename(id, &r.old_path, &r.new_path)))?,
            Some(Content::StorageWriteRequest(r)) => {
                let data = r.file.as_ref().map(|f| f.data.clone()).unwrap_or_default();
                answer_all(io, self.with_storage(|s| s.write(id, &r.path, &data, req.has_next)))?
            }
            // As the firmware: a ping with has_next is refused, otherwise its data comes back.
            Some(Content::SystemPingRequest(r)) => {
                if req.has_next {
                    send(io, error(CommandStatus::ErrorInvalidParameters))?;
                } else {
                    send(io, ok(Content::SystemPingResponse(pb_system::PingResponse { data: r.data.clone() }), false))?;
                }
            }
            // The firmware reboots without answering: the USB port goes away, which ends the
            // session here. A factory reset sets the flag that formats /int on the next boot,
            // then reboots the same way. Other reboot modes are refused.
            Some(Content::SystemRebootRequest(r)) => {
                if pb_system::reboot_request::RebootMode::try_from(r.mode).is_err() {
                    send(io, error(CommandStatus::ErrorInvalidParameters))?;
                } else {
                    self.with_storage(|s| s.end_session());
                    return Ok(false);
                }
            }
            // update_operation_prepare: the manifest must be a file whose folder exists and whose
            // first line names the format; a refusal is ERROR_INVALID_PARAMETERS with the code.
            Some(Content::SystemUpdateRequest(r)) => {
                let code = self.with_storage(|s| s.update_check(&r.update_manifest));
                let status = if code == 0 { CommandStatus::Ok } else { CommandStatus::ErrorInvalidParameters };
                let resp = pb_system::UpdateResponse { code };
                send(
                    io,
                    Main {
                        command_id: id,
                        command_status: status as i32,
                        content: Some(Content::SystemUpdateResponse(resp)),
                        ..Default::default()
                    },
                )?;
            }
            Some(Content::SystemFactoryResetRequest(_)) => {
                self.with_storage(|s| s.factory_reset());
                return Ok(false);
            }
            _ => send(io, error(CommandStatus::ErrorNotImplemented))?,
        }
        Ok(true)
    }

    /// A storage request other than a write: like the firmware, it first ends a write in
    /// progress (answering that write "interrupted"), then runs `f` on the storage.
    fn storage_request<R>(&self, io: &mut impl Write, f: impl FnOnce(&storage::Storage) -> R) -> io::Result<R> {
        let (interrupted, r) = self.with_storage(|s| (s.reset(true), f(s)));
        answer_all(io, interrupted.into_iter().collect())?;
        Ok(r)
    }

    fn list(&self, io: &mut impl Write, id: u32, path: &str) -> io::Result<()> {
        let file =
            |name: &str, dir: bool, size: u32| pb_storage::File { r#type: dir as i32, name: name.to_string(), size, ..Default::default() };
        let frame = |files: Vec<pb_storage::File>, has_next: bool| Main {
            command_id: id,
            has_next,
            content: Some(Content::StorageListResponse(pb_storage::ListResponse { file: files })),
            ..Default::default()
        };
        if path == "/" {
            let files = ["any", "int", "ext"].iter().map(|n| file(n, true, 0)).collect();
            return send(io, frame(files, false));
        }
        let nodes = self.with_storage(|s| s.nodes.clone());
        if !nodes.iter().any(|n| n.path == path && n.dir) {
            let status = CommandStatus::ErrorStorageNotExist as i32;
            return send(
                io,
                Main { command_id: id, command_status: status, content: Some(Content::Empty(Empty {})), ..Default::default() },
            );
        }
        let prefix = format!("{path}/");
        let children: Vec<pb_storage::File> = nodes
            .iter()
            .filter_map(|n| n.path.strip_prefix(&prefix).filter(|rest| !rest.contains('/')).map(|name| (n, name)))
            .filter(|(_, name)| name.is_ascii())
            .map(|(n, name)| file(name, n.dir, n.size))
            .chain(self.hostile.iter().filter(|(d, _, _)| d == path).map(|(_, name, dir)| file(name, *dir, 1)))
            .collect();
        let mut chunks = children.chunks(8).peekable();
        if chunks.peek().is_none() {
            return send(io, frame(Vec::new(), false));
        }
        // As the firmware: a frame goes out with has_next when a ninth entry turns up, the
        // last frame (1 to 8 entries) without it.
        while let Some(c) = chunks.next() {
            send(io, frame(c.to_vec(), chunks.peek().is_some()))?;
        }
        Ok(())
    }
}

/// One line per request for transcripts: the command id, the request type and its path or key
/// (`7 StorageListRequest /ext`). Lets two clients' request sequences be compared.
pub fn describe(m: &Main) -> String {
    let arg = match &m.content {
        Some(Content::StorageListRequest(r)) => r.path.clone(),
        Some(Content::StorageStatRequest(r)) => r.path.clone(),
        Some(Content::StorageReadRequest(r)) => r.path.clone(),
        Some(Content::StorageInfoRequest(r)) => r.path.clone(),
        Some(Content::StorageMd5sumRequest(r)) => r.path.clone(),
        Some(Content::PropertyGetRequest(r)) => r.key.clone(),
        Some(Content::StorageMkdirRequest(r)) => r.path.clone(),
        Some(Content::StorageDeleteRequest(r)) => format!("{}{}", r.path, if r.recursive { " recursive" } else { "" }),
        Some(Content::StorageRenameRequest(r)) => format!("{} -> {}", r.old_path, r.new_path),
        Some(Content::StorageWriteRequest(r)) => {
            let data = r.file.as_ref().map(|f| f.data.as_slice()).unwrap_or_default();
            use md5::Digest;
            let sum: String = md5::Md5::digest(data).iter().take(4).map(|b| format!("{b:02x}")).collect();
            let file = if r.file.is_some() { format!("{} bytes {sum}", data.len()) } else { "no file".into() };
            format!("{} {file}{}", r.path, if m.has_next { " next" } else { "" })
        }
        Some(Content::SystemPingRequest(r)) => format!("{} bytes", r.data.len()),
        Some(Content::SystemRebootRequest(r)) => format!("mode {}", r.mode),
        Some(Content::SystemUpdateRequest(r)) => r.update_manifest.clone(),
        _ => String::new(),
    };
    let name = match &m.content {
        None => "None".to_string(),
        Some(c) => format!("{c:?}").split('(').next().unwrap_or("?").to_string(),
    };
    format!("{} {name} {}", m.command_id, arg.escape_debug()).trim_end().to_string()
}

fn record(t: &mut Transcript, data: &[u8]) {
    let room = MAX_RECORD.saturating_sub(t.received.len());
    t.received.extend_from_slice(&data[..data.len().min(room)]);
}

/// The firmware's answer for an empty file, byte for byte: a `file` whose `data` is present
/// with length 0 (nanopb sends the set pointer). prost would leave the empty field out, and
/// the C++ app dereferences a missing `data` (plugins/flipperproto0/storageresponse.cpp), so the
/// fake must send what the firmware sends. Same bytes as `resp_read_empty_file` in
/// flipper-rpc's nanopb fixture.
pub fn empty_file_read_response(id: u32) -> Vec<u8> {
    let varint = |mut v: u64, out: &mut Vec<u8>| loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    };
    let mut main = vec![0x08];
    varint(u64::from(id), &mut main);
    // storage_read_response (10) { file (1) { data (4): "" } }
    main.extend([0x52, 0x04, 0x0a, 0x02, 0x22, 0x00]);
    let mut frame = Vec::new();
    varint(main.len() as u64, &mut frame);
    frame.extend(main);
    frame
}

fn send_raw(io: &mut impl Write, frame: &[u8]) -> io::Result<()> {
    io.write_all(frame)?;
    io.flush()
}

/// Sends storage answers: empty content with the status, as `rpc_send_and_release_empty`.
fn answer_all(io: &mut impl Write, answers: Vec<storage::Answer>) -> io::Result<()> {
    for (id, status) in answers {
        send(io, Main { command_id: id, command_status: status as i32, content: Some(Content::Empty(Empty {})), ..Default::default() })?;
    }
    Ok(())
}

fn send(io: &mut impl Write, msg: Main) -> io::Result<()> {
    let f = encode_frame(&msg).map_err(|e| io::Error::other(e.to_string()))?;
    io.write_all(&f)?;
    io.flush()
}

/// Serving over a pseudo-terminal, the way a client meets a real Flipper's serial port.
pub mod pty {
    use std::io::{self, Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    use serialport::SerialPort;

    use super::{FakeFlipper, Transcript};

    /// How long the fake waits for a client to open the port, and for a quiet client to speak.
    pub const WAIT: Duration = Duration::from_secs(60);

    /// The master side of the pseudo-terminal as a byte stream for `FakeFlipper::serve`: read
    /// timeouts are waited through (up to [`WAIT`] of silence), and a hang-up, which the master reports
    /// once the client has closed the port (EIO or BrokenPipe), is the end of the stream.
    pub struct PtyMaster<'a> {
        pub port: &'a mut serialport::TTYPort,
    }

    impl Read for PtyMaster<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let start = Instant::now();
            loop {
                match self.port.read(buf) {
                    Ok(n) => return Ok(n),
                    Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                        if start.elapsed() > WAIT {
                            return Ok(0);
                        }
                    }
                    // The client closed the port: the master reports EIO, or serialport reports the
                    // hang-up its poll saw as BrokenPipe.
                    Err(e) if e.raw_os_error() == Some(5) || e.kind() == io::ErrorKind::BrokenPipe => return Ok(0),
                    Err(e) => return Err(e),
                }
            }
        }
    }

    impl Write for PtyMaster<'_> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.port.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.port.flush()
        }
    }

    /// Waits until a client has the slave side open (the master stops reporting a hang-up).
    pub fn wait_for_client(port: &serialport::TTYPort) -> bool {
        use nix::poll::{poll, PollFd, PollFlags};
        let start = Instant::now();
        while start.elapsed() < WAIT {
            let mut fds = [PollFd::new(port.as_raw_fd(), PollFlags::POLLIN)];
            match poll(&mut fds, 50) {
                Ok(_) if fds[0].revents().is_some_and(|r| r.contains(PollFlags::POLLHUP)) => std::thread::sleep(Duration::from_millis(20)),
                Ok(_) => return true,
                Err(_) => return false,
            }
        }
        false
    }

    /// After the device left (reboot, factory reset): waits until the client has closed the port,
    /// so that the next session starts with the next open, as a device back on USB would. A
    /// pseudo-terminal cannot vanish the way a USB port does, so a client waiting for the device
    /// to leave sees it only when the fake exits.
    pub fn wait_for_close(port: &serialport::TTYPort) {
        use nix::poll::{poll, PollFd, PollFlags};
        let start = Instant::now();
        while start.elapsed() < WAIT {
            let mut fds = [PollFd::new(port.as_raw_fd(), PollFlags::empty())];
            match poll(&mut fds, 50) {
                Ok(_) if fds[0].revents().is_some_and(|r| r.contains(PollFlags::POLLHUP)) => return,
                Ok(_) => {}
                Err(_) => return,
            }
        }
    }

    /// Makes a pseudo-terminal, calls `ready` with its path, then serves `sessions` clients one
    /// after the other (the C++ app opens the port twice: device information, then the work).
    /// Like a real Flipper, the banner goes out when a client opens the port.
    pub fn serve_pty(fake: &FakeFlipper, sessions: usize, ready: impl FnOnce(&str)) -> (Vec<Transcript>, io::Result<()>) {
        let mut all = Vec::new();
        let r = (|| {
            let (mut master, slave) = serialport::TTYPort::pair().map_err(io::Error::from)?;
            let name = slave.name().unwrap_or_default();
            drop(slave);
            ready(&name);
            for _ in 0..sessions {
                if !wait_for_client(&master) {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "no client opened the port"));
                }
                // Let the client finish setting up the port before the banner.
                std::thread::sleep(Duration::from_millis(200));
                let t = fake.serve(&mut PtyMaster { port: &mut master })?;
                let left = t.left;
                all.push(t);
                if left {
                    wait_for_close(&master);
                }
            }
            Ok(())
        })();
        (all, r)
    }
}
