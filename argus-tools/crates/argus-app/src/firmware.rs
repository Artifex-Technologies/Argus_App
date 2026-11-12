//! Which firmware action the main button offers, as the C++ app decides it (`ApplicationBackend::firmwareUpdateState`,
//! `FlipperZero::canUpdate/canInstall/canRepair`, `VCPDeviceInfoHelper::branchToChannelName`), and the texts around it.

use flipper_updates::directory::{self, Version};

use crate::device::DeviceInfo;

pub const RELEASE: &str = "release";
pub const RELEASE_CANDIDATE: &str = "release-candidate";
pub const DEVELOPMENT: &str = "development";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateState {
    /// No device, or the directory was never read.
    Unknown,
    Checking,
    /// The directory could not be read (the button says "Try again").
    Error(String),
    CanRepair,
    CanUpdate,
    CanInstall,
    NoUpdates,
}

impl UpdateState {
    /// The main button's text.
    pub fn button(&self) -> &'static str {
        match self {
            UpdateState::Unknown => "No data",
            UpdateState::Checking => "Checking...",
            UpdateState::Error(_) => "Try again",
            UpdateState::CanRepair => "Repair",
            UpdateState::CanUpdate => "Update",
            UpdateState::CanInstall => "Install",
            UpdateState::NoUpdates => "No updates",
        }
    }

    pub fn enabled(&self) -> bool {
        matches!(self, UpdateState::CanUpdate | UpdateState::CanInstall | UpdateState::CanRepair | UpdateState::Error(_))
    }

    pub fn tooltip(&self) -> &'static str {
        match self {
            UpdateState::CanRepair => "Repair a broken firmware installation. May erase your progress and settings.",
            UpdateState::CanUpdate => "Update the device to the latest version",
            UpdateState::CanInstall => "Install firmware from the selected update channel",
            UpdateState::Error(_) => "Check the connection and try again",
            _ => "",
        }
    }
}

/// The channel a firmware branch belongs to: `X.Y.Z` is release, `X.Y.Z-rc` release candidate, `dev` development,
/// anything else (a fork's branch such as `ARG-1.5.1r2`) custom.
pub fn branch_to_channel(branch: &str) -> &'static str {
    let (num, rc) = match branch.strip_suffix("-rc") {
        Some(n) => (n, true),
        None => (branch, false),
    };
    let parts: Vec<&str> = num.split('.').collect();
    if parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
        if rc {
            RELEASE_CANDIDATE
        } else {
            RELEASE
        }
    } else if branch == "dev" {
        DEVELOPMENT
    } else {
        "custom"
    }
}

/// `dd-MM-yyyy` as (year, month, day).
fn device_date(s: &str) -> Option<(i64, i64, i64)> {
    let mut it = s.split('-').map(|p| p.parse::<i64>().ok());
    let (d, m, y) = (it.next()??, it.next()??, it.next()??);
    (it.next().is_none() && (1..=12).contains(&m) && (1..=31).contains(&d)).then_some((y, m, d))
}

