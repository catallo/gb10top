//! NVMe SMART health through `nvme smart-log`. The command needs root, so
//! this collector only exists when gb10top itself runs as root.

use super::{Collector, Snapshot};
use crate::sysfs::{file_name, list, Root};
use serde::Serialize;
use std::process::Command;

/// Seconds between SMART queries; the values change slowly.
const PERIOD: f64 = 60.0;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Smart {
    pub dev: String,
    /// Non-zero means the drive itself reports a problem.
    pub critical_warning: u64,
    pub temp_c: Option<f64>,
    pub avail_spare_pct: Option<u64>,
    pub spare_thresh_pct: Option<u64>,
    /// Share of rated endurance consumed.
    pub percent_used: Option<u64>,
    pub read_bytes: Option<u64>,
    pub written_bytes: Option<u64>,
    pub power_on_hours: Option<u64>,
    pub power_cycles: Option<u64>,
    pub unsafe_shutdowns: Option<u64>,
    pub media_errors: Option<u64>,
}

pub struct SmartCollector {
    devs: Vec<String>,
    cache: Vec<Smart>,
    age: Option<f64>,
}

impl SmartCollector {
    pub fn probe(root: &Root) -> Option<Self> {
        // SAFETY: geteuid has no preconditions.
        if !root.is_live() || unsafe { libc::geteuid() } != 0 {
            return None;
        }
        let devs: Vec<String> =
            list("/sys/class/nvme").iter().map(|p| file_name(p)).collect();
        (!devs.is_empty()).then(|| SmartCollector { devs, cache: Vec::new(), age: None })
    }
}

/// Parse the JSON printed by `nvme smart-log -o json`.
pub fn parse(dev: &str, json: &str) -> Option<Smart> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let n = |k: &str| v.get(k).and_then(|x| x.as_u64());
    // Data units are 1000 blocks of 512 bytes.
    let units = |k: &str| n(k).map(|u| u.saturating_mul(512_000));
    Some(Smart {
        dev: dev.to_string(),
        critical_warning: n("critical_warning")?,
        // Reported in kelvin.
        temp_c: n("temperature").map(|k| k as f64 - 273.15),
        avail_spare_pct: n("avail_spare"),
        spare_thresh_pct: n("spare_thresh"),
        percent_used: n("percent_used"),
        read_bytes: units("data_units_read"),
        written_bytes: units("data_units_written"),
        power_on_hours: n("power_on_hours"),
        power_cycles: n("power_cycles"),
        unsafe_shutdowns: n("unsafe_shutdowns"),
        media_errors: n("media_errors"),
    })
}

impl Collector for SmartCollector {
    fn name(&self) -> &'static str {
        "smart"
    }

    fn sample(&mut self, dt: f64, snap: &mut Snapshot) {
        let age = self.age.map(|a| a + dt);
        if age.is_none_or(|a| a >= PERIOD) {
            self.cache = self
                .devs
                .iter()
                .filter_map(|dev| {
                    let out = Command::new("nvme")
                        .args(["smart-log", &format!("/dev/{dev}"), "-o", "json"])
                        .output()
                        .ok()?;
                    parse(dev, &String::from_utf8_lossy(&out.stdout))
                })
                .collect();
            self.age = Some(0.0);
        } else {
            self.age = age;
        }
        snap.smart = self.cache.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nvme_cli_output() {
        // Trimmed from a real `nvme smart-log -o json` on the GX10's drive.
        let json = r#"{"critical_warning":0,"temperature":316,"avail_spare":100,
            "spare_thresh":5,"percent_used":0,"data_units_read":39685290,
            "data_units_written":15216223,"power_cycles":26,"power_on_hours":2135,
            "unsafe_shutdowns":6,"media_errors":0}"#;
        let s = parse("nvme0", json).unwrap();
        assert_eq!(s.critical_warning, 0);
        assert!((s.temp_c.unwrap() - 42.85).abs() < 0.01);
        assert_eq!(s.avail_spare_pct, Some(100));
        assert_eq!(s.written_bytes, Some(15216223 * 512_000));
        assert_eq!(s.power_on_hours, Some(2135));
        assert!(parse("nvme0", "not json").is_none());
    }
}
