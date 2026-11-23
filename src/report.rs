//! Plain-text dump of one snapshot, used by `--once` to validate every source.

use crate::collect::Snapshot;
use std::fmt::Write;

fn opt<T: std::fmt::Display>(v: Option<T>, unit: &str) -> String {
    v.map(|v| format!("{v}{unit}")).unwrap_or_else(|| "n/a".into())
}

fn optf(v: Option<f64>, prec: usize, unit: &str) -> String {
    v.map(|v| format!("{v:.prec$}{unit}")).unwrap_or_else(|| "n/a".into())
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

/// Bytes per second as a human-readable rate.
fn rate(v: Option<f64>) -> String {
    match v {
        None => "n/a".into(),
        Some(b) if b >= 1e6 => format!("{:.1} MB/s", b / 1e6),
        Some(b) => format!("{:.1} kB/s", b / 1e3),
    }
}

pub fn text(s: &Snapshot) -> String {
    let mut o = String::new();
    let w = &mut o;

    if let Some(p) = &s.spbm {
        let _ = writeln!(w, "== POWER (spbm) ==");
        for r in &p.rails {
            let _ = writeln!(w, "  {:<10} {:>7.2} W", r.label, r.watts);
        }
        for l in &p.limits {
            let _ = writeln!(
                w,
                "  {:<10} {:>7.2} W   cap {}  range {}..{}",
                l.label,
                l.watts,
                optf(l.cap_w, 0, " W"),
                optf(l.min_w, 1, ""),
                optf(l.max_w, 0, " W"),
            );
        }
        for e in &p.energy {
            let _ = writeln!(
                w,
                "  energy {:<6} {:>10.1} kJ   avg {}",
                e.label,
                e.joules / 1e3,
                optf(e.watts, 2, " W"),
            );
        }
        for t in &p.temps {
            let _ = writeln!(w, "  temp {:<11} {:>6.2} C", t.label, t.celsius);
        }
        let _ = writeln!(
            w,
            "  pl_level {}  prochot {}  tj_max_c {}",
            opt(p.pl_level, ""),
            opt(p.prochot, ""),
            opt(p.tj_max_c, ""),
        );
    } else {
        let _ = writeln!(w, "== POWER (spbm) == not present");
    }

    if let Some(c) = &s.cpu {
        let _ = writeln!(
            w,
            "== CPU ==  total {}  P {}  E {}  load {:.2} {:.2} {:.2}  governor {}",
            optf(c.util, 1, "%"),
            optf(c.p_util, 1, "%"),
            optf(c.e_util, 1, "%"),
            c.load[0],
            c.load[1],
            c.load[2],
            c.governor.as_deref().unwrap_or("n/a"),
        );
        for core in &c.cores {
            let _ = writeln!(
                w,
                "  cpu{:<2} {} {:>6}  {:>5} / {} MHz",
                core.id,
                core.kind,
                optf(core.util, 1, "%"),
                optf(core.freq_mhz, 0, ""),
                optf(core.max_mhz, 0, ""),
            );
        }
    }

    if let Some(m) = &s.mem {
        let _ = writeln!(
            w,
            "== MEMORY ==  used {} / {}  available {}  cached {}  shmem {}  swap {} / {}",
            gib(m.used()),
            gib(m.total),
            gib(m.available),
            gib(m.cached),
            gib(m.shmem),
            gib(m.swap_total - m.swap_free),
            gib(m.swap_total),
        );
    }
    if let Some(p) = &s.psi {
        let line = |name: &str, p: &Option<crate::collect::mem::Pressure>| match p {
            Some(p) => format!("{name} some {:.2}% full {}", p.some10, optf(p.full10, 2, "%")),
            None => format!("{name} n/a"),
        };
        let _ = writeln!(
            w,
            "== PRESSURE (10 s) ==  {}  |  {}  |  {}",
            line("cpu", &p.cpu),
            line("mem", &p.memory),
            line("io", &p.io),
        );
    }

    if let Some(g) = &s.gpu {
        let _ = writeln!(
            w,
            "== GPU ==  {}  util {}  mem-util {}  temp {}  power {}  clock {} / {} MHz  {}",
            g.name.as_deref().unwrap_or("?"),
            opt(g.util, "%"),
            opt(g.mem_util, "%"),
            opt(g.temp_c, " C"),
            optf(g.power_w, 2, " W"),
            opt(g.clock_mhz, ""),
            opt(g.max_clock_mhz, ""),
            g.pstate.as_deref().unwrap_or("n/a"),
        );
        let _ = writeln!(
            w,
            "  enc {}  dec {}  fb-mem {} / {}  throttle [{}]  process mem total {}",
            opt(g.enc_util, "%"),
            opt(g.dec_util, "%"),
            g.mem_used.map(gib).unwrap_or_else(|| "n/a".into()),
            g.mem_total.map(gib).unwrap_or_else(|| "n/a".into()),
            g.throttle.join(", "),
            gib(g.procs_mem()),
        );
        for p in &g.procs {
            let _ = writeln!(
                w,
                "  {:>8} {} {:<20} {}",
                p.pid,
                p.kind,
                p.name,
                p.mem.map(gib).unwrap_or_else(|| "n/a".into()),
            );
        }
    } else {
        let _ = writeln!(w, "== GPU == NVML not available");
    }

    if let Some(t) = &s.thermal {
        let _ = writeln!(w, "== THERMAL ==");
        for z in &t.zones {
            let _ = writeln!(
                w,
                "  {:<15} {:<11} {:>6.1} C  crit {}",
                z.name,
                z.label.as_deref().unwrap_or(&z.kind),
                z.celsius,
                optf(z.crit_c, 1, " C"),
            );
        }
        for x in &t.sensors {
            let _ = writeln!(w, "  {:<15} {:<10} {:>6.1} C", x.chip, x.label, x.celsius);
        }
        let _ = writeln!(
            w,
            "  cpu cooling: {} devices, max throttle state {}",
            t.cpu_cooling_count, t.cpu_cooling_max_state,
        );
        for c in &t.cooling {
            let _ = writeln!(w, "  cooling {:<36} {}/{}", c.kind, c.cur, c.max);
        }
    }

    let _ = writeln!(w, "== DISKS ==");
    for d in &s.disks {
        let _ = writeln!(
            w,
            "  {:<8} {:<22} rd {} ({} iops)  wr {} ({} iops)  busy {}",
            d.name,
            d.model.as_deref().unwrap_or(""),
            rate(d.read_bps),
            optf(d.read_iops, 0, ""),
            rate(d.write_bps),
            optf(d.write_iops, 0, ""),
            optf(d.busy_pct, 1, "%"),
        );
    }
    let _ = writeln!(w, "== NETWORK ==");
    for n in &s.nets {
        let _ = writeln!(
            w,
            "  {:<16} {:<8} {:<4} {:>10}  rx {}  tx {}  err {}/{}  drop {}/{}",
            n.name,
            n.state,
            if n.physical { "phys" } else { "virt" },
            opt(n.speed_mbps, " Mb/s"),
            rate(n.rx_bps),
            rate(n.tx_bps),
            n.rx_errors,
            n.tx_errors,
            n.rx_dropped,
            n.tx_dropped,
        );
    }
    let _ = writeln!(w, "== PCIe ==");
    for l in &s.pcie {
        let _ = writeln!(
            w,
            "  {} class {}  {} x{}  (max {} x{})",
            l.addr,
            l.class.as_deref().unwrap_or("?"),
            l.speed,
            opt(l.width, ""),
            l.max_speed.as_deref().unwrap_or("?"),
            opt(l.max_width, ""),
        );
    }
    o
}
