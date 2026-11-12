//! The Rust encoding against frames produced by the C++ app's own nanopb code
//! (tests/fixtures/nanopb_frames.txt, written by tests/nanopb/gen.sh; CI regenerates it and
//! fails if it changes). Requests must be byte-identical to what the C++ app sends; responses
//! (encoded the way the firmware encodes them) must decode to the values the driver put in.

use std::collections::BTreeMap;

use flipper_rpc::frame::{encode_frame, FrameDecoder};
use flipper_rpc::proto::pb::main::Content;
use flipper_rpc::proto::pb::{CommandStatus, Empty, Main};
use flipper_rpc::proto::{pb_gui, pb_property, pb_storage, pb_system};

fn fixtures() -> BTreeMap<String, Vec<u8>> {
    let text = include_str!("fixtures/nanopb_frames.txt");
    text.lines()
        .map(|l| {
            let (name, hex) = l.split_once(' ').expect("name hex");
            let bytes = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
            (name.to_string(), bytes)
        })
        .collect()
}

fn req(id: u32, content: Content) -> Main {
    Main { command_id: id, content: Some(content), ..Default::default() }
}

/// A write frame the way `flipper_rpc::write::write_frames` builds it.
fn write(id: u32, data: &[u8], has_next: bool) -> Main {
    let frames = flipper_rpc::write::write_frames("/int/favorites.txt", data);
    let (content, _) = frames.into_iter().next().flatten().expect("one chunk");
    Main { command_id: id, has_next, content: Some(content), ..Default::default() }
}

fn decode_one(bytes: &[u8]) -> Main {
    let mut d = FrameDecoder::new();
    d.push(bytes).unwrap();
    let m = d.next_frame().unwrap().expect("a complete frame");
    assert_eq!(d.pending(), 0, "the frame is exactly the fixture's length");
    m
}

fn path(p: &str) -> String {
    p.to_string()
}

#[test]
fn requests_are_byte_identical_to_the_cpp_app() {
    let f = fixtures();
    let list = |id, p: &str| req(id, Content::StorageListRequest(pb_storage::ListRequest { path: path(p), ..Default::default() }));
    let cases: Vec<(&str, Main)> = vec![
        ("req_protobuf_version", req(1, Content::SystemProtobufVersionRequest(pb_system::ProtobufVersionRequest {}))),
        ("req_property_devinfo", req(2, Content::PropertyGetRequest(pb_property::GetRequest { key: "devinfo".into() }))),
        ("req_device_info", req(3, Content::SystemDeviceInfoRequest(pb_system::DeviceInfoRequest {}))),
        ("req_list_ext", list(4, "/ext")),
        ("req_list_root", list(5, "/")),
        ("req_list_subghz_id300", list(300, "/ext/subghz")),
        ("req_info_ext", req(6, Content::StorageInfoRequest(pb_storage::InfoRequest { path: path("/ext") }))),
        ("req_stat_manifest", req(7, Content::StorageStatRequest(pb_storage::StatRequest { path: path("/ext/Manifest") }))),
        ("req_md5_manifest", req(8, Content::StorageMd5sumRequest(pb_storage::Md5sumRequest { path: path("/ext/Manifest") }))),
        ("req_read_manifest", req(9, Content::StorageReadRequest(pb_storage::ReadRequest { path: path("/ext/Manifest") }))),
        ("req_ping_abc", req(10, Content::SystemPingRequest(pb_system::PingRequest { data: b"abc".to_vec() }))),
        ("req_get_datetime", req(12, Content::SystemGetDatetimeRequest(pb_system::GetDateTimeRequest {}))),
        ("req_protobuf_version_idmax", req(u32::MAX, Content::SystemProtobufVersionRequest(pb_system::ProtobufVersionRequest {}))),
        ("req_read_pair_stat", req(13, Content::StorageStatRequest(pb_storage::StatRequest { path: path("/int/favorites.txt") }))),
        ("req_read_pair_read", req(13, Content::StorageReadRequest(pb_storage::ReadRequest { path: path("/int/favorites.txt") }))),
        ("req_write_chunk_next", write(20, b"hello", true)),
        ("req_write_chunk_last", write(20, b"!", false)),
        ("req_write_empty_file", write(21, b"", false)),
        ("req_ping_empty", req(20, Content::SystemPingRequest(pb_system::PingRequest { data: Vec::new() }))),
        ("req_mkdir_sub", req(22, Content::StorageMkdirRequest(pb_storage::MkdirRequest { path: path("/int/sub") }))),
        ("req_delete_sub", req(23, Content::StorageDeleteRequest(pb_storage::DeleteRequest { path: path("/int/sub"), recursive: false }))),
        (
            "req_delete_sub_recursive",
            req(24, Content::StorageDeleteRequest(pb_storage::DeleteRequest { path: path("/int/sub"), recursive: true })),
        ),
        (
            "req_rename",
            req(
                25,
                Content::StorageRenameRequest(pb_storage::RenameRequest {
                    old_path: path("/int/favorites.txt"),
                    new_path: path("/int/renamed.txt"),
                }),
            ),
        ),
        (
            "req_reboot_os",
            req(26, Content::SystemRebootRequest(pb_system::RebootRequest { mode: pb_system::reboot_request::RebootMode::Os as i32 })),
        ),
        (
            "req_reboot_update",
            req(27, Content::SystemRebootRequest(pb_system::RebootRequest { mode: pb_system::reboot_request::RebootMode::Update as i32 })),
        ),
        ("req_factory_reset", req(28, Content::SystemFactoryResetRequest(pb_system::FactoryResetRequest {}))),
        (
            "req_update",
            req(
                29,
                Content::SystemUpdateRequest(pb_system::UpdateRequest { update_manifest: path("/ext/update/f7-update-test/update.fuf") }),
            ),
        ),
        ("req_screen_start", req(30, Content::GuiStartScreenStreamRequest(pb_gui::StartScreenStreamRequest {}))),
        ("req_screen_stop", req(31, Content::GuiStopScreenStreamRequest(pb_gui::StopScreenStreamRequest {}))),
        (
            "req_input_ok_short",
            req(
                32,
                Content::GuiSendInputEventRequest(pb_gui::SendInputEventRequest {
                    key: pb_gui::InputKey::Ok as i32,
                    r#type: pb_gui::InputType::Short as i32,
                }),
            ),
        ),
        (
            "req_input_up_press",
            req(
                33,
                Content::GuiSendInputEventRequest(pb_gui::SendInputEventRequest {
                    key: pb_gui::InputKey::Up as i32,
                    r#type: pb_gui::InputType::Press as i32,
                }),
            ),
        ),
    ];
    for (name, msg) in cases {
        let want = f.get(name).unwrap_or_else(|| panic!("fixture {name} missing"));
        assert_eq!(&encode_frame(&msg).unwrap(), want, "{name}");
    }
}

