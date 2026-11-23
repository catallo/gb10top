//! Detail tabs: one subsystem per screen, with long history, statistics over
//! the kept history and the raw values.

use super::panels::*;
use super::theme::*;
use super::widgets::{meter, panel, Pane};
use super::{clock, gpu_watts, primary_net, sensors, tj_max, App};
use crate::alerts::Level;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Span;

/// Below this width, panels that sit side by side are stacked instead.
const STACK_BELOW: u16 = 100;

fn unavailable(buf: &mut Buffer, area: Rect, title: &str, msg: &str) {
    panel(buf, area, title, ACCENT, vec![]).line(0, vec![dim(msg.to_string())]);
}

/// How much time `samples` samples cover at the current refresh rate.
fn window(app: &App, samples: usize) -> String {
    format!("last {}", duration(samples as f64 * app.interval))
}

/// Graph of a series filling the pane from `row` for `h` rows, with the top
/// and bottom of the scale labelled on the left. Returns the samples drawn.
fn axis_graph(
    p: &mut Pane,
    row: u16,
    h: u16,
    app: &App,
    key: &str,
    (lo, hi): (f64, f64),
    grad: &Grad,
    label: impl Fn(f64) -> String,
) -> usize {
    let h = h.min(p.area.height.saturating_sub(row));
    if h == 0 {
        return 0;
    }
    let (top, bottom) = (label(hi), label(lo));
    let gutter = top.chars().count().max(bottom.chars().count()) as u16 + 1;
    let gw = (p.width() as u16).saturating_sub(gutter);
    for r in 0..h {
        let text = if r == 0 {
            &top
        } else if r == h - 1 {
            &bottom
        } else {
            ""
        };
        p.line(row + r, vec![dim(format!("{text:>w$}", w = gutter as usize - 1)), span("┤", FAINT)]);
    }
    let data: Vec<f64> = app.series.tail(key, gw as usize * 2).iter().map(|v| v - lo).collect();
    p.graph(gutter, row, gw, h, &data, hi - lo, grad);
    data.len()
}

struct Row<'a> {
    name: String,
    color: Rgb,
    key: String,
    unit: &'a str,
    prec: usize,
}

/// Table of series: current value, then min / mean / max over the kept
/// history, then a one-row trend scaled to the series' own range.
fn series_table(p: &mut Pane, top: u16, heading: &str, rows: &[Row], app: &App) {
    let w = p.width();
    let trend_x = 46u16;
    let trend_w = (w as u16).saturating_sub(trend_x);
    let mut head = vec![dim(format!("{heading:<12}{:>8}{:>8}{:>8}{:>8}  ", "NOW", "MIN", "AVG", "MAX"))];
    if trend_w >= 6 {
        head.push(dim("TREND (own scale)"));
    }
    p.line(top, head);
    for (i, r) in rows.iter().enumerate() {
        let y = top + 1 + i as u16;
        let cell = |v: Option<f64>| match v {
            Some(v) => format!("{:>8}", format!("{v:.prec$}{}", r.unit, prec = r.prec)),
            None => format!("{:>8}", "·"),
        };
        let now = app.series.tail(&r.key, 1).pop();
        let stats = app.series.stats(&r.key);
        let name: String = r.name.chars().take(11).collect();
        p.line(y, vec![
            span(format!("{name:<12}"), r.color),
            bold(cell(now), FG),
            dim(cell(stats.map(|s| s.0))),
            dim(cell(stats.map(|s| s.1))),
            dim(cell(stats.map(|s| s.2))),
        ]);
        if trend_w >= 6 {
            let data = app.series.tail(&r.key, trend_w as usize * 2);
            let lo = data.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = data.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            // A flat series is drawn as a mid-height line.
            let (shifted, max): (Vec<f64>, f64) = if hi - lo > 1e-9 {
                (data.iter().map(|v| v - lo).collect(), hi - lo)
            } else {
                (data.iter().map(|_| 0.5).collect(), 1.0)
            };
            p.graph(trend_x, y, trend_w, 1, &shifted, max, &Grad::mono(r.color));
        }
    }
}

// ---------- power ----------

fn rail_color(label: &str) -> Rgb {
    match label {
        "gpu" => GPU,
        "cpu_p" => PCORE,
        "cpu_e" => ECORE,
        "soc_pkg" | "cpu_gpu" => SOC,
        "sys_total" | "dc_input" => POWER,
        _ => BOARD,
    }
}

