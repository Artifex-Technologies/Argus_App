//! The region bundle (`/regions/api/v0/bundle` on Flipper's server) and the region data the C++ app makes from it
//! for `/int/.region_data` (`RegionInfo`, `RegionProvisioningOperation::generateRegionData`, the plugin's
//! `RegionData`), with Qt's JSON rules (`qjson`).
//!
//! The bundle is `{"error": {"code", "text"}}` (checked first) or `{"success": {"bands", "countries", "country",
//! "default"}}`: `bands` a non-empty object of bands (`start`, `end`, `max_power`, `duty_cycle`), `countries` a
//! non-empty object of non-empty lists of band names, `country` the country the server saw (a string or null),
//! `default` a non-empty list of band names for any other country.
//!
//! Checked in both apps since 2026-11-12 (the C++ app took any value, see docs/RUST_PORT.md): every value of a
//! band is a whole number, `start` and `end` within 32 bits and `start <= end`, `max_power` (dBm) within -128..127
//! and `duty_cycle` (percent) within 0..100, as the firmware keeps them in 8 bits (the C++ app read other
//! values as 0 or cut them to 8 bits); every name in a list is a string and names a band (an unknown one was an
//! all-zero band); anything else makes the bundle invalid.

use std::collections::HashMap;

use prost::Message as _;
use qjson::Value;

use flipper_rpc::proto::pb;

/// The largest bundle read (Flipper's is a few KiB).
pub const MAX_BUNDLE: u64 = 1 << 20;

/// Where the firmware reads the region data.
pub const REGION_DATA_PATH: &str = "/int/.region_data";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Band {
    pub start: u32,
    pub end: u32,
    pub power_limit: i32,
    pub duty_cycle: u32,
}

/// A valid bundle that says "success".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionInfo {
    country: Vec<u8>,
    default: Vec<Vec<u8>>,
    countries: HashMap<Vec<u8>, Vec<Vec<u8>>>,
    bands: HashMap<Vec<u8>, Band>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bundle {
    /// The server's own error (`RegionInfo::isError`).
    ServerError {
        code: i32,
        text: String,
    },
    Success(RegionInfo),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn bad(m: &str) -> Error {
    Error(m.to_string())
}

/// `toLocal8Bit()` of a key or name (UTF-8; Qt leaves a lone surrogate out).
fn bytes(s: &[u16]) -> Vec<u8> {
    qjson::to_utf8(s)
}

/// A whole number within `min..=max`.
fn integer(v: &Value, min: i64, max: i64) -> Option<i64> {
    let n = match *v {
        Value::Int(n) => n,
        Value::Double(d) if d.fract() == 0.0 && d >= min as f64 && d <= max as f64 => d as i64,
        _ => return None,
    };
    (min..=max).contains(&n).then_some(n)
}

fn band(v: &Value) -> Result<Band, Error> {
    let o = v.as_object().ok_or_else(|| bad("Band info is not an object"))?;
    let (Some(start), Some(end), Some(duty), Some(power)) = (o.get("start"), o.get("end"), o.get("duty_cycle"), o.get("max_power")) else {
        return Err(bad("Band object is missing one or more fields"));
    };
    let range = || bad("Band value out of range");
    let start = integer(start, 0, u32::MAX as i64).ok_or_else(range)? as u32;
    let end = integer(end, 0, u32::MAX as i64).ok_or_else(range)? as u32;
    let power_limit = integer(power, -128, 127).ok_or_else(range)? as i32;
    let duty_cycle = integer(duty, 0, 100).ok_or_else(range)? as u32;
    if start > end {
        return Err(range());
    }
    Ok(Band { start, end, power_limit, duty_cycle })
}

fn names(v: &Value, what: &str) -> Result<Vec<Vec<u8>>, Error> {
    let Value::Array(a) = v else {
        return Err(Error(format!("{what} is not an array")));
    };
    if a.is_empty() {
        return Err(Error(format!("{what} is an empty array")));
    }
    a.iter()
        .map(|x| match x {
            Value::Str(s) => Ok(bytes(s)),
            _ => Err(Error(format!("{what} holds a name that is not a string"))),
        })
        .collect()
}

fn success(v: &Value) -> Result<RegionInfo, Error> {
    let o = v.as_object().ok_or_else(|| bad("Success info is not an object"))?;
    let (Some(bands_v), Some(countries_v), Some(country_v), Some(default_v)) =
        (o.get("bands"), o.get("countries"), o.get("country"), o.get("default"))
    else {
        return Err(bad("Success object is missing one or more fields"));
    };
    let bands_o = bands_v.as_object().ok_or_else(|| bad("Bands info is not an object"))?;
    if bands_o.entries().is_empty() {
        return Err(bad("Bands info is an empty object"));
    }
    let mut bands = HashMap::new();
    for (k, v) in bands_o.entries() {
        bands.insert(bytes(k), band(v)?);
    }
    let countries_o = countries_v.as_object().ok_or_else(|| bad("Countries info is not an object"))?;
    if countries_o.entries().is_empty() {
        return Err(bad("Countries info is an empty object"));
    }
    let mut countries = HashMap::new();
    for (k, v) in countries_o.entries() {
        countries.insert(bytes(k), names(v, "Country bands list")?);
    }
    let country = match country_v {
        Value::Str(s) => bytes(s),
        Value::Null => Vec::new(),
        _ => return Err(bad("Country is not a string")),
    };
    let default = names(default_v, "Default bands list")?;
    if countries.values().flatten().chain(&default).any(|k| !bands.contains_key(k)) {
        return Err(bad("A bands list names a band that is not in bands"));
    }
    Ok(RegionInfo { country, default, countries, bands })
}

/// `RegionInfo(text)`: the bundle, or why it is not one (the C++ app's "Server returned invalid data").
pub fn parse(text: &[u8]) -> Result<Bundle, Error> {
    let doc = qjson::from_json(text).map_err(|e| Error(format!("Failed to parse the document: {}", e.text())))?;
    let obj = doc.as_object().ok_or_else(|| bad("Json document is not an object"))?;
    if let Some(e) = obj.get("error") {
        let o = e.as_object().ok_or_else(|| bad("Error info is not an object"))?;
        let (Some(code), Some(text)) = (o.get("code"), o.get("text")) else {
            return Err(bad("Error object is missing one or more fields"));
        };
        return Ok(Bundle::ServerError { code: code.to_int(0), text: text.to_text() });
    }
    match obj.get("success") {
        Some(s) => success(s).map(Bundle::Success),
        None => Err(bad("Got valid JSON object, but neither success nor error info")),
    }
}

impl RegionInfo {
    /// The country the server saw, if it named one.
    pub fn detected_country(&self) -> &[u8] {
        &self.country
    }

    /// `RegionInfo::countryOr`: the server's country, or `fallback` when it named none.
    pub fn country_or(&self, fallback: &[u8]) -> Vec<u8> {
        if self.country.is_empty() {
            fallback.to_vec()
        } else {
            self.country.clone()
        }
    }

    /// `countryBandKeys`: the country's band names, or the default ones.
    pub fn band_names(&self, country: &[u8]) -> &[Vec<u8>] {
        self.countries.get(country).unwrap_or(&self.default)
    }

    /// `bandsByCountry`: the bands, in the list's order.
    pub fn bands_for(&self, country: &[u8]) -> Vec<Band> {
        self.band_names(country).iter().map(|k| self.bands[k]).collect()
    }
}

/// `RegionData::encode`: the `PB_Region` message the firmware reads from `/int/.region_data` (no
/// `country_code` when it is empty, as the C++ app sends none).
pub fn encode(country: &[u8], bands: &[Band]) -> Vec<u8> {
    pb::Region {
        country_code: country.to_vec(),
        bands: bands
            .iter()
            .map(|b| pb::region::Band { start: b.start, end: b.end, power_limit: b.power_limit, duty_cycle: b.duty_cycle })
            .collect(),
    }
    .encode_to_vec()
}

/// What `generateRegionData` makes of a bundle: the country used, its bands and the bytes for
/// [`REGION_DATA_PATH`]. `fallback_country` stands in when the server named none (the C++ app takes the
/// system locale's territory there; argus-cli takes `--country`, or none: the default bands).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionData {
    pub country: Vec<u8>,
    pub bands: Vec<Band>,
    pub bytes: Vec<u8>,
}

