//! The live screen as the app runs it, against the fake: the stream stays open between requests,
//! a quiet wait does not break the session, input events go out as the C++ encoder writes them
//! (`req_screen_start`, `req_input_ok_short`, ... in flipper-rpc's nanopb fixture) and bring the
//! screen the press leads to.

use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

use flipper_fake::{input_frame, screen_frame, FakeFlipper, Fault};
use flipper_rpc::frame::encode_frame;
use flipper_rpc::proto::pb::main::Content;
use flipper_rpc::proto::pb::Main;
use flipper_rpc::proto::pb_gui::{self, InputKey, InputType};
use flipper_rpc::Session;

fn fixture(name: &str) -> Vec<u8> {
    let text = include_str!("../../flipper-rpc/tests/fixtures/nanopb_frames.txt");
    let hex = text.lines().find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix(' '))).unwrap();
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
}

fn with_fake<R>(fake: FakeFlipper, f: impl FnOnce(&mut Session<UnixStream>) -> R) -> (R, flipper_fake::Transcript, FakeFlipper) {
    let (client, mut server) = UnixStream::pair().unwrap();
    let t = thread::spawn(move || {
        let tr = fake.serve(&mut server).unwrap();
        (tr, fake)
    });
    let mut s = Session::start(client, Duration::from_millis(500)).unwrap();
    let r = f(&mut s);
    drop(s);
    let (tr, fake) = t.join().unwrap();
    (r, tr, fake)
}

#[test]
fn the_stream_stays_open_and_input_brings_a_new_screen() {
    let fake = FakeFlipper { screen_frames: 2, ..FakeFlipper::standard() };
    let ((first, second, quiet, after, md5_ok), tr, fake) = with_fake(fake, |s| {
        s.screen_start().unwrap();
        let first = s.next_screen_frame(Duration::from_secs(2)).unwrap().unwrap();
        let second = s.next_screen_frame(Duration::from_secs(2)).unwrap().unwrap();
        // nothing more comes: a short wait gives None and the session stays usable
        let quiet = s.next_screen_frame(Duration::from_millis(50)).unwrap();
        s.gui_input(InputKey::Ok, InputType::Short).unwrap();
        let after = s.next_screen_frame(Duration::from_secs(2)).unwrap().unwrap();
        // another request while the stream runs still gets its answer
        let md5_ok = s.storage_md5("/int/favorites.txt").is_ok();
        s.screen_stop().unwrap();
        (first, second, quiet, after, md5_ok)
    });
    assert_eq!(first.data, screen_frame(0));
    assert_eq!(second.data, screen_frame(1));
    assert!(quiet.is_none());
    assert_eq!(after.data, input_frame(1));
    assert!(md5_ok);
    assert_eq!(*fake.inputs.borrow(), vec![(InputKey::Ok as i32, InputType::Short as i32)]);
    // the bytes the C++ encoder writes for the same requests (ids aside)
    let ids: Vec<u32> = tr.requests.iter().map(|m| m.command_id).collect();
    assert_eq!(ids, vec![1, 2, 3, 4]);
    let renumber = |name: &str, id: u32| {
        let want = fixture(name);
        let mut d = flipper_rpc::frame::FrameDecoder::new();
        d.push(&want).unwrap();
        let mut m: Main = d.next_frame().unwrap().unwrap();
        m.command_id = id;
        encode_frame(&m).unwrap()
    };
    let wire = &tr.received[b"start_rpc_session\r".len()..];
    let mut want = renumber("req_screen_start", 1);
    want.extend(renumber("req_input_ok_short", 2));
    assert!(wire.starts_with(&want), "the stream start and the input event are the C++ app's bytes");
    assert!(matches!(tr.requests[3].content, Some(Content::GuiStopScreenStreamRequest(_))));
}

#[test]
fn frames_of_the_wrong_length_are_skipped_and_a_refused_input_keeps_the_session() {
    let fake = FakeFlipper { fault: Fault::ShortFrame, ..FakeFlipper::standard() };
    let ((frame, refused, still), _, _) = with_fake(fake, |s| {
        s.screen_start().unwrap();
        let frame = s.next_screen_frame(Duration::from_secs(2)).unwrap().unwrap();
        // a key outside the enum, sent raw: the firmware (and the fake) refuse it
        let bad = pb_gui::SendInputEventRequest { key: 9, r#type: 0 };
        let refused = s.request(Content::GuiSendInputEventRequest(bad), |_| Ok(())).is_err();
        let still = s.protobuf_version().is_ok();
        (frame, refused, still)
    });
    assert_eq!(frame.data, screen_frame(0), "the 1023-byte frame before it was dropped");
    assert!(refused && still);
}
