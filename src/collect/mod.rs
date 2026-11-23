//! Collectors: one per data source, each optional. A collector that fails to
//! probe is simply absent, and its section of the snapshot stays empty.

pub mod cpu;
pub mod gpu;
pub mod io;
pub mod mem;
pub mod procs;
pub mod smart;
pub mod spbm;
pub mod thermal;

use crate::sysfs::Root;
use serde::Serialize;
use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Serialize, Default, Clone, Debug)]
pub struct Snapshot {
    /// Unix time of the sample, seconds.
    pub t: f64,
    /// Seconds since the previous sample (0 for the first).
    pub dt: f64,
    pub spbm: Option<spbm::Spbm>,
    pub cpu: Option<cpu::Cpu>,
    pub mem: Option<mem::Mem>,
    pub psi: Option<mem::Psi>,
    pub thermal: Option<thermal::Thermal>,
    pub gpu: Option<gpu::Gpu>,
    pub disks: Vec<io::Disk>,
    pub nets: Vec<io::Net>,
    pub pcie: Vec<io::PcieLink>,
    pub procs: Vec<procs::Proc>,
    /// NVMe health; only filled when running as root.
    pub smart: Vec<smart::Smart>,
}

pub trait Collector {
    fn name(&self) -> &'static str;
    /// Fill this collector's part of the snapshot. `dt` is seconds since the
    /// previous call (0 on the first), used for rate metrics.
    fn sample(&mut self, dt: f64, snap: &mut Snapshot);
}

/// Probe every source and return the collectors that are present.
pub fn probe_all(root: &Root) -> Vec<Box<dyn Collector>> {
    let mut v: Vec<Box<dyn Collector>> = Vec::new();
    if let Some(c) = spbm::SpbmCollector::probe(root) {
        v.push(Box::new(c));
    }
    v.push(Box::new(cpu::CpuCollector::new(root)));
    v.push(Box::new(mem::MemCollector::new(root)));
    v.push(Box::new(thermal::ThermalCollector::new(root)));
    if root.is_live() {
        if let Some(c) = gpu::GpuCollector::probe() {
            v.push(Box::new(c));
        }
    }
    v.push(Box::new(io::IoCollector::new(root)));
    if let Some(c) = smart::SmartCollector::probe(root) {
        v.push(Box::new(c));
    }
    // Last, so that it can merge the GPU collector's per-process memory.
    v.push(Box::new(procs::ProcCollector::new(root)));
    v
}

/// Owns the collectors and the sampling clock.
pub struct Sampler {
    collectors: Vec<Box<dyn Collector>>,
    last: Instant,
}

impl Sampler {
    /// Probe all sources and take a priming sample so that the first real
    /// sample already has rates.
    pub fn new(root: &Root) -> Self {
        let mut collectors = probe_all(root);
        let mut scratch = Snapshot::default();
        for c in &mut collectors {
            c.sample(0.0, &mut scratch);
        }
        Sampler { collectors, last: Instant::now() }
    }

    pub fn sample(&mut self) -> Snapshot {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let mut snap = Snapshot { t, dt, ..Default::default() };
        for c in &mut self.collectors {
            c.sample(dt, &mut snap);
        }
        snap
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.collectors.iter().map(|c| c.name()).collect()
    }
}

/// Turns monotonically increasing counters into per-second rates.
#[derive(Default)]
pub struct Rates {
    prev: HashMap<String, u64>,
}

impl Rates {
    /// Rate for `key` given its current counter value; None on first sight,
    /// when dt is zero, or when the counter went backwards.
    pub fn rate(&mut self, key: &str, cur: u64, dt: f64) -> Option<f64> {
        let prev = self.prev.insert(key.to_string(), cur)?;
        (dt > 0.0 && cur >= prev).then(|| (cur - prev) as f64 / dt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates() {
        let mut r = Rates::default();
        assert_eq!(r.rate("a", 100, 1.0), None);
        assert_eq!(r.rate("a", 300, 2.0), Some(100.0));
        assert_eq!(r.rate("a", 200, 1.0), None); // counter reset
        assert_eq!(r.rate("a", 250, 0.0), None);
    }
}