#[test]
fn empty_path_differs_on_the_wire_but_not_in_meaning() {
    // nanopb sends a set-but-empty pointer string (the C++ app's QByteArray is never NULL);
    // proto3 encoders, prost included, leave empty strings out. Both decode to the same message.
    // The Rust operations never send an empty path (ops::check_path refuses it).
    let f = fixtures();
    let cpp = &f["req_list_empty_path"];
    let rust = encode_frame(&req(11, Content::StorageListRequest(pb_storage::ListRequest::default()))).unwrap();
    assert_ne!(&rust, cpp);
    assert_eq!(decode_one(cpp), decode_one(&rust));
}

#[test]
fn firmware_style_responses_decode_and_re_encode_identically() {
    let f = fixtures();
    let file = |dir: bool, name: &str, size: u32| pb_storage::File { r#type: dir as i32, name: name.into(), size, ..Default::default() };
    let cases: Vec<(&str, Main)> = vec![
        (
            "resp_protobuf_version",
            req(1, Content::SystemProtobufVersionResponse(pb_system::ProtobufVersionResponse { major: 0, minor: 25 })),
        ),
        (
            "resp_property_pair",
            Main {
                has_next: true,
                ..req(2, Content::PropertyGetResponse(pb_property::GetResponse { key: "hardware.name".into(), value: "Fakeflip".into() }))
            },
        ),
        ("resp_property_end", req(2, Content::Empty(Empty {}))),
        (
            "resp_device_info_pair",
            req(
                3,
                Content::SystemDeviceInfoResponse(pb_system::DeviceInfoResponse { key: "hardware_name".into(), value: "Fakeflip".into() }),
            ),
        ),
        (
            "resp_list_three",
            Main {
                has_next: true,
                ..req(
                    4,
                    Content::StorageListResponse(pb_storage::ListResponse {
                        file: vec![file(true, "apps", 0), file(false, "Manifest", 48213), file(false, "signal_00.sub", 100)],
                    }),
                )
            },
        ),
        ("resp_list_end_empty", req(4, Content::StorageListResponse(pb_storage::ListResponse { file: vec![] }))),
        ("resp_error_not_exist", Main { command_status: CommandStatus::ErrorStorageNotExist as i32, ..req(5, Content::Empty(Empty {})) }),
        (
            "resp_info_ext",
            req(6, Content::StorageInfoResponse(pb_storage::InfoResponse { total_space: 31_902_400_512, free_space: 30_000_000_000 })),
        ),
        ("resp_stat_manifest", req(7, Content::StorageStatResponse(pb_storage::StatResponse { file: Some(file(false, "", 48213)) }))),
        ("resp_ping_abc", req(10, Content::SystemPingResponse(pb_system::PingResponse { data: b"abc".to_vec() }))),
        (
            "resp_md5",
            req(8, Content::StorageMd5sumResponse(pb_storage::Md5sumResponse { md5sum: "d41d8cd98f00b204e9800998ecf8427e".into() })),
        ),
        (
            "resp_read_chunk",
            Main {
                has_next: true,
                ..req(
                    9,
                    Content::StorageReadResponse(pb_storage::ReadResponse {
                        file: Some(pb_storage::File { data: b"hello".to_vec(), ..Default::default() }),
                    }),
                )
            },
        ),
        (
            "resp_datetime",
            req(
                12,
                Content::SystemGetDatetimeResponse(pb_system::GetDateTimeResponse {
                    datetime: Some(pb_system::DateTime { hour: 12, minute: 34, second: 56, day: 9, month: 11, year: 2026, weekday: 1 }),
                }),
            ),
        ),
    ];
    for (name, want) in cases {
        let bytes = f.get(name).unwrap_or_else(|| panic!("fixture {name} missing"));
        assert_eq!(decode_one(bytes), want, "{name} decodes");
        assert_eq!(&encode_frame(&want).unwrap(), bytes, "{name} re-encodes");
    }
}

#[test]
fn empty_file_read_differs_on_the_wire_but_not_in_meaning() {
    // The firmware sends an empty file as a set zero-length `data`; prost leaves it out.
    let f = fixtures();
    let cpp = &f["resp_read_empty_file"];
    let want = req(9, Content::StorageReadResponse(pb_storage::ReadResponse { file: Some(pb_storage::File::default()) }));
    assert_eq!(decode_one(cpp), want);
    assert_ne!(&encode_frame(&want).unwrap(), cpp);
}

#[test]
fn every_fixture_is_covered() {
    let f = fixtures();
    assert_eq!(f.len(), 46, "a frame was added to driver.c; add it to these tests");
}
