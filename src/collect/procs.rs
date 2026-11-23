//! Per-process CPU and memory, merged with NVML's per-process GPU memory and
//! the name of the Docker container each process runs in.

use super::{Collector, Snapshot};
use crate::sysfs::{file_name, list, read_str, Root};
use serde::Serialize;
use std::collections::HashMap;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How many processes to keep per ranking (CPU, memory); GPU users are always kept.
const KEEP: usize = 40;

#[derive(Serialize, Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    pub name: String,
    /// Percent of one core over the last interval; None on first sight.
    pub cpu: Option<f64>,
    /// Resident memory in bytes.
    pub rss: u64,
    pub gpu_mem: Option<u64>,
    /// "C" compute or "G" graphics when the process uses the GPU.
    pub gpu_kind: Option<String>,
    pub container: Option<String>,
}

type Names = Arc<Mutex<HashMap<String, String>>>;

pub struct ProcCollector {
    root: Root,
    /// pid -> CPU ticks at the previous sample.
    prev: HashMap<u32, u64>,
    /// pid -> container id (None = not in a container).
    containers: HashMap<u32, Option<String>>,
    /// container id -> name, refreshed in the background.
    names: Names,
    ticks_per_sec: f64,
    page_size: u64,
}

impl ProcCollector {
    pub fn new(root: &Root) -> Self {
        let names: Names = Arc::default();
        if root.is_live() {
            let shared = names.clone();
            std::thread::spawn(move || loop {
                if let Some(map) = docker_names() {
                    *shared.lock().unwrap() = map;
                }
                std::thread::sleep(Duration::from_secs(15));
            });
        }
        // SAFETY: sysconf has no preconditions.
        let (ticks, page) = unsafe { (libc::sysconf(libc::_SC_CLK_TCK), libc::sysconf(libc::_SC_PAGESIZE)) };
        ProcCollector {
            root: root.clone(),
            prev: HashMap::new(),
            containers: HashMap::new(),
            names,
            ticks_per_sec: if ticks > 0 { ticks as f64 } else { 100.0 },
            page_size: if page > 0 { page as u64 } else { 4096 },
        }
    }

    fn container(&mut self, pid: u32) -> Option<String> {
        let root = &self.root;
        let id = self
            .containers
            .entry(pid)
            .or_insert_with(|| {
                read_str(root.path(&format!("/proc/{pid}/cgroup"))).and_then(|t| container_id(&t))
            })
            .clone()?;
        let names = self.names.lock().unwrap();
        Some(names.get(&id).cloned().unwrap_or_else(|| id[..12].to_string()))
    }
}

/// `docker ps` as a map of full container id to name; None if docker is unusable.
fn docker_names() -> Option<HashMap<String, String>> {
    let out = Command::new("docker")
        .args(["ps", "--no-trunc", "--format", "{{.ID}} {{.Names}}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_once(' '))
            .map(|(id, name)| (id.to_string(), name.to_string()))
            .collect(),
    )
}

/// Extract a 64-hex-digit container id from the contents of /proc/PID/cgroup.
fn container_id(cgroup: &str) -> Option<String> {
    cgroup
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|tok| tok.len() == 64)
        .map(str::to_string)
}

/// Parse /proc/PID/stat into (comm, cpu ticks, resident pages).
fn parse_stat(stat: &str) -> Option<(String, u64, u64)> {
    // comm may contain spaces and parentheses; it ends at the last ')'.
    let (open, close) = (stat.find('(')?, stat.rfind(')')?);
    let comm = stat.get(open + 1..close)?.to_string();
    let f: Vec<&str> = stat.get(close + 1..)?.split_ascii_whitespace().collect();
    // After comm, field 0 is state; utime, stime and rss are stat fields 14, 15, 24.
    let num = |i: usize| f.get(i)?.parse::<u64>().ok();
    Some((comm, num(11)? + num(12)?, num(21)?))
}

impl Collector for ProcCollector {
    fn name(&self) -> &'static str {
        "procs"
    }

    fn sample(&mut self, dt: f64, snap: &mut Snapshot) {
        let mut all: Vec<Proc> = Vec::new();
        let mut seen: HashMap<u32, u64> = HashMap::new();
        for dir in list(self.root.path("/proc")) {
            let Ok(pid) = file_name(&dir).parse::<u32>() else { continue };
            let Some((name, ticks, pages)) =
                read_str(dir.join("stat")).as_deref().and_then(parse_stat)
            else {
                continue;
            };
            seen.insert(pid, ticks);
            let cpu = self.prev.get(&pid).and_then(|prev| {
                (dt > 0.0 && ticks >= *prev)
                    .then(|| 100.0 * (ticks - prev) as f64 / self.ticks_per_sec / dt)
            });
            all.push(Proc {
                pid,
                name,
                cpu,
                rss: pages * self.page_size,
                gpu_mem: None,
                gpu_kind: None,
                container: None,
            });
        }
        self.prev = seen;
        self.containers.retain(|pid, _| self.prev.contains_key(pid));

        let gpu: HashMap<u32, (Option<u64>, String)> = snap
            .gpu
            .iter()
            .flat_map(|g| &g.procs)
            .map(|p| (p.pid, (p.mem, p.kind.clone())))
            .collect();
        for p in &mut all {
            if let Some((mem, kind)) = gpu.get(&p.pid) {
                p.gpu_mem = *mem;
                p.gpu_kind = Some(kind.clone());
            }
        }

        // Keep the top of each ranking plus every GPU user.
        let mut keep: Vec<bool> = all.iter().map(|p| p.gpu_kind.is_some()).collect();
        let mut order: Vec<usize> = (0..all.len()).collect();
        order.sort_by(|a, b| all[*b].cpu.unwrap_or(0.0).total_cmp(&all[*a].cpu.unwrap_or(0.0)));
        order.iter().take(KEEP).for_each(|i| keep[*i] = true);
        order.sort_by_key(|i| std::cmp::Reverse(all[*i].rss));
        order.iter().take(KEEP).for_each(|i| keep[*i] = true);

        let mut out: Vec<Proc> =
            all.into_iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| p).collect();
        for p in &mut out {
            p.container = self.container(p.pid);
        }
        snap.procs = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_with_awkward_comm() {
        let line = "42 (my (odd) name) S 1 42 42 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 3 0 1000 123456 777 18446744073709551615 0 0";
        let (comm, ticks, pages) = parse_stat(line).unwrap();
        assert_eq!(comm, "my (odd) name");
        assert_eq!(ticks, 300);
        assert_eq!(pages, 777);
    }

    #[test]
    fn container_ids() {
        let id = "a".repeat(64);
        assert_eq!(
            container_id(&format!("0::/system.slice/docker-{id}.scope\n")),
            Some(id.clone())
        );
        assert_eq!(container_id(&format!("0::/docker/{id}\n")), Some(id));
        assert_eq!(container_id("0::/user.slice/user-1000.slice/session-3.scope\n"), None);
    }
}