pub fn power(buf: &mut Buffer, area: Rect, app: &App) {
    let Some(sp) = &app.snap.spbm else {
        unavailable(buf, area, "POWER", "power sensor (spbm / spark_hwmon driver) not present");
        return;
    };
    let rail = |l: &str| sp.rail(l).unwrap_or(0.0);
    let (sys, pkg) = (rail("sys_total"), rail("soc_pkg"));
    let scale = sp.limits.iter().find(|l| l.label == "syspl2").and_then(|l| l.cap_w).unwrap_or(240.0);
    // The rail and limit tables need about 17 rows; the graph takes the rest.
    let top_h = area.height.saturating_sub(17).max(6);
    let [top, bottom] = Layout::vertical([Constraint::Length(top_h), Constraint::Min(0)]).areas(area);

    let avg = app.series.stats("sys_w").map(|s| s.1).unwrap_or(sys);
    let mut p = panel(buf, top, "SYSTEM POWER", POWER, vec![]);
    let h = p.area.height;
    let n = axis_graph(&mut p, 0, h, app, "sys_w", (0.0, scale), &Grad::heat(), |v| format!("{v:.0} W"));
    // Redraw the frame now that the time window covered by the graph is known.
    let right = vec![
        bold(format!("{sys:.1} W"), POWER),
        dim(format!(" · avg {avg:.0} · peak {:.0} · {}", app.peak_w, window(app, n))),
    ];
    panel(buf, top, "SYSTEM POWER", POWER, right);

    let [l, r] = split2(bottom, 58, STACK_BELOW);
    let mut p = panel(buf, l, "RAILS", POWER, vec![dim("watts, as named by the driver")]);
    let rows: Vec<Row> = sp
        .rails
        .iter()
        .map(|x| Row {
            name: x.label.clone(),
            color: rail_color(&x.label),
            key: format!("rail/{}", x.label),
            unit: "",
            prec: 1,
        })
        .collect();
    series_table(&mut p, 0, "RAIL", &rows, app);

    let capped = sp.pl_level.is_some_and(|l| l >= 2);
    let right = vec![if capped { badge("PACKAGE CAPPED", BAD) } else { span("● within limits", OK) }];
    let mut p = panel(buf, r, "LIMITS · ENERGY", POWER, right);
    let w = p.width();
    let mut row = 0;
    let names = [("pl1", "pkg slow"), ("pl2", "pkg fast"), ("syspl1", "sys slow"), ("syspl2", "sys fast")];
    for l in &sp.limits {
        let name = names.iter().find(|n| n.0 == l.label).map(|n| n.1).unwrap_or(&l.label);
        let mut spans = limit_spans(name, l, w.saturating_sub(16).max(30));
        if let (Some(lo), Some(hi)) = (l.min_w, l.max_w) {
            spans.push(dim(format!("{lo:.1}..{hi:.0}")));
        }
        p.line(row, spans);
        row += 1;
    }
    let flag = |name: &str, v: Option<String>| vec![dim(format!("{name} ")), span(v.unwrap_or_else(|| "n/a".into()), FG), dim("   ")];
    row += 1;
    p.line(row, [
        flag("pl_level", sp.pl_level.map(|v| v.to_string())),
        flag("prochot", sp.prochot.map(|v| v.to_string())),
        flag("tj_max", sp.tj_max_c.map(|v| format!("{v:.0}°"))),
    ].concat());
    row += 2;
    p.line(row, vec![dim(format!("{:<10}{:>12}{:>10}", "COUNTER", "TOTAL", "NOW"))]);
    for e in &sp.energy {
        row += 1;
        p.line(row, vec![
            span(format!("{:<10}", e.label), rail_color(&e.label.replace("pkg", "soc_pkg"))),
            span(format!("{:>9.1} Wh", e.joules / 3600.0), FG),
            dim(match e.watts {
                Some(w) => format!("{w:>8.1} W"),
                None => format!("{:>10}", "·"),
            }),
        ]);
    }
    row += 2;
    p.line(row, vec![
        dim("session   "),
        span(format!("{:.2} Wh", app.wh), FG),
        dim(format!(" in {}", duration(app.elapsed_s))),
    ]);
    let board = (sys - pkg).max(0.0);
    p.line(row + 1, vec![
        dim("board     "),
        span(format!("{board:.1} W"), FG),
        dim(format!(" outside the package ({:.0}%)", if sys > 0.0 { 100.0 * board / sys } else { 0.0 })),
    ]);
}

