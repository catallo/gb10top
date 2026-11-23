//! Append-only CSV log of the headline metrics, one row per sample.

use crate::alerts::tj_max;
use crate::collect::Snapshot;
use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;

const GIB: f64 = (1u64 << 30) as f64;

/// Column names, in the order `row` produces them.
pub const COLUMNS: [&str; 24] = [
    "unix_time", "sys_w", "pkg_w", "gpu_w", "cpu_p_w", "cpu_e_w", "pl_level", "tj_c",
    "cpu_util_pct", "p_util_pct", "e_util_pct", "load1", "gpu_util_pct", "gpu_clock_mhz",
    "gpu_temp_c", "gpu_nvml_w", "gpu_mem_gib", "mem_used_gib", "mem_available_gib",
    "swap_used_gib", "disk_read_bps", "disk_write_bps", "net_rx_bps", "net_tx_bps",
];

fn num(v: Option<f64>, prec: usize) -> String {
    v.map(|v| format!("{v:.prec$}")).unwrap_or_default()
}

/// One CSV row; missing values are empty fields.
pub fn row(s: &Snapshot) -> String {
    let rail = |l: &str| s.spbm.as_ref().and_then(|p| p.rail(l));
    let gpu = s.gpu.as_ref();
    let mem = s.mem.as_ref();
    let cpu = s.cpu.as_ref();
    let disk = |f: fn(&crate::collect::io::Disk) -> Option<f64>| {
        (!s.disks.is_empty()).then(|| s.disks.iter().filter_map(f).sum::<f64>())
    };
    let net = |f: fn(&crate::collect::io::Net) -> Option<f64>| {
        let phys: Vec<_> = s.nets.iter().filter(|n| n.physical).collect();
        (!phys.is_empty()).then(|| phys.iter().filter_map(|n| f(n)).sum::<f64>())
    };
    [
        format!("{:.3}", s.t),
        num(rail("sys_total"), 2),
        num(rail("soc_pkg"), 2),
        num(rail("gpu"), 2),
        num(rail("cpu_p"), 2),
        num(rail("cpu_e"), 2),
        num(s.spbm.as_ref().and_then(|p| p.pl_level).map(|v| v as f64), 0),
        num(tj_max(s), 1),
        num(cpu.and_then(|c| c.util), 1),
        num(cpu.and_then(|c| c.p_util), 1),
        num(cpu.and_then(|c| c.e_util), 1),
        num(cpu.map(|c| c.load[0]), 2),
        num(gpu.and_then(|g| g.util).map(f64::from), 0),
        num(gpu.and_then(|g| g.clock_mhz).map(f64::from), 0),
        num(gpu.and_then(|g| g.temp_c).map(f64::from), 0),
        num(gpu.and_then(|g| g.power_w), 2),
        num(gpu.map(|g| g.procs_mem() as f64 / GIB), 2),
        num(mem.map(|m| m.used() as f64 / GIB), 2),
        num(mem.map(|m| m.available as f64 / GIB), 2),
        num(mem.map(|m| (m.swap_total - m.swap_free) as f64 / GIB), 2),
        num(disk(|d| d.read_bps), 0),
        num(disk(|d| d.write_bps), 0),
        num(net(|n| n.rx_bps), 0),
        num(net(|n| n.tx_bps), 0),
    ]
    .join(",")
}

pub struct CsvLog {
    out: BufWriter<File>,
}

impl CsvLog {
    /// Open for appending; the header is written only into an empty file.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening log {}", path.display()))?;
        let empty = file.metadata()?.len() == 0;
        let mut out = BufWriter::new(file);
        if empty {
            writeln!(out, "{}", COLUMNS.join(","))?;
        }
        Ok(CsvLog { out })
    }

    pub fn write(&mut self, s: &Snapshot) -> Result<()> {
        writeln!(self.out, "{}", row(s))?;
        // Flushed every row so the file is useful while the app runs.
        self.out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_matches_header_and_leaves_gaps_empty() {
        let s = Snapshot { t: 12.5, ..Default::default() };
        let r = row(&s);
        assert_eq!(r.split(',').count(), COLUMNS.len());
        assert!(r.starts_with("12.500,,"));
    }
}