pub fn region_data(bundle_text: &[u8], fallback_country: &[u8]) -> Result<RegionData, Error> {
    match parse(bundle_text)? {
        Bundle::ServerError { code, text } => Err(Error(format!("the region server says: {text} (code {code})"))),
        Bundle::Success(info) => {
            let country = info.country_or(fallback_country);
            let bands = info.bands_for(&country);
            let bytes = encode(&country, &bands);
            Ok(RegionData { country, bands, bytes })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUNDLE: &str = r#"{"success":{"bands":{"R1":{"start":433050000,"end":434790000,"max_power":10,"duty_cycle":50},"R2":{"start":868150000,"end":868550000,"max_power":12,"duty_cycle":50},"R3":{"start":304100000,"end":321950000,"max_power":-3,"duty_cycle":100}},"countries":{"DE":["R1","R2"],"US":["R3"]},"country":null,"default":["R1"]}}"#;

    #[test]
    fn picks_the_bands() {
        let d = region_data(BUNDLE.as_bytes(), b"DE").unwrap();
        assert_eq!(d.country, b"DE");
        assert_eq!(d.bands.len(), 2);
        let d = region_data(BUNDLE.as_bytes(), b"").unwrap();
        assert_eq!(d.bands, vec![Band { start: 433050000, end: 434790000, power_limit: 10, duty_cycle: 50 }]);
        // no country_code field at all
        assert_eq!(d.bytes[0], 0x12);
        let us = BUNDLE.replace(r#""country":null"#, r#""country":"US""#);
        let d = region_data(us.as_bytes(), b"DE").unwrap();
        assert_eq!((d.country.as_slice(), d.bands[0].power_limit), (&b"US"[..], -3));
    }

    #[test]
    fn refuses_what_the_firmware_cannot_hold() {
        for (from, to) in [
            (r#""max_power":10"#, r#""max_power":128"#),
            (r#""max_power":10"#, r#""max_power":"10""#),
            (r#""duty_cycle":100"#, r#""duty_cycle":101"#),
            (r#""start":433050000"#, r#""start":-1"#),
            (r#""start":433050000"#, r#""start":433050000.5"#),
            (r#""end":868550000"#, r#""end":4294967296"#),
            (r#""start":868150000"#, r#""start":868600000"#),
            (r#""default":["R1"]"#, r#""default":["R9"]"#),
            (r#""default":["R1"]"#, r#""default":[1]"#),
            (r#""default":["R1"]"#, r#""default":[]"#),
            (r#""US":["R3"]"#, r#""US":"R3""#),
            (r#""country":null"#, r#""country":5"#),
        ] {
            let b = BUNDLE.replace(from, to);
            assert_ne!(b, BUNDLE);
            assert!(parse(b.as_bytes()).is_err(), "{to}");
        }
        assert_eq!(parse(br#"{"error":{"code":3,"text":"no"},"success":1}"#), Ok(Bundle::ServerError { code: 3, text: "no".into() }));
    }
}