// ---------- GPU ----------

pub fn gpu(buf: &mut Buffer, area: Rect, app: &App) {
    let s = &app.snap;
    let Some(g) = &s.gpu else {
        unavailable(buf, area, "GPU", "NVML not available");
        return;
    };
    // State needs 11 rows and the process list about 9; the graphs take the rest.
    let top_h = area.height.saturating_sub(20).max(6);
    let [top, state, procs] =
        Layout::vertical([Constraint::Length(top_h), Constraint::Length(11), Constraint::Min(0)]).areas(area);

    let [l, r] = split2(top, 50, STACK_BELOW);
    let mut p = panel(buf, l, "UTILISATION", GPU, vec![pct_span(g.util.unwrap_or(0) as f64)]);
    let h = p.area.height;
    axis_graph(&mut p, 0, h, app, "gpu_util", (0.0, 100.0), &Grad::mono(GPU), |v| format!("{v:.0}%"));
    let watts = gpu_watts(s).unwrap_or(0.0);
    let max = app.series.stats("gpu_w").map(|x| x.2).unwrap_or(0.0).max(120.0);
    let mut p = panel(buf, r, "POWER", POWER, vec![bold(format!("{watts:.1} W"), POWER)]);
    let h = p.area.height;
    axis_graph(&mut p, 0, h, app, "gpu_w", (0.0, max), &Grad::mono(POWER), |v| format!("{v:.0} W"));

    let right = vec![dim(g.name.clone().unwrap_or_default())];
    let mut p = panel(buf, state, "STATE", GPU, right);
    let row = |name: &str, color: Rgb, key: &str, unit: &'static str, prec: usize| Row {
        name: name.into(),
        color,
        key: key.into(),
        unit,
        prec,
    };
    let mut rows = vec![row("util", GPU, "gpu_util", "%", 0)];
    if s.spbm.is_some() {
        rows.push(row("power rail", POWER, "rail/gpu", "W", 1));
    }
    rows.push(row("power nvml", POWER, "gpu_nvml_w", "W", 1));
    rows.push(row("clock", ACCENT, "gpu_clock", "", 0));
    rows.push(row("temp", (255, 140, 80), "gpu_temp", "°", 0));
    let n = rows.len() as u16;
    series_table(&mut p, 0, "METRIC", &rows, app);
    let w = p.width();
    p.line(n + 2, fit(vec![
        vec![dim("clock max "), span(g.max_clock_mhz.map(|m| format!("{m} MHz")).unwrap_or_else(|| "n/a".into()), FG)],
        vec![dim("   enc "), span(format!("{}%", g.enc_util.unwrap_or(0)), FG)],
        vec![dim("  dec "), span(format!("{}%", g.dec_util.unwrap_or(0)), FG)],
        vec![dim("   mem-bus "), span(format!("{}%", g.mem_util.unwrap_or(0)), FG)],
        vec![dim("   driver "), span(g.driver.clone().unwrap_or_else(|| "n/a".into()), FG)],
    ], w));
    let mut l = vec![dim("throttle  ")];
    if g.throttle.is_empty() {
        l.push(span("● none reported", OK));
        if s.spbm.as_ref().and_then(|p| p.pl_level).is_some_and(|l| l >= 2) {
            l.push(dim("  but the package power cap is active (see Power)"));
        }
    } else {
        l.push(badge(&g.throttle.join(" "), BAD));
    }
    p.line(n + 3, l);

    // Processes using the GPU, largest first.
    let mut users: Vec<_> = s.procs.iter().filter(|x| x.gpu_kind.is_some()).collect();
    users.sort_by_key(|x| std::cmp::Reverse(x.gpu_mem.unwrap_or(0)));
    let right = vec![bold(format!("{} GiB", gib(g.procs_mem())), GPU), dim(" of the unified pool")];
    let mut p = panel(buf, procs, "GPU PROCESSES", GPU, right);
    let total = s.mem.as_ref().map(|m| m.total).unwrap_or(1).max(1) as f64;
    let bar = p.width().saturating_sub(78).min(40);
    p.line(0, vec![dim(format!(
        "{:>7} {:<18}{:<5}{:<22}{:>8}{:>8}{:>8}",
        "PID", "NAME", "TYPE", "CONTAINER", "GPU MEM", "RSS", "CPU%"
    ))]);
    let clip = |t: &str, n: usize| -> String { t.chars().take(n - 1).collect() };
    for (i, x) in users.iter().enumerate() {
        let kind = match x.gpu_kind.as_deref() {
            Some("C") => "comp",
            Some("G") => "gfx",
            _ => "",
        };
        let mut l = vec![
            dim(format!("{:>7} ", x.pid)),
            span(format!("{:<18}", clip(&x.name, 18)), GPU),
            dim(format!("{kind:<5}")),
            span(
                format!("{:<22}", clip(x.container.as_deref().unwrap_or("·"), 22)),
                if x.container.is_some() { SOC } else { FAINT },
            ),
            bold(format!("{:>8}", x.gpu_mem.map(size).unwrap_or_else(|| "·".into())), GPU),
            span(format!("{:>8}", size(x.rss)), FG),
            dim(format!("{:>8.1}  ", x.cpu.unwrap_or(0.0))),
        ];
        l.extend(meter(bar, x.gpu_mem.unwrap_or(0) as f64 / total, &Grad::mono(GPU)));
        p.line(1 + i as u16, l);
    }
}

