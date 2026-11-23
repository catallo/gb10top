//! The `spbm` hwmon device (ACPI NVDA8800): system power rails, power limits,
//! energy counters and SoC temperatures. It comes from the out-of-tree
//! `spark_hwmon` DKMS driver, so it may be absent. Power is in microwatts and
//! energy in microjoules.
//!
//! What the values mean, measured on an ASUS GX10 under CPU and GPU load:
//! - `sys_total` and `dc_input` agree within about 1 W: whole-system draw.
//! - `soc_pkg` is the SoC package and matches the `pkg` energy counter.
//!   `sys_total - soc_pkg` (7 W idle, about 20 W loaded) is the rest of the
//!   board plus conversion loss.
//! - `cpu_gpu` is `cpu_p + cpu_e + gpu` plus about 1 W. The `gpu` rail reads
//!   15-20% above NVML's GPU power and reacts faster.
//! - Limit inputs are the averages the firmware compares against each cap:
//!   `pl2` tracks instantaneous package power, `pl1` is a slow average of it,
//!   `syspl2` / `syspl1` are the same pair for whole-system power.
//! - `pl_level` is 1 normally and 2 while package power exceeds the `pl2`
//!   cap. NVML reports no throttle reason when that happens.
//! - `prochot` stayed at 1 up to 93 C and 160 W, so 1 is the resting value.
//! - `tj_max_c` is the hottest junction temperature in whole degrees and
//!   equals the hottest ACPI thermal zone.
//! - `temp*_input` holds centi-kelvin where hwmon expects millidegrees (see
//!   `decode_temp`). The `dla` sensor is stuck at -0.25 C, i.e. not populated.

