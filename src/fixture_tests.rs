//! Tests against a recorded GX10 (`tests/fixtures/gx10`, made with
//! `tools/record_fixture.py`): the collectors read real file layouts, and
//! every tab is drawn at every supported size.

use crate::collect::gpu::{Gpu, GpuProc};
use crate::collect::smart::Smart;
use crate::collect::{Sampler, Snapshot};
use crate::config::Config;
use crate::sysfs::Root;
use crate::tui::{self, App, Tab};
use std::fs;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gx10")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            copy_tree(&entry.path(), &dest);
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path()).unwrap(), dest).unwrap();
        } else {
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}

/// A private, writable copy of the fixture, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gb10top-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        copy_tree(&fixture(), &dir);
        Scratch(dir)
    }

    fn write(&self, path: &str, text: &str) {
        fs::write(self.0.join(path), text).unwrap();
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.0.join(path)).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn collectors_read_the_recorded_machine() {
    let mut sampler = Sampler::new(&Root::new(fixture()));
    let s = sampler.sample();

    let p = s.spbm.as_ref().expect("spbm present");
    assert!(p.rail("sys_total").unwrap() > 5.0);
    assert_eq!(p.rails.len(), 10);
    let pl1 = p.limits.iter().find(|l| l.label == "pl1").unwrap();
    assert_eq!((pl1.cap_w, pl1.max_w), (Some(140.0), Some(250.0)));
    assert_eq!(p.energy.len(), 4);
    // Temperatures are decoded from centi-kelvin and agree with tj_max_c.
    let tj = p.temps.iter().find(|t| t.label == "tj_max").unwrap().celsius;
    assert!((tj - p.tj_max_c.unwrap()).abs() < 1.5, "tj {tj} vs {:?}", p.tj_max_c);
    assert!(p.temps.iter().all(|t| (20.0..110.0).contains(&t.celsius)));
    assert!(p.temps.iter().all(|t| t.label != "dla"), "unpopulated sensor is dropped");

    let c = s.cpu.as_ref().unwrap();
    let kind = |k: &str| c.cores.iter().filter(|x| x.kind == k).count();
    assert_eq!((c.cores.len(), kind("P"), kind("E")), (20, 10, 10));
    let core = |id: u32| c.cores.iter().find(|x| x.id == id).unwrap();
    assert_eq!((core(5).kind.as_str(), core(5).cluster), ("P", Some(0)));
    assert_eq!((core(0).kind.as_str(), core(0).cluster), ("E", Some(0)));
    assert_eq!((core(15).kind.as_str(), core(15).cluster), ("P", Some(1)));
    assert_eq!(core(15).max_mhz, Some(3900.0));

    let m = s.mem.as_ref().unwrap();
    assert!((120..124).contains(&(m.total >> 30)), "about 121.6 GiB, got {}", m.total >> 30);
    assert!(s.psi.as_ref().unwrap().memory.is_some());

    let t = s.thermal.as_ref().unwrap();
    let labels: Vec<_> = t.zones.iter().map(|z| z.label.clone().unwrap()).collect();
    assert_eq!(labels[0], "tj_max");
    assert_eq!(labels[4], "cpu_p_clu1");
    assert!(t.zones.iter().all(|z| z.crit_c == Some(104.8)));
    assert_eq!(t.cpu_cooling_count, 20);
    assert!(t.sensors.iter().any(|x| x.chip == "nvme"));
    // The GPU root port reports a nonsense cooling state, which is dropped.
    assert_eq!(t.cooling.len(), 5);

    assert_eq!(s.disks.len(), 1);
    assert_eq!(s.disks[0].name, "nvme0n1");
    let eth = s.nets.iter().find(|n| n.driver.as_deref() == Some("r8127")).unwrap();
    assert!(eth.physical);
    assert_eq!(s.pcie.len(), 10);

    let p200 = s.procs.iter().find(|x| x.pid == 200).unwrap();
    assert_eq!(p200.name, "model server");
    assert_eq!(p200.container.as_deref(), Some("abababababab"));
    assert_eq!(s.procs.iter().find(|x| x.pid == 100).unwrap().container, None);
    assert!(s.gpu.is_none() && s.smart.is_empty(), "live-only sources stay empty");
}