// ---------- CPU ----------

pub fn cpu(buf: &mut Buffer, area: Rect, app: &App) {
    let s = &app.snap;
    let Some(c) = &s.cpu else {
        unavailable(buf, area, "CPU", "no CPU data");
        return;
    };
    let [clusters, sched] = Layout::vertical([Constraint::Min(0), Constraint::Length(4)]).areas(area);
    let halves = split2(clusters, 50, STACK_BELOW);
    let zone = |label: &str| -> Option<f64> {
        s.thermal.as_ref()?.zones.iter().find(|z| z.label.as_deref() == Some(label)).map(|z| z.celsius)
    };
    let kinds = [
        ("P", "P-CORES", PCORE, c.p_util, "cpu_p", "cpu_p"),
        ("E", "E-CORES", ECORE, c.e_util, "cpu_e", "cpu_e"),
    ];
    for (rect, (kind, title, colr, util, rail, hist)) in halves.into_iter().zip(kinds) {
        let cores: Vec<_> = c.cores.iter().filter(|x| x.kind == kind).collect();
        let mut right = vec![pct_span(util.unwrap_or(0.0))];
        if let Some(w) = s.spbm.as_ref().and_then(|p| p.rail(rail)) {
            right.push(dim(" · "));
            right.push(span(format!("{w:.1} W"), POWER));
        }
        let mut p = panel(buf, rect, title, colr, right);
        let (w, h) = (p.width(), p.area.height);

        // Summary line: core count, frequency range, temperature per cluster.
        let mut line = vec![dim(format!("{} cores", cores.len()))];
        if let Some(max) = cores.iter().filter_map(|x| x.max_mhz).reduce(f64::max) {
            line.push(dim(format!(" up to {:.1} GHz", max / 1000.0)));
        }
        let mut clus: Vec<u32> = cores.iter().filter_map(|x| x.cluster).collect();
        clus.dedup();
        for n in clus {
            if let Some(t) = zone(&format!("cpu_{}_clu{n}", kind.to_lowercase())) {
                line.push(dim(format!("   cluster {n} ")));
                line.push(temp_span(t));
            }
        }
        p.line(0, line);

        // The history graph takes whatever the core list leaves.
        let gh = h.saturating_sub(2 + cores.len() as u16).min(20);
        if gh >= 2 {
            axis_graph(&mut p, 1, gh, app, hist, (0.0, 100.0), &Grad::mono(colr), |v| format!("{v:.0}%"));
        }
        let top = if gh >= 2 { 1 + gh } else { 1 };
        let trend_x = 33u16;
        let trend_w = (w as u16).saturating_sub(trend_x);
        p.line(top, vec![dim(format!("{:<5}{:<4}{:<17}{:>5}  {}", "CORE", "CLU", "LOAD", "MHz", if trend_w >= 6 { "TREND" } else { "" }))]);
        for (i, core) in cores.iter().enumerate() {
            let y = top + 1 + i as u16;
            let u = core.util.unwrap_or(0.0);
            let mut l = vec![
                span(format!("{}{:<4}", core.kind, core.id), colr),
                dim(format!("{:<4}", core.cluster.map(|n| n.to_string()).unwrap_or_default())),
            ];
            l.extend(meter(11, u / 100.0, &Grad::heat()));
            l.push(span(format!(" {u:>3.0}% "), heat(u / 100.0)));
            l.push(dim(format!("{:>5}", core.freq_mhz.map(|f| format!("{f:.0}")).unwrap_or_default())));
            p.line(y, l);
            if trend_w >= 6 {
                let data = app.series.tail(&format!("core/{}", core.id), trend_w as usize * 2);
                p.graph(trend_x, y, trend_w, 1, &data, 100.0, &Grad::mono(colr));
            }
        }
    }

    let mut p = panel(buf, sched, "SCHEDULER", ACCENT, vec![pct_span(c.util.unwrap_or(0.0)), dim(" overall")]);
    let w = p.width();
    let throttle = s.thermal.as_ref().map(|t| t.cpu_cooling_max_state).unwrap_or(0);
    p.line(0, fit(vec![
        vec![dim("load "), span(format!("{:.2} {:.2} {:.2}", c.load[0], c.load[1], c.load[2]), FG), dim(" (1 / 5 / 15 min)")],
        vec![dim("   governor "), span(c.governor.clone().unwrap_or_else(|| "?".into()), FG)],
        vec![dim("   thermal throttle "), if throttle > 0 { badge(&format!("state {throttle}"), BAD) } else { span("● off", OK) }],
    ], w));
    let mut l = Vec::new();
    if let Some(pr) = s.psi.as_ref().and_then(|p| p.cpu.as_ref()) {
        l.push(vec![dim("pressure "), span(format!("{:.1}%", pr.some10), heat(pr.some10 / 25.0)), dim(" of the last 10 s some task waited for a core")]);
    }
    if let Some(sp) = &s.spbm {
        let watts = sp.rail("cpu_p").unwrap_or(0.0) + sp.rail("cpu_e").unwrap_or(0.0);
        l.push(vec![dim("   cores draw "), span(format!("{watts:.1} W"), POWER)]);
    }
    p.line(1, fit(l, w));
}

