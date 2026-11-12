//! Finding devices without opening them. The Qt app watches USB for Flipper's IDs and connects to what it finds; here a
//! scan only **reads sysfs** (`/sys/class/tty/ttyACM*` and `/sys/bus/usb/devices/*`): no device node is opened until
//! the user connects (or, with auto-connect on, until exactly one Flipper is found). The tests never scan the real
//! sysfs: they hand the app a fixed list (a fake Flipper on a socket or pseudo-terminal), or a sysfs tree of their own.

use std::path::{Path, PathBuf};

/// Flipper Zero's USB IDs: the serial port (CDC-ACM) and ST's DFU bootloader (recovery mode).
pub const VID: &str = "0483";
pub const PID_SERIAL: &str = "5740";
pub const PID_DFU: &str = "df11";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The Flipper's serial port (`/dev/ttyACMn`).
    Serial(PathBuf),
    /// A Unix socket carrying the same byte stream (a fake Flipper; `--socket`).
    Socket(PathBuf),
    /// A Flipper in recovery mode on USB. Its bootloader is not reachable from the app yet (no USB transport is wired
    /// in); recovery runs only through `--dfu-socket`.
    RecoveryUsb(String),
    /// A bootloader on a Unix socket (the fake; `--dfu-socket`), one request per line (`flipper_dfu::line`).
    RecoverySocket(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub kind: Kind,
}

impl Found {
    pub fn is_recovery(&self) -> bool {
        matches!(self.kind, Kind::RecoveryUsb(_) | Kind::RecoverySocket(_))
    }

    /// Where it is, as the Qt app's system location line shows it (a socket by its file name).
    pub fn location(&self) -> String {
        let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match &self.kind {
            Kind::Serial(p) => p.display().to_string(),
            Kind::Socket(p) | Kind::RecoverySocket(p) => format!("socket {}", name(p)),
            Kind::RecoveryUsb(bus) => format!("USB {bus}"),
        }
    }
}

pub trait Discovery: Send {
    fn scan(&self) -> Vec<Found>;
}

/// A fixed list: the device named on the command line, or the tests' fake.
pub struct Fixed(pub Vec<Found>);

impl Discovery for Fixed {
    fn scan(&self) -> Vec<Found> {
        self.0.clone()
    }
}

/// Reads sysfs under `sys` (normally `/sys`); device nodes are named under `dev` (normally `/dev`) but never opened here.
pub struct Sysfs {
    pub sys: PathBuf,
    pub dev: PathBuf,
}

fn read_id(dir: &Path, name: &str) -> Option<String> {
    let mut s = std::fs::read_to_string(dir.join(name)).ok()?;
    s.truncate(s.trim_end().len());
    Some(s.to_ascii_lowercase())
}

fn is_flipper(usb: &Path, pid: &str) -> bool {
    read_id(usb, "idVendor").as_deref() == Some(VID) && read_id(usb, "idProduct").as_deref() == Some(pid)
}

impl Discovery for Sysfs {
    fn scan(&self) -> Vec<Found> {
        let mut out = Vec::new();
        let mut ttys: Vec<String> = std::fs::read_dir(self.sys.join("class/tty"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("ttyACM") && n[6..].bytes().all(|b| b.is_ascii_digit()) && n.len() > 6)
            .collect();
        ttys.sort_by_key(|n| n[6..].parse::<u32>().unwrap_or(u32::MAX));
        for name in ttys {
            // class/tty/ttyACMn/device is the USB interface; its parent is the USB device with the IDs
            let Ok(iface) = std::fs::canonicalize(self.sys.join("class/tty").join(&name).join("device")) else { continue };
            if iface.parent().is_some_and(|usb| is_flipper(usb, PID_SERIAL)) {
                out.push(Found { kind: Kind::Serial(self.dev.join(&name)) });
            }
        }
        let mut dfu: Vec<String> = std::fs::read_dir(self.sys.join("bus/usb/devices"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| is_flipper(&e.path(), PID_DFU))
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        dfu.sort();
        out.extend(dfu.into_iter().map(|bus| Found { kind: Kind::RecoveryUsb(bus) }));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb(dir: &Path, vid: &str, pid: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("idVendor"), format!("{vid}\n")).unwrap();
        std::fs::write(dir.join("idProduct"), format!("{pid}\n")).unwrap();
    }

    #[test]
    fn a_sysfs_tree_gives_flippers_only_and_opens_nothing() {
        let root = std::env::temp_dir().join(format!("argus-app-sysfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let devices = root.join("devices/pci0/usb1");
        // a Flipper on 1-2 (ttyACM1), another serial device on 1-3 (ttyACM0), a Flipper in DFU on 1-4
        usb(&devices.join("1-2"), "0483", "5740");
        usb(&devices.join("1-3"), "2341", "0043");
        usb(&devices.join("1-4"), "0483", "DF11");
        std::fs::create_dir_all(devices.join("1-2/1-2:1.0")).unwrap();
        std::fs::create_dir_all(devices.join("1-3/1-3:1.0")).unwrap();
        for (tty, iface) in [("ttyACM1", "1-2/1-2:1.0"), ("ttyACM0", "1-3/1-3:1.0")] {
            let d = root.join("class/tty").join(tty);
            std::fs::create_dir_all(&d).unwrap();
            std::os::unix::fs::symlink(devices.join(iface), d.join("device")).unwrap();
        }
        std::fs::create_dir_all(root.join("class/tty/ttyS0")).unwrap();
        let bus = root.join("bus/usb/devices");
        std::fs::create_dir_all(&bus).unwrap();
        for d in ["1-2", "1-3", "1-4"] {
            std::os::unix::fs::symlink(devices.join(d), bus.join(d)).unwrap();
        }
        // the device nodes do not exist at all: a scan never looks at them
        let found = Sysfs { sys: root.clone(), dev: root.join("no-such-dev") }.scan();
        assert_eq!(
            found,
            vec![Found { kind: Kind::Serial(root.join("no-such-dev/ttyACM1")) }, Found { kind: Kind::RecoveryUsb("1-4".into()) }]
        );
        assert!(found[1].is_recovery() && !found[0].is_recovery());
        std::fs::remove_dir_all(root).unwrap();
    }
}