#[test]
fn rates_come_from_counter_changes() {
    let scratch = Scratch::new("rates");
    let mut sampler = Sampler::new(&Root::new(&scratch.0));
    let first = sampler.sample();
    assert_eq!(first.nets.iter().find(|n| n.name == "enP7s7").unwrap().rx_bps, Some(0.0));

    // Add 1 MB received and make cpu3 fully busy for 500 ticks.
    let rx = "sys/class/net/enP7s7/statistics/rx_bytes";
    let before: u64 = scratch.read(rx).trim().parse().unwrap();
    scratch.write(rx, &format!("{}\n", before + 1_000_000));
    let stat: String = scratch
        .read("proc/stat")
        .lines()
        .map(|l| match l.strip_prefix("cpu3 ") {
            Some(rest) => {
                let mut f: Vec<u64> = rest.split(' ').map(|x| x.parse().unwrap()).collect();
                f[0] += 500;
                format!("cpu3 {}\n", f.iter().map(u64::to_string).collect::<Vec<_>>().join(" "))
            }
            None => format!("{l}\n"),
        })
        .collect();
    scratch.write("proc/stat", &stat);
    std::thread::sleep(std::time::Duration::from_millis(50));

    let s = sampler.sample();
    let rx_bps = s.nets.iter().find(|n| n.name == "enP7s7").unwrap().rx_bps.unwrap();
    assert!((rx_bps * s.dt - 1_000_000.0).abs() < 1.0, "1 MB over the interval, got {rx_bps} B/s");
    let cores = &s.cpu.as_ref().unwrap().cores;
    assert_eq!(cores.iter().find(|c| c.id == 3).unwrap().util, Some(100.0));
    assert_eq!(cores.iter().find(|c| c.id == 4).unwrap().util, None, "no ticks elapsed");
}

/// A fixture snapshot with the live-only sources filled in by hand.
fn full_snapshot(sampler: &mut Sampler) -> Snapshot {
    let mut s = sampler.sample();
    s.dt = 1.0;
    s.t = 1_700_000_000.0;
    s.gpu = Some(Gpu {
        name: Some("NVIDIA GB10".into()),
        driver: Some("580.0".into()),
        util: Some(96),
        temp_c: Some(70),
        power_w: Some(94.0),
        clock_mhz: Some(2476),
        max_clock_mhz: Some(3003),
        procs: vec![GpuProc { pid: 200, name: "model server".into(), kind: "C".into(), mem: Some(40 << 30) }],
        ..Default::default()
    });
    for p in &mut s.procs {
        if p.pid == 200 {
            p.gpu_mem = Some(40 << 30);
            p.gpu_kind = Some("C".into());
        }
    }
    s.smart = vec![Smart {
        dev: "nvme0".into(),
        critical_warning: 0,
        temp_c: Some(43.0),
        avail_spare_pct: Some(100),
        spare_thresh_pct: Some(5),
        percent_used: Some(1),
        read_bytes: Some(1 << 40),
        written_bytes: Some(1 << 40),
        power_on_hours: Some(2135),
        power_cycles: Some(26),
        unsafe_shutdowns: Some(6),
        media_errors: Some(0),
    }];
    s
}

fn app_with(snaps: usize, tweak: impl Fn(&mut Snapshot)) -> App {
    let root = Root::new(fixture());
    let mut sampler = Sampler::new(&root);
    let first = full_snapshot(&mut sampler);
    let mut app = App::new(sampler, &root, Config::default());
    for i in 0..snaps {
        let mut s = first.clone();
        s.t += i as f64;
        tweak(&mut s);
        app.ingest(s);
    }
    app
}

#[test]
fn every_tab_draws_at_every_size() {
    let mut app = app_with(30, |_| {});
    let sizes = [(60, 14), (80, 24), (100, 30), (110, 31), (124, 40), (150, 46), (220, 70)];
    for tab in Tab::ALL {
        app.tab = tab;
        for (w, h) in sizes {
            let text = tui::to_text(&app, w, h).unwrap();
            assert!(text.contains("gb10top"), "{tab:?} at {w}x{h}");
            assert!(text.contains(&format!("{}", tab.title())), "{tab:?} at {w}x{h}");
        }
    }
    let tiny = tui::to_text(&app, 50, 10).unwrap();
    assert!(tiny.contains("needs at least 60x14"));
}