/// Seconds since 1970 as a UTC date (the C++ app takes the local date; the difference is a few hours at most).
pub fn civil_date(secs: i64) -> (i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    // Howard Hinnant's civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `FlipperZero::canUpdate`.
pub fn can_update(info: &DeviceInfo, server_channel: &str, latest: &Version) -> bool {
    let no_assets = info.sd_present && !info.assets_installed;
    if no_assets || info.radio_version.is_empty() {
        return true;
    }
    let device_channel = branch_to_channel(&info.firmware_branch);
    let device_version = if device_channel == DEVELOPMENT { &info.firmware_commit } else { &info.firmware_version };
    let cmp = directory::compare(device_version, &latest.number);
    // an unreadable date never counts as older (the C++ QDate comparison with an invalid date is false)
    let older_by_date = match device_date(&info.build_date) {
        Some(d) => d <= civil_date(latest.timestamp),
        None => false,
    };
    use std::cmp::Ordering::*;
    match (device_channel, server_channel) {
        (RELEASE, RELEASE) | (RELEASE, RELEASE_CANDIDATE) | (RELEASE_CANDIDATE, RELEASE_CANDIDATE) => cmp == Less,
        (RELEASE_CANDIDATE, RELEASE) => cmp != Greater,
        (RELEASE, DEVELOPMENT) | (RELEASE_CANDIDATE, DEVELOPMENT) | (DEVELOPMENT, RELEASE) | (DEVELOPMENT, RELEASE_CANDIDATE) => {
            older_by_date
        }
        (DEVELOPMENT, DEVELOPMENT) => *device_version != latest.number && older_by_date,
        _ => false,
    }
}

/// `ApplicationBackend::firmwareUpdateState` once the directory is read.
pub fn state(info: &DeviceInfo, recovery: bool, server_channel: &str, latest: Option<&Version>) -> UpdateState {
    let Some(latest) = latest else { return UpdateState::Unknown };
    if recovery {
        UpdateState::CanRepair
    } else if can_update(info, server_channel, latest) {
        UpdateState::CanUpdate
    } else if branch_to_channel(&info.firmware_branch) != server_channel {
        UpdateState::CanInstall
    } else {
        UpdateState::NoUpdates
    }
}

/// The channel link's text: "Release 1.2.3", the version cut at its first `-` (the Qt app also strips an `AUG-` prefix).
pub fn channel_label(channel: &str, version: &str) -> String {
    let name = match channel {
        DEVELOPMENT => "Dev",
        RELEASE_CANDIDATE => "RC",
        RELEASE => "Release",
        _ => "Unknown",
    };
    let v = version.strip_prefix("AUG-").unwrap_or(version);
    format!("{name} {}", v.split('-').next().unwrap_or(v))
}

/// The confirmation's extra paragraph for a device running a fork: the channels serve official firmware, which replaces
/// it (the Qt app's `customFirmwareWarning`).
pub fn fork_warning(info: &DeviceInfo) -> Option<String> {
    (!info.fork.is_empty() && info.fork != "Official")
        .then(|| format!("This replaces the {} firmware ({}) on this device.", info.fork, info.firmware_version))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(number: &str, timestamp: i64) -> Version {
        Version { number: number.into(), changelog: String::new(), timestamp, files: Vec::new() }
    }

    fn info(branch: &str, version: &str, date: &str) -> DeviceInfo {
        DeviceInfo {
            firmware_branch: branch.into(),
            firmware_version: version.into(),
            firmware_commit: "abc123".into(),
            build_date: date.into(),
            radio_version: "1.17.3".into(),
            sd_present: true,
            assets_installed: true,
            ..DeviceInfo::default()
        }
    }

    #[test]
    fn branches_map_to_channels_as_in_the_cpp_app() {
        assert_eq!(branch_to_channel("0.98.3"), RELEASE);
        assert_eq!(branch_to_channel("0.98.3-rc"), RELEASE_CANDIDATE);
        assert_eq!(branch_to_channel("dev"), DEVELOPMENT);
        assert_eq!(branch_to_channel("ARG-1.5.1r2"), "custom");
        assert_eq!(branch_to_channel("1.2"), "custom");
    }

    #[test]
    fn the_button_follows_the_cpp_rules() {
        let new = version("1.2.0", 1_800_000_000);
        // release on release: by version
        assert_eq!(state(&info("1.1.0", "1.1.0", "01-01-2026"), false, RELEASE, Some(&new)), UpdateState::CanUpdate);
        assert_eq!(state(&info("1.2.0", "1.2.0", "01-01-2026"), false, RELEASE, Some(&new)), UpdateState::NoUpdates);
        // a fork on the release channel: install
        assert_eq!(state(&info("ARG-1.5.1r2", "ARG-1.5.1r2", "09-11-2026"), false, RELEASE, Some(&new)), UpdateState::CanInstall);
        // no radio stack or missing databases: always update
        let mut broken = info("1.2.0", "1.2.0", "01-01-2026");
        broken.radio_version.clear();
        assert_eq!(state(&broken, false, RELEASE, Some(&new)), UpdateState::CanUpdate);
        // dev to dev: another commit built no later than the server's
        let dev = version("def456", 1_800_000_000);
        assert_eq!(state(&info("dev", "x", "01-01-2026"), false, DEVELOPMENT, Some(&dev)), UpdateState::CanUpdate);
        assert_eq!(state(&info("dev", "x", "01-01-2099"), false, DEVELOPMENT, Some(&dev)), UpdateState::NoUpdates);
        assert_eq!(state(&info("dev", "x", "01-01-2026"), true, DEVELOPMENT, Some(&dev)), UpdateState::CanRepair);
        assert_eq!(state(&info("dev", "x", "01-01-2026"), false, DEVELOPMENT, None), UpdateState::Unknown);
    }

    #[test]
    fn dates_and_labels() {
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(civil_date(1_700_000_000), (2023, 11, 14));
        assert_eq!(civil_date(951_782_400), (2000, 2, 29));
        assert_eq!(channel_label(RELEASE, "1.2.0"), "Release 1.2.0");
        assert_eq!(channel_label(DEVELOPMENT, "AUG-abc-12"), "Dev abc");
        assert!(fork_warning(&DeviceInfo { fork: "Argus".into(), firmware_version: "ARG-1".into(), ..DeviceInfo::default() }).is_some());
        assert!(fork_warning(&DeviceInfo { fork: "Official".into(), ..DeviceInfo::default() }).is_none());
    }
}
