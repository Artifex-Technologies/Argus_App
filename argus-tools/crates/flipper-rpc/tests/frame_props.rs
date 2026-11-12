//! Property tests for the frame decoder: any bytes, cut anywhere, never panic, never buffer
//! more than the limit, and decode the same however the stream is split. Inputs stay small
//! (at most a few KiB per case) so the run is quick and bounded.

use proptest::prelude::*;

use flipper_rpc::frame::{encode_frame, FrameDecoder, FrameError, MAX_FRAME_LEN};
use flipper_rpc::proto::pb::main::Content;
use flipper_rpc::proto::pb::Main;
use flipper_rpc::proto::{pb_property, pb_storage};

/// Feeds `data` in pieces of the given sizes; returns the frames and the first error.
fn feed(data: &[u8], cuts: &[usize]) -> (Vec<Main>, Option<FrameError>) {
    let mut d = FrameDecoder::new();
    let mut out = Vec::new();
    let mut pos = 0;
    let mut i = 0;
    while pos < data.len() {
        let n = cuts.get(i).copied().unwrap_or(data.len()).clamp(1, data.len() - pos);
        i += 1;
        if let Err(e) = d.push(&data[pos..pos + n]) {
            return (out, Some(e));
        }
        pos += n;
        loop {
            match d.next_frame() {
                Ok(Some(m)) => out.push(m),
                Ok(None) => break,
                Err(e) => return (out, Some(e)),
            }
        }
        assert!(d.pending() <= MAX_FRAME_LEN + 5 + n, "buffer holds at most one partial frame plus the last push");
    }
    (out, None)
}

fn file() -> impl Strategy<Value = pb_storage::File> {
    (any::<bool>(), "[ -~]{0,40}", any::<u32>(), proptest::collection::vec(any::<u8>(), 0..64), "[0-9a-f]{0,32}")
        .prop_map(|(dir, name, size, data, md5sum)| pb_storage::File { r#type: dir as i32, name, size, data, md5sum })
}

fn message() -> impl Strategy<Value = Main> {
    let content = prop_oneof![
        proptest::collection::vec(file(), 0..8).prop_map(|file| Content::StorageListResponse(pb_storage::ListResponse { file })),
        ("\\PC{0,30}", "\\PC{0,60}").prop_map(|(key, value)| Content::PropertyGetResponse(pb_property::GetResponse { key, value })),
        file().prop_map(|f| Content::StorageReadResponse(pb_storage::ReadResponse { file: Some(f) })),
    ];
    (any::<u32>(), 0i32..30, any::<bool>(), proptest::option::of(content))
        .prop_map(|(command_id, command_status, has_next, content)| Main { command_id, command_status, has_next, content })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn arbitrary_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..4096), cuts in proptest::collection::vec(1usize..64, 0..64)) {
        let _ = feed(&data, &cuts);
    }

    #[test]
    fn messages_round_trip_however_split(msgs in proptest::collection::vec(message(), 1..6), cuts in proptest::collection::vec(1usize..32, 0..128)) {
        let stream: Vec<u8> = msgs.iter().flat_map(|m| encode_frame(m).unwrap()).collect();
        let (whole, err) = feed(&stream, &[stream.len()]);
        prop_assert!(err.is_none());
        prop_assert_eq!(&whole, &msgs);
        let (split, err) = feed(&stream, &cuts);
        prop_assert!(err.is_none());
        prop_assert_eq!(split, msgs);
    }

    #[test]
    fn a_valid_stream_followed_by_junk_keeps_its_frames(msgs in proptest::collection::vec(message(), 1..4), junk in proptest::collection::vec(any::<u8>(), 1..256)) {
        let mut stream: Vec<u8> = msgs.iter().flat_map(|m| encode_frame(m).unwrap()).collect();
        stream.extend(&junk);
        let (got, _) = feed(&stream, &[7]);
        prop_assert!(got.len() >= msgs.len());
        prop_assert_eq!(&got[..msgs.len()], &msgs[..]);
    }
}
