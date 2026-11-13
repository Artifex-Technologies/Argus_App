//! `argus-app`: the Argus desktop app in Rust, shipped next to the Qt app until I switch.
//!
//!   argus-app [--port DEVICE | --socket PATH | --dfu-socket PATH | --no-discovery] [--update-server URL]
//!             [--config-dir DIR] [--timeout MS]
//!
//! Without `--port`/`--socket`/`--dfu-socket` it finds Flipper Zeros by reading sysfs (nothing is opened until it
//! connects); with them it uses only the devices named. `--dfu-socket` names a bootloader on a Unix socket speaking
//! `flipper_dfu::line` (recovery mode; the app has no USB recovery transport). `--no-discovery` finds nothing at all.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use argus_app::discovery::{Discovery, Fixed, Found, Kind, Sysfs};
use argus_app::{app, net::Net, settings, theme, ArgusApp, Config};
use eframe::egui;

const USAGE: &str =
    "usage: argus-app [--port DEVICE | --socket PATH | --dfu-socket PATH | --no-discovery] [--update-server URL] [--config-dir DIR] [--timeout MS]";

fn main() -> ExitCode {
    let mut named: Option<Vec<Found>> = None;
    let mut server = flipper_updates::DEFAULT_SERVER.to_string();
    let mut config_dir = settings::default_dir();
    let mut timeout = Duration::from_secs(3);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{a} needs a value"));
        let r: Result<(), String> = (|| {
            match a.as_str() {
                "--port" => named.get_or_insert_with(Vec::new).push(Found { kind: Kind::Serial(PathBuf::from(value()?)) }),
                "--socket" => named.get_or_insert_with(Vec::new).push(Found { kind: Kind::Socket(PathBuf::from(value()?)) }),
                "--dfu-socket" => named.get_or_insert_with(Vec::new).push(Found { kind: Kind::RecoverySocket(PathBuf::from(value()?)) }),
                "--no-discovery" => {
                    named.get_or_insert_with(Vec::new);
                }
                "--update-server" => {
                    server = value()?;
                    if !server.starts_with("https://") {
                        return Err("the update server must be https://".into());
                    }
                }
                "--config-dir" => config_dir = Some(PathBuf::from(value()?)),
                "--timeout" => timeout = Duration::from_millis(value()?.parse().map_err(|_| "--timeout takes milliseconds")?),
                "-h" | "--help" => return Err(String::new()),
                other => return Err(format!("unknown argument {other}")),
            }
            Ok(())
        })();
        if let Err(e) = r {
            if !e.is_empty() {
                eprintln!("{e}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    }
    let Some(config_dir) = config_dir else {
        eprintln!("no settings folder (HOME or XDG_CONFIG_HOME)");
        return ExitCode::from(1);
    };
    let Some(data) = argus_api::bridge::data_home() else {
        eprintln!("no data folder (HOME or XDG_DATA_HOME)");
        return ExitCode::from(1);
    };
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let cfg = Config {
        discovery: match named {
            Some(list) => Box::new(Fixed(list)) as Box<dyn Discovery>,
            None => Box::new(Sysfs { sys: "/sys".into(), dev: "/dev".into() }),
        },
        config_dir,
        datastore: data.join("Argus/chymaera/chymaera.db"),
        library_dir: data.join("argus/captures"),
        net: Net::new(server, None),
        theme: None,
        yggdrasil_config: theme::yggdrasil_config(),
        timeout,
        home,
        api_token: String::new(),
        recovery_poll: Duration::from_secs(1),
        tools: argus_app::tools::Dirs {
            data: data.join("argus"),
            pulls: data.join("Argus/chymaera/pulls"),
            limbo_socket: PathBuf::from(argus_tools::limbo::DEFAULT_SOCKET),
        },
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(app::WINDOW)
            .with_min_inner_size(app::WINDOW)
            .with_title("Argus")
            .with_app_id("argus-app")
            // draws its own title strip, as Yggdrasil's apps do
            .with_decorations(false),
        ..Default::default()
    };
    struct Shell(ArgusApp);
    impl eframe::App for Shell {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            self.0.ui(ui);
        }
    }
    match eframe::run_native("Argus", options, Box::new(|_| Ok(Box::new(Shell(ArgusApp::new(cfg)))))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}
