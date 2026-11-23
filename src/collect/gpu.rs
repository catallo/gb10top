//! GPU telemetry through NVML. On the GB10 several calls return
//! "not supported" (framebuffer memory, fan); every field is optional.

use super::{Collector, Snapshot};
use crate::sysfs::read_str;
use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
use nvml_wrapper::enums::device::UsedGpuMemory;
use nvml_wrapper::struct_wrappers::device::ProcessInfo;
use nvml_wrapper::Nvml;
use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct GpuProc {
    pub pid: u32,
    pub name: String,
    /// "C" compute, "G" graphics.
    pub kind: String,
    /// Bytes; None when the driver does not report it.
    pub mem: Option<u64>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Gpu {
    pub name: Option<String>,
    pub driver: Option<String>,
    pub util: Option<u32>,
    pub mem_util: Option<u32>,
    pub temp_c: Option<u32>,
    pub power_w: Option<f64>,
    pub clock_mhz: Option<u32>,
    pub max_clock_mhz: Option<u32>,
    pub pstate: Option<String>,
    pub throttle: Vec<String>,
    pub enc_util: Option<u32>,
    pub dec_util: Option<u32>,
    /// Framebuffer memory (total, used) in bytes; unsupported on unified memory.
    pub mem_total: Option<u64>,
    pub mem_used: Option<u64>,
    pub procs: Vec<GpuProc>,
}

impl Gpu {
    /// Sum of per-process GPU memory, the only GPU memory figure the GB10 gives.
    pub fn procs_mem(&self) -> u64 {
        self.procs.iter().filter_map(|p| p.mem).sum()
    }
}

pub struct GpuCollector {
    nvml: Nvml,
    driver: Option<String>,
}

impl GpuCollector {
    pub fn probe() -> Option<Self> {
        let nvml = Nvml::init().ok()?;
        nvml.device_by_index(0).ok()?;
        let driver = nvml.sys_driver_version().ok();
        Some(GpuCollector { nvml, driver })
    }
}

fn procs(list: Vec<ProcessInfo>, kind: &str) -> impl Iterator<Item = GpuProc> + '_ {
    list.into_iter().map(move |p| GpuProc {
        pid: p.pid,
        name: read_str(format!("/proc/{}/comm", p.pid)).unwrap_or_else(|| "?".into()),
        kind: kind.to_string(),
        mem: match p.used_gpu_memory {
            UsedGpuMemory::Used(b) => Some(b),
            UsedGpuMemory::Unavailable => None,
        },
    })
}

impl Collector for GpuCollector {
    fn name(&self) -> &'static str {
        "gpu"
    }

    fn sample(&mut self, _dt: f64, snap: &mut Snapshot) {
        let Ok(d) = self.nvml.device_by_index(0) else { return };
        let util = d.utilization_rates().ok();
        let mem = d.memory_info().ok();
        let mut g = Gpu {
            name: d.name().ok(),
            driver: self.driver.clone(),
            util: util.as_ref().map(|u| u.gpu),
            mem_util: util.as_ref().map(|u| u.memory),
            temp_c: d.temperature(TemperatureSensor::Gpu).ok(),
            power_w: d.power_usage().ok().map(|mw| mw as f64 / 1000.0),
            clock_mhz: d.clock_info(Clock::Graphics).ok(),
            max_clock_mhz: d.max_clock_info(Clock::Graphics).ok(),
            pstate: d.performance_state().ok().map(|p| format!("{p:?}")),
            throttle: d
                .current_throttle_reasons()
                .map(|r| {
                    r.iter_names()
                        .map(|(n, _)| n.to_string())
                        // Idling is a clock reduction, not a problem worth flagging.
                        .filter(|n| n != "GPU_IDLE" && n != "NONE")
                        .collect()
                })
                .unwrap_or_default(),
            enc_util: d.encoder_utilization().ok().map(|u| u.utilization),
            dec_util: d.decoder_utilization().ok().map(|u| u.utilization),
            mem_total: mem.as_ref().map(|m| m.total),
            mem_used: mem.as_ref().map(|m| m.used),
            procs: Vec::new(),
        };
        if let Ok(list) = d.running_compute_processes() {
            g.procs.extend(procs(list, "C"));
        }
        if let Ok(list) = d.running_graphics_processes() {
            g.procs.extend(procs(list, "G"));
        }
        g.procs.sort_by_key(|p| std::cmp::Reverse(p.mem));
        snap.gpu = Some(g);
    }
}