// ---------- thermal ----------

pub fn thermal(buf: &mut Buffer, area: Rect, app: &App) {
    let s = &app.snap;
    let Some(t) = &s.thermal else {
        unavailable(buf, area, "THERMAL", "no thermal data");
        return;
    };
    let orange = (255, 140, 80);
    let crit = t.zones.iter().filter_map(|z| z.crit_c).reduce(f64::min);
    // The sensor and cooling tables need about 13 rows; the graph takes the rest.
    let top_h = area.height.saturating_sub(13).max(6);
    let [top, bottom] = Layout::vertical([Constraint::Length(top_h), Constraint::Min(0)]).areas(area);

    let hi = crit.unwrap_or(105.0);
    let mut p = panel(buf, top, "JUNCTION TEMPERATURE", orange, vec![]);
    let h = p.area.height;
    let n = axis_graph(&mut p, 0, h, app, "tj", (30.0, hi), &Grad::heat(), |v| format!("{v:.0}°"));
    // Redraw the frame now that the time window covered by the graph is known.
    let mut right = Vec::new();
    if let Some(tj) = tj_max(s) {
        right.push(temp_span(tj));
        right.push(dim("C"));
        if let Some((_, _, max)) = app.series.stats("tj") {
            right.push(dim(format!(" · max {max:.0}°")));
        }
        if let Some(c) = crit {
            right.push(dim(format!(" · {:.0}° below critical", c - tj)));
        }
        right.push(dim(format!(" · {}", window(app, n))));
    }
    panel(buf, top, "JUNCTION TEMPERATURE", orange, right);

    let [l, r] = split2(bottom, 62, STACK_BELOW);
    let mut p = panel(buf, l, "SENSORS", orange, vec![dim("NVMe and Wi-Fi refresh every 5 s")]);
    let rows: Vec<Row> = sensors(s)
        .into_iter()
        .map(|(name, c)| Row {
            key: format!("temp/{name}"),
            name,
            color: heat(temp_frac(c)),
            unit: "°",
            prec: 1,
        })
        .collect();
    series_table(&mut p, 0, "SENSOR", &rows, app);

    let right = vec![if t.cpu_cooling_max_state > 0 {
        badge("THROTTLING", BAD)
    } else {
        span("● no throttling", OK)
    }];
    let mut p = panel(buf, r, "COOLING", orange, right);
    p.line(0, vec![
        dim("CPU throttle    "),
        span(format!("state {}", t.cpu_cooling_max_state), if t.cpu_cooling_max_state > 0 { BAD } else { FG }),
        dim(format!(" across {} cores", t.cpu_cooling_count)),
    ]);
    let mut row = 1;
    for c in &t.cooling {
        // "PCIe_Port_Link_Speed_0004:00:00.0" -> "PCIe 0004:00:00.0 speed"
        let name = match c.kind.strip_prefix("PCIe_Port_Link_Speed_") {
            Some(addr) => format!("PCIe {addr}"),
            None => c.kind.clone(),
        };
        p.line(row, vec![
            dim(format!("{name:<22}")),
            span(format!("limit step {}/{}", c.cur, c.max), if c.cur > 0 { WARN } else { FG }),
        ]);
        row += 1;
    }
    row += 1;
    p.line(row, vec![dim("fan             no speed sensor on this platform")]);
    let a = &app.cfg.alerts;
    p.line(row + 1, vec![dim(format!(
        "alerts          warn {:.0}°  critical {:.0}°",
        a.tj_warn_c, a.tj_crit_c
    ))]);
    if let Some(c) = crit {
        p.line(row + 2, vec![dim(format!("firmware trip   {c:.1}° (shutdown)"))]);
    }
}

