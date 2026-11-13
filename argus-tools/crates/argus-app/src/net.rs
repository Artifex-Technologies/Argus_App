//! The update server, off the UI thread: the firmware directory, an update package and the region data, each fetched
//! with `flipper_updates::fetch` (HTTPS only, bounded, SHA-256 checked). The app fetches only when asked: the
//! directory when a device connects (Settings can turn that off) or on "Try again", files when an update was
//! confirmed. The tests point it at `flipper_fake::update_server` on 127.0.0.1 with a CA of their own.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use flipper_updates::directory::{self, Directory};
use flipper_updates::fetch::Client;
use flipper_updates::region;

use crate::device::{UpdateJob, Waker};

/// Where an update package comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackageSource {
    /// The latest version of a channel on the update server (its `update_tgz` for target f7).
    Server { channel: String },
    /// A `.tgz` the user named.
    File(PathBuf),
}

pub enum NetEvent {
    Directory(Result<Directory, String>),
    /// An update ready for the device (package checked, region data fetched or why not).
    UpdateReady(Result<Box<UpdateJob>, String>),
    /// The legacy update's files, fetched and checked.
    LegacyReady(Result<Box<crate::recovery::LegacyFiles>, String>),
}

/// The update server and the client that reaches it; the client is made on first use (system roots) unless one was
/// given (the tests' own CA).
#[derive(Clone)]
pub struct Net {
    pub server: String,
    client: Arc<std::sync::Mutex<Option<Arc<Client>>>>,
}

impl Net {
    pub fn new(server: String, client: Option<Client>) -> Net {
        Net { server, client: Arc::new(std::sync::Mutex::new(client.map(Arc::new))) }
    }

    fn client(&self) -> Result<Arc<Client>, String> {
        let mut c = self.client.lock().unwrap_or_else(|e| e.into_inner());
        if c.is_none() {
            *c = Some(Arc::new(Client::new().map_err(|e| e.to_string())?));
        }
        Ok(c.clone().expect("set above"))
    }

    fn directory(&self) -> Result<Directory, String> {
        let text =
            self.client()?.get(&flipper_updates::directory_url(&self.server), directory::MAX_DIRECTORY).map_err(|e| e.to_string())?;
        directory::parse(&text).map_err(|e| e.to_string())
    }

    /// Reads the directory on a thread of its own.
    pub fn fetch_directory(&self, tx: Sender<NetEvent>, waker: Waker) {
        let net = self.clone();
        std::thread::spawn(move || {
            let _ = tx.send(NetEvent::Directory(net.directory()));
            waker();
        });
    }

    fn package(&self, source: &PackageSource) -> Result<flipper_rpc::update::UpdatePackage, String> {
        let limits = flipper_rpc::update::PackageLimits::default();
        let refused = |e: flipper_rpc::restore::ArchiveError| e.to_string().replace("backup refused", "update package refused");
        match source {
            PackageSource::File(path) => {
                let data = crate::device::read_local(path, flipper_updates::fetch::MAX_FILE)?;
                flipper_rpc::update::read_update_package(&data[..], &limits).map_err(refused)
            }
            PackageSource::Server { channel } => {
                let dir = self.directory()?;
                let version = dir.channel(channel).and_then(|c| c.latest()).ok_or(format!("the update server has no {channel} version"))?;
                let file = version.file("update_tgz", "f7").ok_or(format!("version {} has no update package", version.number))?;
                let data = self.client()?.get_file(file, flipper_updates::fetch::MAX_FILE).map_err(|e| e.to_string())?;
                flipper_rpc::update::read_update_package(&data[..], &limits).map_err(refused)
            }
        }
    }

    /// The files of a repair from the latest version of `channel` (`flipper_updates::repair::fetch`).
    pub fn repair_files(&self, channel: &str) -> Result<flipper_updates::repair::RepairFiles, String> {
        let dir = self.directory()?;
        let client = self.client()?;
        flipper_updates::repair::fetch(&client, &dir, channel)
    }

    /// The region data for a device not of the development region (the default bands when the server names no
    /// country).
    pub fn region(&self) -> Result<Vec<u8>, String> {
        let text = self.client()?.get(&flipper_updates::region_url(&self.server), region::MAX_BUNDLE).map_err(|e| e.to_string())?;
        // no country from the server: the default bands (argus-cli never guesses a country either)
        region::region_data(&text, b"").map(|d| d.bytes).map_err(|e| e.to_string())
    }

    /// Fetches the legacy update's files on a thread (`LegacyFiles::fetch`).
    pub fn prepare_legacy(&self, channel: String, info: &crate::device::DeviceInfo, tx: Sender<NetEvent>, waker: Waker) {
        let net = self.clone();
        let (version, stack_type, region) = (info.radio_version.clone(), info.stack_type, info.region);
        std::thread::spawn(move || {
            let r = crate::recovery::LegacyFiles::fetch(&net, &channel, &version, stack_type, region).map(Box::new);
            let _ = tx.send(NetEvent::LegacyReady(r));
            waker();
        });
    }

    /// Prepares an update on a thread: the package (read and checked whole before anything goes to the device), then,
    /// for a device not of the development region, its region data (a failure there is carried along, as the C++ app
    /// goes on without it).
    pub fn prepare_update(&self, source: PackageSource, device_region: u32, tx: Sender<NetEvent>, waker: Waker) {
        let net = self.clone();
        std::thread::spawn(move || {
            let r = net.package(&source).map(|package| {
                let region = (device_region != 0).then(|| net.region());
                Box::new(UpdateJob { package, region })
            });
            let _ = tx.send(NetEvent::UpdateReady(r));
            waker();
        });
    }
}