use super::{Collector, Snapshot};
use crate::sysfs::{list, read_num, read_str, Root};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Serialize, Clone, Debug)]
pub struct Rail {
    pub label: String,
    pub watts: f64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Limit {
    pub label: String,
    pub watts: f64,
    pub cap_w: Option<f64>,
    pub min_w: Option<f64>,
    pub max_w: Option<f64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Energy {
    pub label: String,
    pub joules: f64,
    /// Average power over the last interval, derived from the counter.
    pub watts: Option<f64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Temp {
    pub label: String,
    pub celsius: f64,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Spbm {
    pub rails: Vec<Rail>,
    pub limits: Vec<Limit>,
    pub energy: Vec<Energy>,
    pub temps: Vec<Temp>,
    pub pl_level: Option<i64>,
    pub prochot: Option<i64>,
    pub tj_max_c: Option<f64>,
}

impl Spbm {
    pub fn rail(&self, label: &str) -> Option<f64> {
        self.rails.iter().find(|r| r.label == label).map(|r| r.watts)
    }
}

/// Convert a raw `temp*_input` value to Celsius. The driver (as of the build
/// measured) exposes centi-kelvin; a fixed driver would expose millidegrees.
fn decode_temp(raw: f64, centi_kelvin: bool) -> f64 {
    if centi_kelvin {
        raw / 100.0 - 273.15
    } else {
        raw / 1000.0
    }
}

/// Decide which encoding the temperatures use by checking the `tj_max`
/// sensor against the independently reported `tj_max_c`.
fn is_centi_kelvin(tj_max_raw: f64, tj_max_c: f64) -> bool {
    (decode_temp(tj_max_raw, true) - tj_max_c).abs()
        < (decode_temp(tj_max_raw, false) - tj_max_c).abs()
}

pub struct SpbmCollector {
    dir: PathBuf,
    /// (index, label, is_limit)
    power: Vec<(u32, String, bool)>,
    energy: Vec<(u32, String)>,
    temp: Vec<(u32, String)>,
    energy_rate: HashMap<String, EnergyRate>,
}

/// The energy counters tick only a few times a second, so average power is
/// taken over a window of at least this long rather than per sample.
const ENERGY_WINDOW: f64 = 1.0;

#[derive(Default)]
struct EnergyRate {
    /// Counter value at the start of the current window, and the window's age.
    base: Option<(u64, f64)>,
    watts: Option<f64>,
}

impl EnergyRate {
    fn update(&mut self, uj: u64, dt: f64) -> Option<f64> {
        match self.base {
            Some((base, age)) if uj >= base => {
                let age = age + dt;
                if age >= ENERGY_WINDOW {
                    self.watts = Some((uj - base) as f64 / 1e6 / age);
                    self.base = Some((uj, 0.0));
                } else {
                    self.base = Some((base, age));
                }
            }
            // First sample, or the counter was reset.
            _ => self.base = Some((uj, 0.0)),
        }
        self.watts
    }
}

impl SpbmCollector {
    pub fn probe(root: &Root) -> Option<Self> {
        let dir = list(root.path("/sys/class/hwmon"))
            .into_iter()
            .find(|d| read_str(d.join("name")).as_deref() == Some("spbm"))?;
        let labelled = |kind: &str| -> Vec<(u32, String)> {
            (1..=64)
                .filter_map(|i| Some((i, read_str(dir.join(format!("{kind}{i}_label")))?)))
                .collect()
        };
        let power = labelled("power")
            .into_iter()
            .map(|(i, l)| {
                let is_limit = dir.join(format!("power{i}_cap")).exists();
                (i, l, is_limit)
            })
            .collect();
        let energy = labelled("energy");
        let temp = labelled("temp");
        Some(SpbmCollector { dir, power, energy, temp, energy_rate: HashMap::new() })
    }

    fn micro(&self, file: String) -> Option<f64> {
        read_num::<f64>(self.dir.join(file)).map(|v| v / 1e6)
    }
}

impl Collector for SpbmCollector {
    fn name(&self) -> &'static str {
        "spbm"
    }

    fn sample(&mut self, dt: f64, snap: &mut Snapshot) {
        let mut s = Spbm::default();
        for (i, label, is_limit) in &self.power {
            let Some(watts) = self.micro(format!("power{i}_input")) else { continue };
            if *is_limit {
                s.limits.push(Limit {
                    label: label.clone(),
                    watts,
                    cap_w: self.micro(format!("power{i}_cap")),
                    min_w: self.micro(format!("power{i}_min")),
                    max_w: self.micro(format!("power{i}_max")),
                });
            } else {
                s.rails.push(Rail { label: label.clone(), watts });
            }
        }
        for (i, label) in &self.energy {
            let Some(uj) = read_num::<u64>(self.dir.join(format!("energy{i}_input"))) else {
                continue;
            };
            let watts = self.energy_rate.entry(label.clone()).or_default().update(uj, dt);
            s.energy.push(Energy { label: label.clone(), joules: uj as f64 / 1e6, watts });
        }
        s.pl_level = read_num(self.dir.join("pl_level"));
        s.prochot = read_num(self.dir.join("prochot"));
        s.tj_max_c = read_num(self.dir.join("tj_max_c"));
        let raw: Vec<(&String, f64)> = self
            .temp
            .iter()
            .filter_map(|(i, l)| Some((l, read_num(self.dir.join(format!("temp{i}_input")))?)))
            .collect();
        let tj_raw = raw.iter().find(|(l, _)| l.as_str() == "tj_max").map(|(_, v)| *v);
        let centi_kelvin = match (tj_raw, s.tj_max_c) {
            (Some(r), Some(c)) => is_centi_kelvin(r, c),
            _ => false,
        };
        for (label, v) in raw {
            let celsius = decode_temp(v, centi_kelvin);
            // An unpopulated sensor (dla) sits just below 0 C and never moves.
            if centi_kelvin && celsius < 1.0 {
                continue;
            }
            s.temps.push(Temp { label: label.clone(), celsius });
        }
        snap.spbm = Some(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn energy_power_uses_a_window() {
        let mut r = EnergyRate::default();
        assert_eq!(r.update(0, 0.0), None);
        // A short sample: nothing yet, the window is not full.
        assert_eq!(r.update(10_000_000, 0.5), None);
        // 30 J over 1 s.
        assert_eq!(r.update(30_000_000, 0.5), Some(30.0));
        // Held until the next window completes: 20 J over 1 s.
        assert_eq!(r.update(35_000_000, 0.5), Some(30.0));
        assert_eq!(r.update(50_000_000, 0.5), Some(20.0));
        // A counter reset starts over without inventing a value.
        assert_eq!(r.update(1_000, 0.5), Some(20.0));
    }

    #[test]
    fn temp_decoding() {
        // Measured: temp1_input=35820 while tj_max_c=85 and the ACPI zone read 85.03.
        assert!(is_centi_kelvin(35820.0, 85.0));
        assert!((decode_temp(35820.0, true) - 85.05).abs() < 0.01);
        // A driver that reports true millidegrees is left alone.
        assert!(!is_centi_kelvin(85030.0, 85.0));
        assert_eq!(decode_temp(85030.0, false), 85.03);
    }
}