// ---------- storage, network, PCIe ----------

fn pci_kind(class: Option<&str>) -> &'static str {
    match class.map(|c| c.get(..6).unwrap_or(c)) {
        Some("0x0604") => "bridge",
        Some("0x0108") => "NVMe",
        Some("0x0200") => "Ethernet",
        Some("0x0207") => "InfiniBand",
        Some("0x0280") => "Wi-Fi",
        Some("0x0300") => "GPU",
        _ => "device",
    }
}

/// Leading number of a link speed such as "16.0 GT/s PCIe".
fn gts(speed: &str) -> Option<f64> {
    speed.split_ascii_whitespace().next()?.parse().ok()
}

pub fn io(buf: &mut Buffer, area: Rect, app: &App) {
    let s = &app.snap;
    let pcie_h = s.pcie.len() as u16 + 3;
    let show_pcie = area.height >= 9 + 8 + pcie_h;
    let mut cons = vec![Constraint::Length(9), Constraint::Min(0)];
    if show_pcie {
        cons.push(Constraint::Length(pcie_h));
    }
    let rows = Layout::vertical(cons).split(area);
    let auto = |a: &[f64], b: &[f64], floor: f64| a.iter().chain(b).copied().fold(floor, f64::max);

    // Storage: read and write side by side on one scale.
    let d = s.disks.first();
    let right = d
        .map(|d| vec![dim(format!("{} · busy {:.0}%", d.model.clone().unwrap_or_default().trim(), d.busy_pct.unwrap_or(0.0)))])
        .unwrap_or_default();
    let mut p = panel(buf, rows[0], "STORAGE", APPS, right);
    let w = p.width() as u16;
    let hw = w.saturating_sub(1) / 2;
    if let Some(d) = d {
        let (r, wr) = (app.series.tail("disk_r", hw as usize * 2), app.series.tail("disk_w", hw as usize * 2));
        let max = auto(&r, &wr, 1e6);
        p.line(0, vec![
            bold(format!("{}  ", d.name), FG), span("▼ ", RX), dim("read "),
            span(rate(d.read_bps.unwrap_or(0.0)), FG),
            dim(format!("  {:.0} iops", d.read_iops.unwrap_or(0.0))),
        ]);
        p.line_at(hw + 1, 0, vec![
            span("▲ ", TX), dim("write "), span(rate(d.write_bps.unwrap_or(0.0)), FG),
            dim(format!("  {:.0} iops   scale {}", d.write_iops.unwrap_or(0.0), rate(max))),
        ]);
        p.graph(0, 1, hw, 4, &r, max, &Grad::mono(RX));
        p.graph(hw + 1, 1, hw, 4, &wr, max, &Grad::mono(TX));
    }
    let health = match s.smart.first() {
        Some(h) => {
            let opt = |v: Option<u64>| v.map(|v| v.to_string()).unwrap_or_else(|| "?".into());
            let ok = h.critical_warning == 0;
            fit(vec![
                vec![dim("health "), if ok { span("● ok", OK) } else { badge("SMART WARNING", BAD) }],
                vec![dim("   wear "), span(format!("{}%", opt(h.percent_used)), FG)],
                vec![dim("   spare "), span(format!("{}%", opt(h.avail_spare_pct)), FG)],
                vec![dim("   written "), span(format!("{:.2} TB", h.written_bytes.unwrap_or(0) as f64 / 1e12), FG)],
                vec![dim("   on "), span(format!("{} h", opt(h.power_on_hours)), FG)],
                vec![dim("   media errors "), span(opt(h.media_errors), FG)],
                vec![dim("   unsafe shutdowns "), span(opt(h.unsafe_shutdowns), FG)],
            ], w as usize)
        }
        None => vec![dim("health  drive wear and error counters need root: run gb10top with sudo")],
    };
    p.line(6, health);

    // Network: every interface, then history for the primary one.
    let cx = s.nets.iter().any(|n| n.driver.as_deref() == Some("mlx5_core"));
    let right = vec![if cx { span("● ConnectX up", OK) } else { dim("ConnectX ports idle (appear when a cable links)") }];
    let mut p = panel(buf, rows[1], "NETWORK", ACCENT, right);
    let (w, h) = (p.width() as u16, p.area.height);
    p.line(0, vec![dim(format!(
        "{:<17}{:<11}{:<9}{:>10}{:>13}{:>13}{:>8}{:>8}",
        "INTERFACE", "DRIVER", "STATE", "SPEED", "RX", "TX", "ERRORS", "DROPS"
    ))]);
    // Physical interfaces first, then virtual ones that are carrying a link.
    let mut nets: Vec<_> = s.nets.iter().filter(|n| n.physical).collect();
    nets.extend(s.nets.iter().filter(|n| !n.physical && n.state != "down"));
    let graph_h = if h >= nets.len() as u16 + 5 { (h - nets.len() as u16 - 2).min(6) } else { 0 };
    let list_rows = h.saturating_sub(1 + if graph_h > 0 { graph_h + 1 } else { 0 }) as usize;
    for (i, n) in nets.iter().take(list_rows).enumerate() {
        let state_c = match n.state.as_str() {
            "up" => OK,
            "down" => FAINT,
            _ => DIM,
        };
        let bad = n.rx_errors + n.tx_errors > 0;
        let name: String = n.name.chars().take(16).collect();
        p.line(1 + i as u16, vec![
            span(format!("{name:<17}"), if n.physical { FG } else { DIM }),
            dim(format!("{:<11}", n.driver.as_deref().unwrap_or("virtual"))),
            span(format!("{:<9}", n.state), state_c),
            dim(format!("{:>10}", n.speed_mbps.map(|m| format!("{m} Mb/s")).unwrap_or_else(|| "·".into()))),
            span(format!("{:>13}", rate(n.rx_bps.unwrap_or(0.0))), FG),
            span(format!("{:>13}", rate(n.tx_bps.unwrap_or(0.0))), FG),
            span(format!("{:>8}", n.rx_errors + n.tx_errors), if bad { BAD } else { DIM }),
            dim(format!("{:>8}", n.rx_dropped + n.tx_dropped)),
        ]);
    }
    if let (Some(n), true) = (primary_net(s), graph_h > 0) {
        let top = h - graph_h - 1;
        let hw = w.saturating_sub(1) / 2;
        let (rx, tx) = (app.series.tail("net_rx", hw as usize * 2), app.series.tail("net_tx", hw as usize * 2));
        let max = auto(&rx, &tx, 1e4);
        p.line(top, vec![bold(format!("{}  ", n.name), FG), span("▼ ", RX), dim("receive")]);
        p.line_at(hw + 1, top, vec![span("▲ ", TX), dim(format!("transmit   scale {}", rate(max)))]);
        p.graph(0, top + 1, hw, graph_h, &rx, max, &Grad::mono(RX));
        p.graph(hw + 1, top + 1, hw, graph_h, &tx, max, &Grad::mono(TX));
    }

    if show_pcie {
        let mut p = panel(buf, rows[2], "PCIe LINKS", SOC, vec![dim("negotiated against the port's maximum")]);
        p.line(0, vec![dim(format!("{:<15}{:<12}{:<22}{:<22}", "ADDRESS", "DEVICE", "LINK", "MAXIMUM"))]);
        for (i, l) in s.pcie.iter().enumerate() {
            let width = |w: Option<u32>| w.map(|w| format!(" x{w}")).unwrap_or_default();
            let below = match (gts(&l.speed), l.max_speed.as_deref().and_then(gts)) {
                (Some(cur), Some(max)) => cur < max || l.width < l.max_width,
                _ => false,
            };
            let short = |sp: &str| sp.replace(" PCIe", "");
            p.line(1 + i as u16, vec![
                span(format!("{:<15}", l.addr), FG),
                dim(format!("{:<12}", pci_kind(l.class.as_deref()))),
                span(format!("{:<22}", format!("{}{}", short(&l.speed), width(l.width))), if below { WARN } else { OK }),
                dim(format!("{:<22}", format!("{}{}", short(l.max_speed.as_deref().unwrap_or("?")), width(l.max_width)))),
                dim(if below { "below maximum" } else { "" }),
            ]);
        }
    }
}

