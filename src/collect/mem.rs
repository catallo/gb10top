//! System memory and pressure stall information. On the GB10 this memory is
//! unified: GPU allocations come out of the same pool.

use super::{Collector, Snapshot};
use crate::sysfs::{read_str, Root};
use serde::Serialize;
use std::collections::HashMap;

/// All sizes in bytes.
#[derive(Serialize, Clone, Debug, Default)]
pub struct Mem {
    pub total: u64,
    pub available: u64,
    pub free: u64,
    /// Page cache + buffers + reclaimable slab.
    pub cached: u64,
    pub shmem: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

impl Mem {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Pressure {
    /// Percent of the last 10 s in which some / all tasks were stalled.
    pub some10: f64,
    pub full10: Option<f64>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Psi {
    pub cpu: Option<Pressure>,
    pub memory: Option<Pressure>,
    pub io: Option<Pressure>,
}

pub struct MemCollector {
    root: Root,
}

impl MemCollector {
    pub fn new(root: &Root) -> Self {
        MemCollector { root: root.clone() }
    }
}

fn parse_meminfo(text: &str) -> Mem {
    let kv: HashMap<&str, u64> = text
        .lines()
        .filter_map(|l| {
            let (k, rest) = l.split_once(':')?;
            let kb: u64 = rest.split_ascii_whitespace().next()?.parse().ok()?;
            Some((k, kb * 1024))
        })
        .collect();
    let g = |k: &str| kv.get(k).copied().unwrap_or(0);
    Mem {
        total: g("MemTotal"),
        available: g("MemAvailable"),
        free: g("MemFree"),
        cached: g("Cached") + g("Buffers") + g("SReclaimable"),
        shmem: g("Shmem"),
        swap_total: g("SwapTotal"),
        swap_free: g("SwapFree"),
    }
}

fn parse_pressure(text: &str) -> Option<Pressure> {
    let avg10 = |prefix: &str| -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(prefix))?
            .split_ascii_whitespace()
            .find_map(|f| f.strip_prefix("avg10="))?
            .parse()
            .ok()
    };
    Some(Pressure { some10: avg10("some")?, full10: avg10("full") })
}

impl Collector for MemCollector {
    fn name(&self) -> &'static str {
        "mem"
    }

    fn sample(&mut self, _dt: f64, snap: &mut Snapshot) {
        if let Some(text) = read_str(self.root.path("/proc/meminfo")) {
            snap.mem = Some(parse_meminfo(&text));
        }
        let p = |res: &str| {
            read_str(self.root.path(&format!("/proc/pressure/{res}")))
                .and_then(|t| parse_pressure(&t))
        };
        let psi = Psi { cpu: p("cpu"), memory: p("memory"), io: p("io") };
        if psi.cpu.is_some() || psi.memory.is_some() || psi.io.is_some() {
            snap.psi = Some(psi);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo() {
        let m = parse_meminfo(
            "MemTotal:       1000 kB\nMemFree:  200 kB\nMemAvailable:    600 kB\n\
             Buffers: 10 kB\nCached: 300 kB\nSReclaimable: 40 kB\nHugePages_Total:   0\n",
        );
        assert_eq!(m.total, 1000 * 1024);
        assert_eq!(m.cached, 350 * 1024);
        assert_eq!(m.used(), 400 * 1024);
    }

    #[test]
    fn pressure() {
        let p = parse_pressure(
            "some avg10=1.50 avg60=0.62 avg300=0.73 total=1\nfull avg10=0.66 avg60=0 avg300=0 total=2",
        )
        .unwrap();
        assert_eq!(p.some10, 1.5);
        assert_eq!(p.full10, Some(0.66));
        let cpu = parse_pressure("some avg10=0.25 avg60=0 avg300=0 total=1").unwrap();
        assert_eq!(cpu.full10, None);
    }
}