#[test]
fn tabs_show_their_data() {
    let mut app = app_with(30, |_| {});
    let mut draw = |tab: Tab| {
        app.tab = tab;
        tui::to_text(&app, 150, 46).unwrap()
    };
    let overview = draw(Tab::Overview);
    for needle in ["ASUS GX10", "NOMINAL", "P-cores", "POWER", "MEMORY", "THERMAL", "model server"] {
        assert!(overview.contains(needle), "overview lacks {needle:?}");
    }
    let power = draw(Tab::Power);
    for needle in ["SYSTEM POWER", "sys_total", "dc_input", "pkg slow", "/140 W", "prochot 1", "244 W┤"] {
        assert!(power.contains(needle), "power lacks {needle:?}");
    }
    let gpu = draw(Tab::Gpu);
    for needle in ["UTILISATION", "2476", "driver 580.0", "none reported", "40.0G", "abababababab"] {
        assert!(gpu.contains(needle), "gpu lacks {needle:?}");
    }
    let cpu = draw(Tab::Cpu);
    for needle in ["P-CORES", "E-CORES", "P19  1", "E0   0", "up to 3.9 GHz", "governor"] {
        assert!(cpu.contains(needle), "cpu lacks {needle:?}");
    }
    let thermal = draw(Tab::Thermal);
    for needle in ["JUNCTION TEMPERATURE", "P clu1", "NVMe", "Wi-Fi", "PCIe 0004:00:00.0", "104.8°"] {
        assert!(thermal.contains(needle), "thermal lacks {needle:?}");
    }
    let io = draw(Tab::Io);
    for needle in ["nvme0n1", "wear 1%", "2135 h", "r8127", "ConnectX ports idle", "below maximum"] {
        assert!(io.contains(needle), "io lacks {needle:?}");
    }
    assert!(draw(Tab::Procs).contains("1-2 of 2"));
    assert!(draw(Tab::Events).contains("nothing active"));

    // The narrow overview is the compact summary.
    app.tab = Tab::Overview;
    assert!(tui::to_text(&app, 80, 24).unwrap().contains("SUMMARY"));
}

#[test]
fn alerts_reach_the_header_and_the_event_log() {
    let mut app = app_with(3, |s| {
        let p = s.spbm.as_mut().unwrap();
        p.pl_level = Some(2);
        p.tj_max_c = Some(97.0);
    });
    let overview = tui::to_text(&app, 150, 46).unwrap();
    assert!(overview.contains("POWER CAP") && overview.contains("HOT"));
    assert!(!overview.contains("NOMINAL"));
    assert!(overview.contains("8 Events (2)"));

    app.tab = Tab::Events;
    let events = tui::to_text(&app, 150, 46).unwrap();
    assert!(events.contains("raised") && events.contains("junction temperature 97"));
    assert!(events.contains("held at its 142 W limit"));
    // Raised once, not once per sample.
    assert_eq!(app.alerts.events.len(), 2);
}

#[test]
fn csv_log_gets_a_header_and_one_row_per_sample() {
    let path = std::env::temp_dir().join(format!("gb10top-test-{}-log.csv", std::process::id()));
    let _ = fs::remove_file(&path);
    let root = Root::new(fixture());
    let mut sampler = Sampler::new(&root);
    let snap = full_snapshot(&mut sampler);
    for _ in 0..2 {
        // Reopening appends without repeating the header.
        let cfg = Config { log: Some(path.clone()), ..Config::default() };
        let mut app = App::new(Sampler::new(&root), &root, cfg);
        app.ingest(snap.clone());
        assert!(app.log_error.is_none());
    }
    let text = fs::read_to_string(&path).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with("unix_time,sys_w,"));
    let cols: Vec<_> = lines[1].split(',').collect();
    assert_eq!(cols.len(), crate::csvlog::COLUMNS.len());
    assert_eq!(cols[0], "1700000000.000");
    assert_eq!(cols[12], "96", "gpu_util_pct");
    let _ = fs::remove_file(&path);
}