// ---------- events ----------

pub fn events(buf: &mut Buffer, area: Rect, app: &App) {
    let [l, r] = split2(area, 60, STACK_BELOW);
    let level = |lv: Level| if lv == Level::Crit { ("CRIT", BAD) } else { ("WARN", WARN) };

    let mut p = panel(buf, l, "EVENT LOG", ACCENT, vec![dim(format!("{} events · newest first", app.alerts.events.len()))]);
    let rows = p.area.height as usize;
    let recent = app.alerts.events.tail(rows);
    if recent.is_empty() {
        p.line(0, vec![span("● ", OK), dim(format!("no alerts in the {} this session has run", duration(app.elapsed_s)))]);
    }
    for (i, e) in recent.iter().rev().enumerate() {
        let (name, c) = level(e.level);
        p.line(i as u16, vec![
            dim(format!("{}  ", clock(e.at))),
            if e.raised { span("▲ raised   ", c) } else { span("▼ cleared  ", OK) },
            span(format!("{name}  "), if e.raised { c } else { DIM }),
            span(e.text.clone(), if e.raised { FG } else { DIM }),
        ]);
    }

    let mut p = panel(buf, r, "ACTIVE · THRESHOLDS", ACCENT, vec![]);
    let mut row = 0;
    let active: Vec<_> = app.alerts.active().collect();
    if active.is_empty() {
        p.line(0, vec![span("● ", OK), dim("nothing active")]);
        row = 1;
    }
    for a in &active {
        p.line(row, vec![badge(a.badge, level(a.level).1), Span::raw(" "), span(a.text.clone(), FG)]);
        row += 1;
    }
    let t = &app.cfg.alerts;
    let lines = [
        format!("junction temp     warn {:.0}°  critical {:.0}°", t.tj_warn_c, t.tj_crit_c),
        format!("memory available  warn {:.0}%  critical {:.0}%", t.mem_available_warn_pct, t.mem_available_crit_pct),
        format!("memory pressure   warn {:.0}%", t.mem_pressure_warn_pct),
        format!("swap in use       warn {:.0}%", t.swap_used_warn_pct),
        format!("NVMe temp         warn {:.0}°", t.disk_temp_warn_c),
        "always on         power cap, throttling, prochot, SMART".to_string(),
    ];
    row += 1;
    p.line(row, vec![dim("THRESHOLDS")]);
    for l in lines {
        row += 1;
        p.line(row, vec![dim(l)]);
    }
    row += 2;
    p.line(row, match (&app.cfg.log, &app.log_error) {
        (_, Some(e)) => vec![badge("LOG FAILED", BAD), Span::raw(" "), span(e.clone(), FG)],
        (Some(path), None) => vec![span("● ", OK), dim(format!("logging to {}", path.display()))],
        (None, None) => vec![dim("CSV logging off (--log FILE)")],
    });
    p.line(row + 1, vec![dim(format!("bell on critical: {}", if app.cfg.bell { "on" } else { "off" }))]);
    p.line(row + 2, vec![dim("defaults: gb10top --print-config")]);
}
