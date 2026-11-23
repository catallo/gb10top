//! Block devices, network interfaces and PCIe link state.

use super::{Collector, Rates, Snapshot};
use crate::sysfs::{file_name, list, read_num, read_str, Root};
use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct Disk {
    pub name: String,
    pub model: Option<String>,
    pub read_bps: Option<f64>,
    pub write_bps: Option<f64>,
    pub read_iops: Option<f64>,
    pub write_iops: Option<f64>,
    /// Percent of the interval the device had I/O in flight.
    pub busy_pct: Option<f64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Net {
    pub name: String,
    pub state: String,
    /// Backed by a real device (as opposed to bridges, veth, tunnels).
    pub physical: bool,
    /// Kernel driver of the backing device, e.g. "r8127" or "mlx5_core".
    pub driver: Option<String>,
    pub speed_mbps: Option<i64>,
    /// Bytes per second.
    pub rx_bps: Option<f64>,
    pub tx_bps: Option<f64>,
    pub rx_errors: u64,
    pub tx_errors: u64,
    pub rx_dropped: u64,
    pub tx_dropped: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct PcieLink {
    pub addr: String,
    /// PCI class code, e.g. 0x060400 for a bridge.
    pub class: Option<String>,
    pub speed: String,
    pub width: Option<u32>,
    pub max_speed: Option<String>,
    pub max_width: Option<u32>,
}

pub struct IoCollector {
    root: Root,
    rates: Rates,
}

impl IoCollector {
    pub fn new(root: &Root) -> Self {
        IoCollector { root: root.clone(), rates: Rates::default() }
    }

    fn disks(&mut self, dt: f64) -> Vec<Disk> {
        let mut out = Vec::new();
        for dir in list(self.root.path("/sys/block")) {
            // Only real hardware: loop and ram devices have no backing device.
            if !dir.join("device").exists() {
                continue;
            }
            let name = file_name(&dir);
            let Some(stat) = read_str(dir.join("stat")) else { continue };
            let f: Vec<u64> =
                stat.split_ascii_whitespace().filter_map(|x| x.parse().ok()).collect();
            if f.len() < 10 {
                continue;
            }
            let mut r = |what: &str, v: u64| self.rates.rate(&format!("disk/{name}/{what}"), v, dt);
            out.push(Disk {
                model: read_str(dir.join("device/model")),
                read_iops: r("rio", f[0]),
                read_bps: r("rsec", f[2]).map(|s| s * 512.0),
                write_iops: r("wio", f[4]),
                write_bps: r("wsec", f[6]).map(|s| s * 512.0),
                busy_pct: r("ticks", f[9]).map(|ms| (ms / 10.0).min(100.0)),
                name,
            });
        }
        out
    }

    fn nets(&mut self, dt: f64) -> Vec<Net> {
        let mut out = Vec::new();
        for dir in list(self.root.path("/sys/class/net")) {
            let name = file_name(&dir);
            if name == "lo" {
                continue;
            }
            let stat = |f: &str| read_num::<u64>(dir.join("statistics").join(f)).unwrap_or(0);
            let (rx, tx) = (stat("rx_bytes"), stat("tx_bytes"));
            out.push(Net {
                state: read_str(dir.join("operstate")).unwrap_or_default(),
                physical: dir.join("device").exists(),
                driver: std::fs::read_link(dir.join("device/driver"))
                    .ok()
                    .map(|p| file_name(&p)),
                speed_mbps: read_num::<i64>(dir.join("speed")).filter(|s| *s > 0),
                rx_bps: self.rates.rate(&format!("net/{name}/rx"), rx, dt),
                tx_bps: self.rates.rate(&format!("net/{name}/tx"), tx, dt),
                rx_errors: stat("rx_errors"),
                tx_errors: stat("tx_errors"),
                rx_dropped: stat("rx_dropped"),
                tx_dropped: stat("tx_dropped"),
                name,
            });
        }
        out
    }

    fn pcie(&self) -> Vec<PcieLink> {
        list(self.root.path("/sys/bus/pci/devices"))
            .into_iter()
            .filter_map(|dir| {
                Some(PcieLink {
                    speed: read_str(dir.join("current_link_speed"))?,
                    addr: file_name(&dir),
                    class: read_str(dir.join("class")),
                    width: read_num(dir.join("current_link_width")),
                    max_speed: read_str(dir.join("max_link_speed")),
                    max_width: read_num(dir.join("max_link_width")),
                })
            })
            .collect()
    }
}

impl Collector for IoCollector {
    fn name(&self) -> &'static str {
        "io"
    }

    fn sample(&mut self, dt: f64, snap: &mut Snapshot) {
        snap.disks = self.disks(dt);
        snap.nets = self.nets(dt);
        snap.pcie = self.pcie();
    }
}
