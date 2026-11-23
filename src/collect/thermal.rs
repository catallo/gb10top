//! ACPI thermal zones, other hwmon temperature chips (NVMe, Wi-Fi) and
//! cooling-device throttle states.

use super::{Collector, Snapshot};
use crate::sysfs::{file_name, list, read_num, read_str, Root};
use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct Zone {
    pub name: String,
    /// What the zone measures, when known for this platform.
    pub label: Option<String>,
    pub kind: String,
    pub celsius: f64,
    pub crit_c: Option<f64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Sensor {
    pub chip: String,
    pub label: String,
    pub celsius: f64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Cooling {
    pub kind: String,
    pub cur: i64,
    pub max: i64,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Thermal {
    pub zones: Vec<Zone>,
    pub sensors: Vec<Sensor>,
    /// Number of CPU cooling devices and the highest throttle state among them.
    pub cpu_cooling_count: usize,
    pub cpu_cooling_max_state: i64,
    /// Non-CPU cooling devices (PCIe link-speed throttles and the like).
    pub cooling: Vec<Cooling>,
}

/// The GB10's seven anonymous `acpitz` zones, in order. Established by
/// loading P cores, E cores and the GPU separately and matching each zone
/// against the labelled spbm sensors (they agree within 0.3 C).
const GB10_ZONES: [&str; 7] =
    ["tj_max", "cpu_e_clu0", "cpu_p_clu0", "cpu_e_clu1", "cpu_p_clu1", "gpu", "soc"];

/// Seconds between reads of the non-ACPI hwmon chips. Reading the NVMe
/// temperature sends a command to the drive and takes about 40 ms per
/// sensor, far too slow to do on every sample.
const SLOW_SENSOR_PERIOD: f64 = 5.0;

pub struct ThermalCollector {
    root: Root,
    sensors: Vec<Sensor>,
    /// Seconds since `sensors` was refreshed; None before the first read.
    sensors_age: Option<f64>,
}

impl ThermalCollector {
    pub fn new(root: &Root) -> Self {
        ThermalCollector { root: root.clone(), sensors: Vec::new(), sensors_age: None }
    }
}

impl ThermalCollector {
    fn read_sensors(&self) -> Vec<Sensor> {
        let mut out = Vec::new();
        for dir in list(self.root.path("/sys/class/hwmon")) {
            let chip = read_str(dir.join("name")).unwrap_or_default();
            // spbm has its own collector; acpitz duplicates the thermal zones.
            if chip == "spbm" || chip.starts_with("acpitz") {
                continue;
            }
            for i in 1..=16 {
                let Some(mc) = read_num::<f64>(dir.join(format!("temp{i}_input"))) else {
                    continue;
                };
                let label = read_str(dir.join(format!("temp{i}_label")))
                    .unwrap_or_else(|| format!("temp{i}"));
                out.push(Sensor { chip: chip.clone(), label, celsius: mc / 1000.0 });
            }
        }
        out
    }
}

impl Collector for ThermalCollector {
    fn name(&self) -> &'static str {
        "thermal"
    }

    fn sample(&mut self, dt: f64, snap: &mut Snapshot) {
        let mut t = Thermal::default();
        for dir in list(self.root.path("/sys/class/thermal")) {
            let name = file_name(&dir);
            if name.starts_with("thermal_zone") {
                let Some(mc) = read_num::<f64>(dir.join("temp")) else { continue };
                let crit_c = (0..8).find_map(|i| {
                    let kind = read_str(dir.join(format!("trip_point_{i}_type")))?;
                    (kind == "critical")
                        .then(|| read_num::<f64>(dir.join(format!("trip_point_{i}_temp"))))
                        .flatten()
                        .map(|v| v / 1000.0)
                });
                t.zones.push(Zone {
                    name,
                    label: None,
                    kind: read_str(dir.join("type")).unwrap_or_default(),
                    celsius: mc / 1000.0,
                    crit_c,
                });
            } else if name.starts_with("cooling_device") {
                let kind = read_str(dir.join("type")).unwrap_or_default();
                let (Some(cur), Some(max)) = (
                    read_num::<i64>(dir.join("cur_state")),
                    read_num::<i64>(dir.join("max_state")),
                ) else {
                    continue;
                };
                if kind == "Processor" {
                    t.cpu_cooling_count += 1;
                    t.cpu_cooling_max_state = t.cpu_cooling_max_state.max(cur);
                } else if (0..=max).contains(&cur) {
                    // Out-of-range states are firmware garbage (seen on the
                    // GPU root port, which reports a negative value).
                    t.cooling.push(Cooling { kind, cur, max });
                }
            }
        }
        if t.zones.len() == GB10_ZONES.len() && t.zones.iter().all(|z| z.kind == "acpitz") {
            for (z, label) in t.zones.iter_mut().zip(GB10_ZONES) {
                z.label = Some(label.to_string());
            }
        }
        let age = self.sensors_age.map(|a| a + dt);
        if age.is_none_or(|a| a >= SLOW_SENSOR_PERIOD) {
            self.sensors = self.read_sensors();
            self.sensors_age = Some(0.0);
        } else {
            self.sensors_age = age;
        }
        t.sensors = self.sensors.clone();
        snap.thermal = Some(t);
    }
}
