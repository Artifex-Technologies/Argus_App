//! The app's settings: one small `key = value` file, `~/.config/argus/argus-app.conf` (the folder can be named with
//! `--config-dir`; the tests always name a private one). Unknown keys and bad values are ignored; a missing file is the
//! defaults. Written whole through a temporary file, 0600.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const FILE: &str = "argus-app.conf";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Follow Yggdrasil's Settings > Appearance.
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The firmware update channel (`release`, `release-candidate`, `development`), as the Qt app's preference.
    pub update_channel: String,
    /// Read the update directory when a Flipper connects (the Qt app always does).
    pub check_updates: bool,
    pub show_hidden: bool,
    /// Connect by itself when exactly one Flipper is found (the Qt app always does).
    pub auto_connect: bool,
    /// The Sarina control API's port.
    pub api_port: u16,
    /// Shows the developer tab (recovery tools), as the Qt app's developer mode.
    pub developer: bool,
    pub style: Style,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            update_channel: "release".into(),
            check_updates: true,
            show_hidden: false,
            auto_connect: true,
            api_port: argus_api::server::DEFAULT_PORT,
            developer: false,
            style: Style::System,
        }
    }
}

fn flag(v: &str) -> Option<bool> {
    match v {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

impl Settings {
    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "update_channel" if !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_graphic()) => {
                    s.update_channel = v.to_string()
                }
                "check_updates" => s.check_updates = flag(v).unwrap_or(s.check_updates),
                "show_hidden" => s.show_hidden = flag(v).unwrap_or(s.show_hidden),
                "auto_connect" => s.auto_connect = flag(v).unwrap_or(s.auto_connect),
                "developer" => s.developer = flag(v).unwrap_or(s.developer),
                "api_port" => s.api_port = v.parse().unwrap_or(s.api_port),
                "style" => {
                    s.style = match v {
                        "light" => Style::Light,
                        "dark" => Style::Dark,
                        _ => Style::System,
                    }
                }
                _ => {}
            }
        }
        s
    }

    pub fn render(&self) -> String {
        let style = match self.style {
            Style::System => "system",
            Style::Light => "light",
            Style::Dark => "dark",
        };
        format!(
            "# Argus (Rust app) settings\nupdate_channel = {}\ncheck_updates = {}\nshow_hidden = {}\nauto_connect = {}\napi_port = {}\ndeveloper = {}\nstyle = {style}\n",
            self.update_channel, self.check_updates, self.show_hidden, self.auto_connect, self.api_port, self.developer
        )
    }

    pub fn load(dir: &Path) -> Settings {
        std::fs::read_to_string(dir.join(FILE)).map(|t| Settings::parse(&t)).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE}.tmp"));
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(self.render().as_bytes())?;
        f.sync_all()?;
        std::fs::rename(tmp, dir.join(FILE))
    }
}

/// `$XDG_CONFIG_HOME/argus`, else `~/.config/argus`.
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|c| c.join("argus"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_bad_values_keep_the_defaults() {
        let s = Settings {
            update_channel: "development".into(),
            show_hidden: true,
            api_port: 45000,
            style: Style::Light,
            ..Settings::default()
        };
        assert_eq!(Settings::parse(&s.render()), s);
        let odd = Settings::parse("api_port = 70000\ncheck_updates = maybe\nupdate_channel = a b\nstyle = blue\n");
        assert_eq!(odd, Settings::default());
    }
}
