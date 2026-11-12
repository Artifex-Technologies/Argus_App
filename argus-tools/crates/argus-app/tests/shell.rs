//! The window without and with a device: waiting, connecting to the fake, the device information, the live screen and
//! the keypad (the inputs qFlipper's KeypadButton sends), and the status bar. Snapshots of the key screens.

mod common;

use common::*;
use eframe::egui::Key;
use flipper_fake::FakeFlipper;
use flipper_rpc::proto::pb::main::Content;
use flipper_rpc::proto::pb_gui::{InputKey, InputType};

fn inputs(fake: &Fake) -> Vec<(InputKey, InputType)> {
    fake.requests()
        .iter()
        .filter_map(|m| match &m.content {
            Some(Content::GuiSendInputEventRequest(r)) => {
                Some((InputKey::try_from(r.key).unwrap(), InputType::try_from(r.r#type).unwrap()))
            }
            _ => None,
        })
        .collect()
}

fn connected(name: &str, fake: FakeFlipper) -> (Harness, Fake) {
    let dir = tempdir(name);
    let fake = Fake::start(&dir, fake);
    let mut h = Harness::new(&dir, vec![socket_device(&fake.sock)], Options::default());
    h.wait("the device's name", |h| h.shows("Fakeflip"));
    (h, fake)
}

#[test]
fn without_a_device_it_waits() {
    let dir = tempdir("nodevice");
    let mut h = Harness::new(&dir, vec![], Options::default());
    h.settle();
    assert!(h.shows("Connect your Flipper Zero"));
    assert!(h.shows("Waiting for devices ..."));
    h.snapshot("no-device");
}

#[test]
fn the_light_style_follows_the_setting() {
    let dir = tempdir("light");
    let mut h = Harness::new(&dir, vec![], Options { dark: false, ..Options::default() });
    h.settle();
    h.snapshot("no-device-light");
}

#[test]
fn it_connects_reads_the_device_and_changes_nothing() {
    let (mut h, fake) = connected("home", FakeFlipper { screen_frames: 1, ..FakeFlipper::standard() });
    h.wait("the live screen", |h| h.has("Open the screen"));
    h.settle();
    for text in
        ["Connected", "socket flipper.sock", "ARG-1.5.1r2", "9/11/26", "94% Free", "Installed", "12.f7b9c6", "1.17.3 Lite", "Ready."]
    {
        assert!(h.shows(text), "{text} is shown");
    }
    h.snapshot("home-info");
    // what it asked: the C++ helper's reads without the clock, then the screen; nothing that changes the device
    let kinds: Vec<String> = fake.requests().iter().map(flipper_fake::describe).map(|l| l.split(' ').nth(1).unwrap().to_string()).collect();
    assert_eq!(
        &kinds[..5],
        ["SystemProtobufVersionRequest", "PropertyGetRequest", "StorageInfoRequest", "StorageStatRequest", "GuiStartScreenStreamRequest"]
    );
    assert!(!kinds.iter().any(|k| k.contains("Datetime") && k.contains("Set")), "the clock is never set");
    assert!(fake.changes().is_empty());
}

#[test]
fn the_screen_opens_full_size_and_the_keypad_sends_qflippers_inputs() {
    let (mut h, fake) = connected("screen", FakeFlipper::standard());
    h.wait("the live screen", |h| h.has("Open the screen"));
    h.click("Open the screen");
    h.settle();
    assert!(h.has("Save Screenshot"));
    h.snapshot("screen");

    // a click: press, short, release
    h.click("OK");
    h.wait("three inputs", |_| inputs(&fake).len() >= 3);
    assert_eq!(inputs(&fake), vec![(InputKey::Ok, InputType::Press), (InputKey::Ok, InputType::Short), (InputKey::Ok, InputType::Release)]);

    // a hold: press, long after 350 ms, a repeat every 150 ms, release (no short)
    let p = h.press("Up");
    for _ in 0..36 {
        h.step(); // 0.6 s of the app's clock
    }
    h.release(p);
    h.wait("the hold's inputs", |_| inputs(&fake).last() == Some(&(InputKey::Up, InputType::Release)));
    let up: Vec<InputType> = inputs(&fake).into_iter().skip(3).map(|(_, t)| t).collect();
    assert_eq!(up[0], InputType::Press);
    assert_eq!(up[1], InputType::Long);
    assert!(up[2..up.len() - 1].iter().all(|t| *t == InputType::Repeat) && up.len() >= 4, "{up:?}");
    assert!(!up.contains(&InputType::Short));

    // the keyboard, as qFlipper's: arrows, and Backspace for back
    let before = inputs(&fake).len();
    h.key(Key::ArrowLeft, true);
    h.key(Key::ArrowLeft, false);
    h.key(Key::Backspace, true);
    h.key(Key::Backspace, false);
    h.wait("the keyboard's inputs", |_| inputs(&fake).len() >= before + 6);
    let keys: Vec<(InputKey, InputType)> = inputs(&fake)[before..].to_vec();
    assert_eq!(
        keys,
        vec![
            (InputKey::Left, InputType::Press),
            (InputKey::Left, InputType::Short),
            (InputKey::Left, InputType::Release),
            (InputKey::Back, InputType::Press),
            (InputKey::Back, InputType::Short),
            (InputKey::Back, InputType::Release),
        ]
    );
    h.click("Back");
    h.settle();
    assert!(h.has("Open the screen"), "home again");
    assert!(fake.changes().is_empty(), "the keypad presses buttons; it changes no files");
}

#[test]
fn a_screenshot_is_saved_only_after_its_dialog() {
    let (mut h, _fake) = connected("shot", FakeFlipper::standard());
    h.wait("the live screen", |h| h.has("Open the screen"));
    h.click("Open the screen");
    h.settle();
    let out = h.dir.join("home/shot.png");
    h.click("Save Screenshot");
    h.set_field(0, out.to_str().unwrap());
    h.click("Cancel");
    h.settle();
    assert!(!out.exists());
    h.click("Save Screenshot");
    h.set_field(0, out.to_str().unwrap());
    h.click("Save");
    h.settle();
    let img = read_png(&out).expect("a PNG");
    assert_eq!(img.size, [512, 256], "four times the screen, as qFlipper's");
}

#[test]
fn a_device_that_leaves_is_waited_for_again() {
    let dir = tempdir("leave");
    // the fake never prints its prompt: the connection fails and the app says so
    let fake = Fake::start(&dir, FakeFlipper { fault: flipper_fake::Fault::NoPrompt, ..FakeFlipper::standard() });
    let mut h = Harness::new(&dir, vec![socket_device(&fake.sock)], Options::default());
    h.wait("the error", |h| h.shows("Cannot Connect"));
    h.settle();
    assert!(h.shows("Cannot Connect"));
    h.click("Back");
    h.settle();
    // not connected to by itself again (it failed); it is offered instead
    assert!(h.has("Connect"));
    assert_eq!(fake.sessions(), 1);
}
