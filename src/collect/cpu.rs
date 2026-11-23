//! Per-core utilisation and frequency, grouped into performance (P) and
//! efficiency (E) clusters by maximum frequency.

use super::{Collector, Snapshot};
use crate::sysfs::{read_num, read_str, Root};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize, Clone, Debug)]
pub struct Core {
    pub id: u32,
    /// "P" for the fastest cluster, "E" otherwise.
    pub kind: String,
    /// Index of the physical cluster (cores sharing an L3 cache), in order
    /// of first core. On the GB10: 0 for cpus 0-9, 1 for cpus 10-19.
    pub cluster: Option<u32>,
    /// Percent busy over the last interval; None on the first sample.
    pub util: Option<f64>,
    pub freq_mhz: Option<f64>,
    pub max_mhz: Option<f64>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Cpu {
    pub util: Option<f64>,
    pub p_util: Option<f64>,
    pub e_util: Option<f64>,
    pub cores: Vec<Core>,
    pub load: [f64; 3],
    pub governor: Option<String>,
}

pub struct CpuCollector {
    root: Root,
    /// cpu id -> (busy, total) jiffies at the previous sample.
    prev: HashMap<u32, (u64, u64)>,
    /// cpu id -> max frequency in MHz (static).
    max_mhz: HashMap<u32, f64>,
    /// cpu id -> list of cpus sharing its L3 cache (static).
    l3: HashMap<u32, Option<String>>,
}

impl CpuCollector {
    pub fn new(root: &Root) -> Self {
        CpuCollector { root: root.clone(), prev: HashMap::new(), max_mhz: HashMap::new(), l3: HashMap::new() }
    }

    fn cpufreq(&self, id: u32, file: &str) -> std::path::PathBuf {
        self.root.path(&format!("/sys/devices/system/cpu/cpu{id}/cpufreq/{file}"))
    }
}

/// Parse a "cpuN ..." line of /proc/stat into (id, busy, total) jiffies.
fn parse_stat_line(line: &str) -> Option<(u32, u64, u64)> {
    let mut it = line.split_ascii_whitespace();
    let id: u32 = it.next()?.strip_prefix("cpu")?.parse().ok()?;
    let f: Vec<u64> = it.take(8).filter_map(|x| x.parse().ok()).collect();
    if f.len() < 5 {
        return None;
    }
    let total: u64 = f.iter().sum();
    let idle = f[3] + f[4]; // idle + iowait
    Some((id, total - idle, total))
}

fn mean(vals: &[f64]) -> Option<f64> {
    (!vals.is_empty()).then(|| vals.iter().sum::<f64>() / vals.len() as f64)
}

impl Collector for CpuCollector {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn sample(&mut self, _dt: f64, snap: &mut Snapshot) {
        let Some(stat) = read_str(self.root.path("/proc/stat")) else { return };
        let mut cores = Vec::new();
        for (id, busy, total) in stat.lines().filter_map(parse_stat_line) {
            let util = self.prev.insert(id, (busy, total)).and_then(|(pb, pt)| {
                (total > pt).then(|| 100.0 * (busy.saturating_sub(pb)) as f64 / (total - pt) as f64)
            });
            if !self.max_mhz.contains_key(&id) {
                if let Some(khz) = read_num::<f64>(self.cpufreq(id, "cpuinfo_max_freq")) {
                    self.max_mhz.insert(id, khz / 1000.0);
                }
            }
            cores.push(Core {
                id,
                kind: String::new(),
                cluster: None,
                util,
                freq_mhz: read_num::<f64>(self.cpufreq(id, "scaling_cur_freq")).map(|k| k / 1000.0),
                max_mhz: self.max_mhz.get(&id).copied(),
            });
        }
        let mut groups: Vec<String> = Vec::new();
        for c in &mut cores {
            let root = &self.root;
            let shared = self.l3.entry(c.id).or_insert_with(|| {
                read_str(root.path(&format!(
                    "/sys/devices/system/cpu/cpu{}/cache/index3/shared_cpu_list",
                    c.id
                )))
            });
            if let Some(list) = shared {
                let idx = groups.iter().position(|g| g == list).unwrap_or_else(|| {
                    groups.push(list.clone());
                    groups.len() - 1
                });
                c.cluster = Some(idx as u32);
            }
        }
        let top = cores.iter().filter_map(|c| c.max_mhz).fold(0.0, f64::max);
        for c in &mut cores {
            c.kind = match c.max_mhz {
                Some(m) if m < top => "E",
                _ => "P",
            }
            .to_string();
        }
        let utils = |kind: Option<&str>| -> Vec<f64> {
            cores
                .iter()
                .filter(|c| kind.is_none_or(|k| c.kind == k))
                .filter_map(|c| c.util)
                .collect()
        };
        let mut load = [0.0; 3];
        if let Some(l) = read_str(self.root.path("/proc/loadavg")) {
            for (slot, v) in load.iter_mut().zip(l.split_ascii_whitespace()) {
                *slot = v.parse().unwrap_or(0.0);
            }
        }
        snap.cpu = Some(Cpu {
            util: mean(&utils(None)),
            p_util: mean(&utils(Some("P"))),
            e_util: mean(&utils(Some("E"))),
            governor: read_str(self.cpufreq(0, "scaling_governor")),
            load,
            cores,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_line() {
        // user nice system idle iowait irq softirq steal
        let (id, busy, total) = parse_stat_line("cpu7 100 0 50 800 50 0 0 0 0 0").unwrap();
        assert_eq!((id, busy, total), (7, 150, 1000));
        assert!(parse_stat_line("cpu 1 2 3 4 5 6 7 8").is_none()); // aggregate line
        assert!(parse_stat_line("intr 12345").is_none());
    }
}
