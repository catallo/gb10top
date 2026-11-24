//! The overview screen: header, eight panels and a footer.

use super::theme::*;
use super::widgets::{meter, panel, stacked};
use super::{detail, gpu_watts, primary_net, sensors, tj_max, Action, App, MouseTargets, Sort, Tab};
use crate::alerts::Level;
use crate::collect::cpu::Core;
use crate::collect::spbm::Limit;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

/// Smallest terminal the app draws in at all.
const MIN_W: u16 = 60;
const MIN_H: u16 = 14;
/// Smallest terminal that gets the full overview rather than the compact one.
const FULL_W: u16 = 110;
const FULL_H: u16 = 31;
pub(super) const GIB: f64 = (1u64 << 30) as f64;

// ---------- formatting ----------

pub(super) fn gib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / GIB)
}

/// Compact size for table cells: 43.5G, 812M, 96K.
pub(super) fn size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1}G", b / GIB)
    } else if b >= 1048576.0 {
        format!("{:.0}M", b / 1048576.0)
    } else {
        format!("{:.0}K", b / 1024.0)
    }
}

/// Bytes per second.
pub(super) fn rate(bps: f64) -> String {
    if bps >= 1e9 {
        format!("{:.2} GB/s", bps / 1e9)
    } else if bps >= 1e6 {
        format!("{:.1} MB/s", bps / 1e6)
    } else {
        format!("{:.1} kB/s", bps / 1e3)
    }
}

pub(super) fn duration(secs: f64) -> String {
    let s = secs as u64;
    let (d, h, m) = (s / 86400, s % 86400 / 3600, s % 3600 / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m {}s", s % 60)
    }
}

/// Position of a temperature on the heat scale (35 C cool, 100 C hot).
pub(super) fn temp_frac(c: f64) -> f64 {
    ((c - 35.0) / 65.0).clamp(0.0, 1.0)
}

pub(super) fn temp_span(c: f64) -> Span<'static> {
    bold(format!("{c:.0}°"), heat(temp_frac(c)))
}

pub(super) fn pct_span(v: f64) -> Span<'static> {
    bold(format!("{v:.0}%"), heat(v / 100.0))
}

/// Colour key for a stacked bar. Gaps shrink, then the swatch spacing, to
/// make it fit `width`.
pub(super) fn legend(items: &[(&str, String, Rgb)], width: usize) -> Vec<Span<'static>> {
    let build = |gap: usize, swatch: &str| -> Vec<Span<'static>> {
        let mut out = Vec::new();
        for (i, (name, value, c)) in items.iter().enumerate() {
            if i > 0 {
                out.push(Span::raw(" ".repeat(gap)));
            }
            out.push(span(swatch.to_string(), *c));
            out.push(dim(format!("{name} ")));
            out.push(span(value.clone(), FG));
        }
        out
    };
    [(3, "■ "), (2, "■ "), (1, "■ "), (1, "■")]
        .into_iter()
        .map(|(gap, swatch)| build(gap, swatch))
        .find(|l| text_width(l) <= width)
        .unwrap_or_else(|| build(1, "■"))
}

/// Join groups of spans, keeping the leading groups that fit in `width`.
pub(super) fn fit(groups: Vec<Vec<Span<'static>>>, width: usize) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    for g in groups {
        if text_width(&out) + text_width(&g) > width {
            break;
        }
        out.extend(g);
    }
    out
}

pub(super) fn text_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.width()).sum()
}

// ---------- screen ----------

pub(super) fn draw(buf: &mut Buffer, area: Rect, app: &App) -> MouseTargets {
    let mut targets = MouseTargets::default();
    if area.width < MIN_W || area.height < MIN_H {
        let msg = format!(
            "gb10top needs at least {MIN_W}x{MIN_H} (this terminal is {}x{})",
            area.width, area.height
        );
        if area.height > 1 {
            buf.set_line(area.x, area.y, &Line::from(span(msg, WARN)), area.width);
        }
        if area.height > 0 {
            controls(
                buf,
                Rect::new(area.x, area.bottom() - 1, area.width, 1),
                &mut targets,
                [(button_face(" Quit ", false), Some(Action::Quit))],
            );
        }
        return targets;
    }
    let [head, tabs, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(if app.tab == Tab::Procs { 2 } else { 1 }),
    ])
    .areas(area);
    header(buf, head, app);
    tab_bar(buf, tabs, app, &mut targets);
    match app.tab {
        Tab::Overview if area.width >= FULL_W && area.height >= FULL_H => overview(buf, body, app, &mut targets),
        Tab::Overview => compact(buf, body, app, &mut targets),
        Tab::Power => detail::power(buf, body, app),
        Tab::Gpu => detail::gpu(buf, body, app),
        Tab::Cpu => detail::cpu(buf, body, app),
        Tab::Thermal => detail::thermal(buf, body, app),
        Tab::Io => detail::io(buf, body, app),
        Tab::Procs => {
            targets.processes = Some(body);
            targets.scroll_end = Some(app.snap.procs.len().saturating_sub(process_rows(body)));
            processes(buf, body, app, Some(app.scroll), &mut targets);
        }
        Tab::Events => detail::events(buf, body, app),
    }
    footer(buf, foot, app, &mut targets);
    targets
}

/// Split into two columns, or two rows when the area is narrower than
/// `min_cols_width`.
pub(super) fn split2(area: Rect, left_pct: u16, min_cols_width: u16) -> [Rect; 2] {
    let cons = [Constraint::Percentage(left_pct), Constraint::Percentage(100 - left_pct)];
    if area.width >= min_cols_width {
        Layout::horizontal(cons).areas(area)
    } else {
        Layout::vertical(cons).areas(area)
    }
}

/// The full overview: every subsystem at once.
fn overview(buf: &mut Buffer, area: Rect, app: &App, targets: &mut MouseTargets) {
    // The thermal / I/O row is dropped on short terminals.
    let tall = area.height >= 38;
    let mut cons = vec![Constraint::Length(12), Constraint::Length(10)];
    if tall {
        cons.push(Constraint::Length(8));
    }
    cons.push(Constraint::Min(3));
    let rows = Layout::vertical(cons).split(area);
    let [l, r] = split2(rows[0], 50, 0);
    cpu(buf, l, app);
    gpu(buf, r, app);
    let [l, r] = split2(rows[1], 58, 0);
    power(buf, l, app);
    memory(buf, r, app);
    let mut next = 2;
    if tall {
        let [l, r] = split2(rows[next], 50, 0);
        thermal(buf, l, app);
        io(buf, r, app);
        next += 1;
    }
    processes(buf, rows[next], app, None, targets);
}

/// Overview for small terminals: one meter line per subsystem, then processes.
fn compact(buf: &mut Buffer, area: Rect, app: &App, targets: &mut MouseTargets) {
    let s = &app.snap;
    let [top, rest] = Layout::vertical([Constraint::Length(8), Constraint::Min(0)]).areas(area);
    let mut p = panel(buf, top, "SUMMARY", ACCENT, vec![dim("tabs 2-6 show each subsystem in full")]);
    let w = p.width();
    let bar = (w.saturating_sub(40)).clamp(8, 40);
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut line = |label: &str, c: Rgb, frac: f64, value: Span<'static>, rest: Vec<Span<'static>>| {
        let mut l = vec![bold(format!("{label:<7}"), c)];
        l.extend(meter(bar, frac, &Grad::heat()));
        l.push(Span::raw(" "));
        l.push(value);
        l.push(Span::raw("  "));
        l.extend(rest);
        lines.push(l);
    };
    if let Some(c) = &s.cpu {
        let u = c.util.unwrap_or(0.0);
        line("CPU", ACCENT, u / 100.0, pct_span(u), vec![dim(format!(
            "P {:.0}%  E {:.0}%  load {:.2}",
            c.p_util.unwrap_or(0.0),
            c.e_util.unwrap_or(0.0),
            c.load[0]
        ))]);
    }
    if let Some(g) = &s.gpu {
        let u = g.util.unwrap_or(0) as f64;
        let mut rest = Vec::new();
        if let Some(w) = gpu_watts(s) {
            rest.push(dim(format!("{w:.0} W  ")));
        }
        if let Some(clk) = g.clock_mhz {
            rest.push(dim(format!("{clk} MHz  ")));
        }
        rest.push(dim(format!("mem {} GiB", gib(g.procs_mem()))));
        line("GPU", GPU, u / 100.0, pct_span(u), rest);
    }
    if let Some(sp) = &s.spbm {
        let sys = sp.rail("sys_total").unwrap_or(0.0);
        let cap = sp.limits.iter().find(|l| l.label == "syspl2").and_then(|l| l.cap_w).unwrap_or(240.0);
        line("POWER", POWER, sys / cap, bold(format!("{sys:.0} W"), POWER), vec![dim(format!(
            "of {cap:.0} W  package {:.0} W",
            sp.rail("soc_pkg").unwrap_or(0.0)
        ))]);
    }
    if let Some(m) = &s.mem {
        let frac = m.used() as f64 / m.total.max(1) as f64;
        line("MEMORY", APPS, frac, pct_span(frac * 100.0), vec![dim(format!(
            "{} of {} GiB",
            gib(m.used()),
            gib(m.total)
        ))]);
    }
    if let Some(t) = tj_max(s) {
        let crit = s.thermal.as_ref().and_then(|t| t.zones.iter().filter_map(|z| z.crit_c).reduce(f64::min));
        let rest = crit.map(|c| vec![dim(format!("{:.0}° below critical", c - t))]).unwrap_or_default();
        line("TEMP", (255, 140, 80), temp_frac(t), bold(format!("{t:.0}°C"), heat(temp_frac(t))), rest);
    }
    let disk_r: f64 = s.disks.iter().filter_map(|d| d.read_bps).sum();
    let disk_w: f64 = s.disks.iter().filter_map(|d| d.write_bps).sum();
    let mut l = vec![
        bold(format!("{:<7}", "I/O"), ACCENT),
        dim("disk "), span("▼ ", RX), span(rate(disk_r), FG), span("  ▲ ", TX), span(rate(disk_w), FG),
    ];
    if let Some(n) = primary_net(s) {
        l.extend([
            dim("   net "), span("▼ ", RX), span(rate(n.rx_bps.unwrap_or(0.0)), FG),
            span("  ▲ ", TX), span(rate(n.tx_bps.unwrap_or(0.0)), FG),
        ]);
    }
    lines.push(l);
    for (row, l) in lines.into_iter().enumerate() {
        p.line(row as u16, l);
    }
    processes(buf, rest, app, None, targets);
}

fn tab_bar(buf: &mut Buffer, area: Rect, app: &App, targets: &mut MouseTargets) {
    let active = app.alerts.active().count();
    let build = |names: bool| -> Vec<Span<'static>> {
        let mut l = vec![Span::raw(" ")];
        for (i, t) in Tab::ALL.iter().enumerate() {
            let mut text = if names || *t == app.tab {
                format!("{} {}", i + 1, t.title())
            } else {
                format!("{}", i + 1)
            };
            if *t == Tab::Events && active > 0 {
                text.push_str(&format!(" ({active})"));
            }
            l.push(button_face(format!(" {text} "), *t == app.tab));
        }
        l
    };
    // Tab names are dropped (all but the active one) when they do not fit.
    let mut l = build(true);
    if text_width(&l) + Tab::ALL.len() - 1 > area.width as usize {
        l = build(false);
    }
    let actions = std::iter::once(None).chain(Tab::ALL.map(|tab| Some(Action::Tab(tab))));
    controls(buf, area, targets, l.into_iter().zip(actions));
}

fn header(buf: &mut Buffer, area: Rect, app: &App) {
    let s = &app.snap;
    let logo = "◢◤ gb10top";
    let grad = Grad::between(ACCENT, GPU);
    let n = logo.chars().count();
    let mut left: Vec<Span> = logo
        .chars()
        .enumerate()
        .map(|(i, ch)| bold(ch.to_string(), grad.at(i as f64 / (n - 1) as f64)))
        .collect();
    left.push(Span::raw("  "));
    left.push(bold(app.model.clone(), FG));
    if let Some(name) = s.gpu.as_ref().and_then(|g| g.name.clone()) {
        left.push(dim(format!(" · {name}")));
    }
    left.push(dim(format!(" · {} · up {}", app.host, duration(app.uptime_s))));

    let tj = tj_max(s);
    let mut right: Vec<Span> = Vec::new();
    if app.paused {
        right.push(badge("PAUSED", WARN));
        right.push(Span::raw(" "));
    }
    if app.log_error.is_some() {
        right.push(badge("LOG FAILED", BAD));
        right.push(Span::raw(" "));
    }
    let active: Vec<_> = app.alerts.active().collect();
    for a in &active {
        right.push(badge(a.badge, if a.level == Level::Crit { BAD } else { WARN }));
        right.push(Span::raw(" "));
    }
    if active.is_empty() {
        right.push(badge("NOMINAL", OK));
        right.push(Span::raw(" "));
    }
    if let Some(w) = s.spbm.as_ref().and_then(|p| p.rail("sys_total")) {
        right.push(bold(format!(" {w:.1} W"), POWER));
    }
    if let Some(t) = tj {
        right.push(Span::raw("  "));
        right.push(temp_span(t));
        right.push(dim("C"));
    }
    right.push(Span::raw(" "));

    let rw = text_width(&right) as u16;
    buf.set_line(area.x + 1, area.y, &Line::from(left), area.width.saturating_sub(rw + 2));
    buf.set_line(area.right().saturating_sub(rw), area.y, &Line::from(right), rw);
}

// ---------- CPU ----------

pub(super) fn core_row(core: &Core, width: usize, c: Rgb) -> Vec<Span<'static>> {
    let util = core.util.unwrap_or(0.0);
    let mut out = vec![span(format!("{}{:<2} ", core.kind, core.id), c)];
    out.extend(meter(width.saturating_sub(9), util / 100.0, &Grad::heat()));
    out.push(span(format!(" {util:>3.0}%"), heat(util / 100.0)));
    out
}

pub(super) fn cpu(buf: &mut Buffer, area: Rect, app: &App) {
    let s = &app.snap;
    let Some(c) = &s.cpu else {
        panel(buf, area, "CPU", ACCENT, vec![]).line(0, vec![dim("no CPU data")]);
        return;
    };
    let right = vec![
        dim("load "),
        span(format!("{:.2} {:.2} {:.2}", c.load[0], c.load[1], c.load[2]), FG),
        dim(format!(" · {} · ", c.governor.as_deref().unwrap_or("?"))),
        pct_span(c.util.unwrap_or(0.0)),
    ];
    let mut p = panel(buf, area, "CPU", ACCENT, right);
    let (w, h) = (p.width(), p.area.height);

    // Left: one meter per core, P cores and E cores side by side.
    let lw = w * 56 / 100;
    let half = lw.saturating_sub(2) / 2;
    let of_kind = |k: &str| -> Vec<&Core> { c.cores.iter().filter(|x| x.kind == k).collect() };
    let (pc, ec) = (of_kind("P"), of_kind("E"));
    for row in 0..h {
        if let Some(core) = pc.get(row as usize) {
            p.line_at(0, row, core_row(core, half, PCORE));
        }
        if let Some(core) = ec.get(row as usize) {
            p.line_at(half as u16 + 2, row, core_row(core, half, ECORE));
        }
    }

    // Right: utilisation history per cluster.
    let x = lw as u16 + 1;
    let gw = (w as u16).saturating_sub(x);
    let gh = h.saturating_sub(2) / 2;
    let zone_max = |prefix: &str| -> Option<f64> {
        s.thermal.as_ref()?.zones.iter()
            .filter(|z| z.label.as_deref().is_some_and(|l| l.starts_with(prefix)))
            .map(|z| z.celsius)
            .reduce(f64::max)
    };
    let clusters = [
        ("P-cores", &pc, PCORE, c.p_util, zone_max("cpu_p"), "cpu_p"),
        ("E-cores", &ec, ECORE, c.e_util, zone_max("cpu_e"), "cpu_e"),
    ];
    for (i, (name, cores, colr, util, temp, hist)) in clusters.into_iter().enumerate() {
        let top = i as u16 * (gh + 1);
        let freqs: Vec<f64> = cores.iter().filter_map(|x| x.freq_mhz).collect();
        let mut label = vec![bold(format!("{name} "), colr), pct_span(util.unwrap_or(0.0))];
        if !freqs.is_empty() {
            let mean = freqs.iter().sum::<f64>() / freqs.len() as f64;
            label.push(dim(format!(" {:.1} GHz ", mean / 1000.0)));
        }
        if let Some(t) = temp {
            label.push(temp_span(t));
        }
        p.line_at(x, top, label);
        p.graph(x, top + 1, gw, gh, &app.series.tail(hist, gw as usize * 2), 100.0, &Grad::mono(colr));
    }
}

// ---------- GPU ----------

pub(super) fn gpu(buf: &mut Buffer, area: Rect, app: &App) {
    let s = &app.snap;
    let Some(g) = &s.gpu else {
        panel(buf, area, "GPU", GPU, vec![]).line(0, vec![dim("NVML not available")]);
        return;
    };
    let mut right = Vec::new();
    if let Some(t) = g.temp_c {
        right.push(temp_span(t as f64));
        right.push(dim("C · "));
    }
    right.push(pct_span(g.util.unwrap_or(0) as f64));
    let mut p = panel(buf, area, "GPU", GPU, right);
    let w = p.width() as u16;

    let mut l = vec![dim("UTIL "), pct_span(g.util.unwrap_or(0) as f64)];
    if let Some(clk) = g.clock_mhz {
        l.push(dim("   CLOCK "));
        l.push(span(format!("{clk}"), FG));
        l.push(dim(match g.max_clock_mhz {
            Some(m) => format!("/{m} MHz   "),
            None => " MHz   ".into(),
        }));
    }
    if g.throttle.is_empty() {
        l.push(span("● ", OK));
        l.push(dim("no throttle"));
    } else {
        l.push(badge(&g.throttle.join(" "), BAD));
    }
    p.line(0, l);
    p.graph(0, 1, w, 4, &app.series.tail("gpu_util", w as usize * 2), 100.0, &Grad::mono(GPU));

    let mut l = vec![dim("POWER ")];
    if let Some(watts) = gpu_watts(s) {
        l.push(bold(format!("{watts:.1} W"), POWER));
    }
    if let (Some(nvml), true) = (g.power_w, s.spbm.is_some()) {
        l.push(dim(format!(" rail · {nvml:.1} W nvml")));
    }
    l.push(dim(format!(
        "   ENC {}%  DEC {}%",
        g.enc_util.unwrap_or(0),
        g.dec_util.unwrap_or(0)
    )));
    p.line(5, l);
    let hist = app.series.tail("gpu_w", w as usize * 2);
    let max = hist.iter().copied().fold(120.0, f64::max);
    p.graph(0, 6, w, 3, &hist, max, &Grad::mono(POWER));

    let users: Vec<_> = g.procs.iter().filter(|x| x.mem.is_some()).collect();
    let mut l = vec![
        dim("MEM "),
        bold(format!("{} GiB", gib(g.procs_mem())), GPU),
        dim(format!(" in {} procs", users.len())),
    ];
    for u in &users {
        let item = vec![
            dim("  · "),
            span(u.name.clone(), FG),
            dim(format!(" {}", size(u.mem.unwrap_or(0)))),
        ];
        if text_width(&l) + text_width(&item) > w as usize {
            break;
        }
        l.extend(item);
    }
    p.line(9, l);
}

// ---------- power ----------

pub(super) fn limit_spans(name: &str, l: &Limit, width: usize) -> Vec<Span<'static>> {
    let cap = l.cap_w.unwrap_or(0.0);
    let frac = if cap > 0.0 { l.watts / cap } else { 0.0 };
    let mut out = vec![dim(format!("{name:<10}"))];
    out.extend(meter(width.saturating_sub(22), frac, &Grad::heat()));
    out.push(span(format!(" {:>3.0}", l.watts), heat(frac)));
    out.push(dim(format!("/{cap:<3.0} W  ")));
    out
}

pub(super) fn power(buf: &mut Buffer, area: Rect, app: &App) {
    let Some(sp) = &app.snap.spbm else {
        let mut p = panel(buf, area, "POWER", POWER, vec![]);
        p.line(0, vec![dim("power sensor (spbm / spark_hwmon driver) not present")]);
        if let Some(w) = gpu_watts(&app.snap) {
            p.line(1, vec![dim("GPU via NVML "), bold(format!("{w:.1} W"), POWER)]);
        }
        return;
    };
    let rail = |l: &str| sp.rail(l).unwrap_or(0.0);
    let limit = |l: &str| sp.limits.iter().find(|x| x.label == l);
    let (sys, pkg) = (rail("sys_total"), rail("soc_pkg"));
    let (g, cp, ce) = (rail("gpu"), rail("cpu_p"), rail("cpu_e"));
    let soc_other = (pkg - g - cp - ce).max(0.0);
    let board = (sys - pkg).max(0.0);
    let scale = limit("syspl2").and_then(|l| l.cap_w).unwrap_or(240.0);

    let capped = sp.pl_level.is_some_and(|l| l >= 2);
    let right = vec![
        dim(format!("limit {scale:.0} W · ")),
        if capped { badge("PACKAGE CAPPED", BAD) } else { span("● within limits", OK) },
    ];
    let mut p = panel(buf, area, "POWER", POWER, right);
    let w = p.width();

    p.line(0, fit(vec![
        vec![bold(format!("{sys:.1} W"), POWER), dim(" system")],
        vec![dim("   "), span(format!("{pkg:.1} W"), FG), dim(" package")],
        vec![dim("   peak "), span(format!("{:.0} W", app.peak_w), FG)],
        vec![dim("   "), span(format!("{:.2} Wh", app.wh), FG)],
        vec![dim(format!(" in {}", duration(app.elapsed_s)))],
    ], w));
    let segs = [(g, GPU), (cp, PCORE), (ce, ECORE), (soc_other, SOC), (board, BOARD)];
    p.line(1, stacked(w, &segs, scale));
    p.line(2, legend(&[
        ("gpu", format!("{g:.1}"), GPU),
        ("p-cores", format!("{cp:.1}"), PCORE),
        ("e-cores", format!("{ce:.1}"), ECORE),
        ("soc", format!("{soc_other:.1}"), SOC),
        ("board", format!("{board:.1}"), BOARD),
    ], w));
    p.graph(0, 3, w as u16, 3, &app.series.tail("sys_w", w * 2), scale, &Grad::heat());

    // The four firmware limits: the averages it tracks against their caps.
    let half = w / 2;
    let rows = [
        [("pkg slow", "pl1"), ("pkg fast", "pl2")],
        [("sys slow", "syspl1"), ("sys fast", "syspl2")],
    ];
    for (r, pair) in rows.iter().enumerate() {
        for (c, (name, label)) in pair.iter().enumerate() {
            if let Some(l) = limit(label) {
                p.line_at((c * half) as u16, 6 + r as u16, limit_spans(name, l, half));
            }
        }
    }
}

// ---------- memory ----------

pub(super) fn memory(buf: &mut Buffer, area: Rect, app: &App) {
    let s = &app.snap;
    let Some(m) = &s.mem else {
        panel(buf, area, "MEMORY", APPS, vec![]).line(0, vec![dim("no memory data")]);
        return;
    };
    let used = m.used();
    let used_pct = 100.0 * used as f64 / m.total.max(1) as f64;
    let mut p = panel(buf, area, "MEMORY", APPS, vec![dim("unified · "), pct_span(used_pct)]);
    let w = p.width();

    // GPU allocations come out of the same pool and are part of "used".
    let gpu_mem = s.gpu.as_ref().map(|g| g.procs_mem()).unwrap_or(0).min(used);
    let apps = used - gpu_mem;
    let cache = m.cached.min(m.available);
    let free = m.available - cache;

    p.line(0, vec![
        bold(format!("{} GiB", gib(used)), APPS),
        dim(format!(" used of {}   ", gib(m.total))),
        span(gib(m.available), FG),
        dim(" available"),
    ]);
    let f = |b: u64| b as f64;
    p.line(1, stacked(w, &[(f(gpu_mem), GPU), (f(apps), APPS), (f(cache), CACHE)], f(m.total)));
    p.line(2, legend(&[
        ("gpu", gib(gpu_mem), GPU),
        ("apps", gib(apps), APPS),
        ("cache", gib(cache), CACHE),
        ("free", gib(free), TRACK),
    ], w));
    p.graph(0, 3, w as u16, 2, &app.series.tail("mem", w * 2), 100.0, &Grad::heat());

    let swap_used = m.swap_total - m.swap_free;
    let mut l = vec![dim("swap      ")];
    l.extend(meter(w.saturating_sub(26), f(swap_used) / f(m.swap_total.max(1)), &Grad::heat()));
    l.push(span(format!(" {}", gib(swap_used)), FG));
    l.push(dim(format!("/{} GiB", gib(m.swap_total))));
    p.line(5, l);

    if let Some(psi) = &s.psi {
        let mut l = vec![dim("pressure  ")];
        for (name, pr) in [("cpu", &psi.cpu), ("mem", &psi.memory), ("io", &psi.io)] {
            if let Some(pr) = pr {
                l.push(dim(format!("{name} ")));
                l.push(span(format!("{:.1}%", pr.some10), heat(pr.some10 / 25.0)));
                l.push(Span::raw("   "));
            }
        }
        p.line(6, l);
    }
    p.line(7, vec![dim(format!("shared    {} GiB", gib(m.shmem)))]);
}

// ---------- thermal ----------

pub(super) fn thermal(buf: &mut Buffer, area: Rect, app: &App) {
    let s = &app.snap;
    let Some(t) = &s.thermal else {
        panel(buf, area, "THERMAL", BAD, vec![]).line(0, vec![dim("no thermal data")]);
        return;
    };
    let crit = t.zones.iter().filter_map(|z| z.crit_c).reduce(f64::min);
    let tj = tj_max(s);
    let mut right = Vec::new();
    if let (Some(tj), Some(crit)) = (tj, crit) {
        right.push(dim(format!("{:.0}° below critical {crit:.0}° · ", crit - tj)));
    }
    right.push(if t.cpu_cooling_max_state > 0 {
        badge(&format!("CPU THROTTLE {}", t.cpu_cooling_max_state), BAD)
    } else {
        span("● no throttling", OK)
    });
    let mut p = panel(buf, area, "THERMAL", (255, 140, 80), right);
    let w = p.width();

    // Top two rows: junction temperature history with its value on the left.
    if let Some(tj) = tj {
        p.line(0, vec![dim("Tj max ")]);
        p.line(1, vec![temp_span(tj), dim("C")]);
        let hist: Vec<f64> = app.series.tail("tj", (w - 8) * 2).iter().map(|v| v - 30.0).collect();
        p.graph(8, 0, w as u16 - 8, 2, &hist, 75.0, &Grad::heat());
    }

    // One meter per sensor in two columns.
    let sensors = sensors(s);
    let half = w / 2;
    let rows = p.area.height.saturating_sub(2) as usize;
    for (i, (name, c)) in sensors.iter().take(rows * 2).enumerate() {
        let mut l = vec![dim(format!("{name:<8}"))];
        l.extend(meter(half.saturating_sub(16), temp_frac(*c), &Grad::heat()));
        l.push(span(format!(" {c:>4.1}°"), heat(temp_frac(*c))));
        p.line_at(((i / rows) * half) as u16, 2 + (i % rows) as u16, l);
    }
}

// ---------- disk and network ----------

pub(super) fn io(buf: &mut Buffer, area: Rect, app: &App) {
    let s = &app.snap;
    // The 200G ConnectX ports only show up as mlx5 interfaces while a cable is in.
    let cx = s.nets.iter().any(|n| n.driver.as_deref() == Some("mlx5_core"));
    let right = vec![if cx { span("● ConnectX up", OK) } else { dim("ConnectX idle (no link)") }];
    let mut p = panel(buf, area, "DISK · NETWORK", ACCENT, right);
    let w = p.width();
    let label_w = 34.min(w / 2);
    let gx = label_w as u16 + 1;
    let gw = (w as u16).saturating_sub(gx);
    let auto = |v: &[f64], floor: f64| v.iter().copied().fold(floor, f64::max);

    if let Some(d) = s.disks.first() {
        p.line(0, vec![bold(d.name.clone(), FG), dim(format!("  busy {:.0}%", d.busy_pct.unwrap_or(0.0)))]);
        p.line(1, vec![
            span("▼ ", RX), dim("read  "), span(rate(d.read_bps.unwrap_or(0.0)), FG),
        ]);
        p.line(2, vec![
            span("▲ ", TX), dim("write "), span(rate(d.write_bps.unwrap_or(0.0)), FG),
        ]);
        let hist = app.series.tail("disk", gw as usize * 2);
        p.graph(gx, 0, gw, 3, &hist, auto(&hist, 1e6), &Grad::mono(APPS));
    }
    if let Some(n) = primary_net(s) {
        let speed = n.speed_mbps.map(|m| format!("  {m} Mb/s")).unwrap_or_default();
        p.line(3, vec![bold(n.name.clone(), FG), dim(format!("{speed}  {}", n.state))]);
        p.line(4, vec![
            span("▼ ", RX), dim("rx    "), span(rate(n.rx_bps.unwrap_or(0.0)), FG),
        ]);
        p.line(5, vec![
            span("▲ ", TX), dim("tx    "), span(rate(n.tx_bps.unwrap_or(0.0)), FG),
        ]);
        let (rx, tx) = (app.series.tail("net_rx", gw as usize * 2), app.series.tail("net_tx", gw as usize * 2));
        let max = auto(&rx, 1e4).max(auto(&tx, 1e4));
        // Receive on the left, transmit on the right, sharing one scale.
        let hw = gw.saturating_sub(1) / 2;
        let newest = |v: &[f64]| v[v.len().saturating_sub(hw as usize * 2)..].to_vec();
        p.graph(gx, 3, hw, 3, &newest(&rx), max, &Grad::mono(RX));
        p.graph(gx + hw + 1, 3, hw, 3, &newest(&tx), max, &Grad::mono(TX));
    }
}

// ---------- processes ----------

/// Process table. With `scroll` the list starts at that row and the title
/// shows the position; without it only the top rows are shown.
fn processes(buf: &mut Buffer, area: Rect, app: &App, scroll: Option<usize>, targets: &mut MouseTargets) {
    let s = &app.snap;
    let mut procs: Vec<_> = s.procs.iter().collect();
    match app.sort {
        Sort::Pid => procs.sort_by_key(|p| p.pid),
        Sort::Name => procs.sort_by(|a, b| a.name.cmp(&b.name)),
        Sort::Container => procs.sort_by_key(|p| (p.container.is_none(), &p.container)),
        // Idle processes tie at zero; break ties by memory so the list stays useful.
        Sort::Cpu => procs.sort_by(|a, b| {
            b.cpu.unwrap_or(0.0).total_cmp(&a.cpu.unwrap_or(0.0)).then(b.rss.cmp(&a.rss))
        }),
        Sort::Mem => procs.sort_by_key(|x| std::cmp::Reverse(x.rss)),
        Sort::Share => procs.sort_by_key(|x| std::cmp::Reverse(x.rss + x.gpu_mem.unwrap_or(0))),
        Sort::Gpu => procs.sort_by_key(|x| std::cmp::Reverse((x.gpu_mem.unwrap_or(0), x.rss))),
    }
    let gpu_users = procs.iter().filter(|x| x.gpu_kind.is_some()).count();
    let visible = process_rows(area);
    let first = scroll.unwrap_or(0).min(procs.len().saturating_sub(visible));
    let right = match scroll {
        Some(_) => vec![dim(format!(
            "{}-{} of {} · {gpu_users} gpu users",
            first + 1,
            (first + visible).min(procs.len()),
            procs.len()
        ))],
        None => vec![dim(format!("{gpu_users} gpu users"))],
    };
    let mut p = panel(buf, area, "PROCESSES", SOC, right);
    let w = p.width();
    let total = s.mem.as_ref().map(|m| m.total).unwrap_or(1).max(1) as f64;

    // Fixed columns; the memory meter takes whatever width is left.
    let (name_w, cont_w) = match w {
        120.. => (22, 24),
        68.. => (16, 18),
        _ => (11, 13),
    };
    let bar_w = w.saturating_sub(8 + name_w + cont_w + 8 + 9 + 9 + 4).min(40);
    // Too narrow for a meaningful meter: drop the column.
    let bar_w = if bar_w < 10 { 0 } else { bar_w };
    let mut headers = Vec::new();
    let mut header = |name: &str, key, width: usize, align_left| {
        let selected = app.sort == key;
        let arrow = match (selected, key) {
            (false, _) => " ",
            (true, Sort::Pid | Sort::Name | Sort::Container) => "▲",
            (true, _) => "▼",
        };
        let text = format!("{name}{arrow}");
        let face_width = width - 1;
        let face = if align_left {
            format!("{text:<face_width$}")
        } else {
            format!("{text:>face_width$}")
        };
        headers.push((button_face(face, selected), Some(Action::Sort(key))));
        headers.push((Span::raw(" "), None));
    };
    header("PID", Sort::Pid, 8, false);
    header("NAME", Sort::Name, name_w, true);
    header("CONTAINER", Sort::Container, cont_w, true);
    header("CPU%", Sort::Cpu, 8, false);
    header("MEM", Sort::Mem, 9, false);
    header("GPU", Sort::Gpu, 9, false);
    if bar_w > 0 {
        header(if bar_w >= 18 { "MEM + GPU SHARE" } else { "SHARE" }, Sort::Share, bar_w, true);
    }
    controls(p.buf, p.area, targets, headers);
    let clip = |t: &str, n: usize| -> String { t.chars().take(n.saturating_sub(1)).collect() };
    for (i, x) in procs.iter().skip(first).take(visible).enumerate() {
        let cpu = x.cpu.unwrap_or(0.0);
        // 100% is one core; colour ramps up to four busy cores.
        let cpu_c = if cpu < 1.0 { DIM } else { heat(cpu / 400.0) };
        let mut l = vec![
            dim(format!("{:>7} ", x.pid)),
            span(
                format!("{:<name_w$}", clip(&x.name, name_w)),
                if x.gpu_kind.is_some() { GPU } else { FG },
            ),
            span(
                format!("{:<cont_w$}", clip(x.container.as_deref().unwrap_or("·"), cont_w)),
                if x.container.is_some() { SOC } else { FAINT },
            ),
            span(format!("{cpu:>7.1} "), cpu_c),
            span(format!("{:>8} ", size(x.rss)), FG),
            match x.gpu_mem {
                Some(m) => span(format!("{:>8} ", size(m)), GPU),
                None => span(format!("{:>8} ", "·"), FAINT),
            },
            Span::raw("  "),
        ];
        // GPU allocations are not part of resident memory, so add them for
        // the process's real share of the unified pool.
        let footprint = (x.rss + x.gpu_mem.unwrap_or(0)) as f64;
        l.extend(meter(bar_w, footprint / total, &Grad::mono(APPS)));
        p.line(1 + i as u16, l);
    }
}

fn process_rows(area: Rect) -> usize {
    (area.height as usize).saturating_sub(3)
}

/// A filled surface for clickable labels. Padding belongs to the hit area.
fn button_face(text: impl Into<String>, selected: bool) -> Span<'static> {
    if selected {
        bold(text, (16, 18, 28)).bg(color(ACCENT))
    } else {
        bold(text, FG).bg(color(TRACK))
    }
}

fn controls(
    buf: &mut Buffer,
    area: Rect,
    targets: &mut MouseTargets,
    items: impl IntoIterator<Item = (Span<'static>, Option<Action>)>,
) -> u16 {
    if area.is_empty() {
        return area.x;
    }
    let mut x = area.x;
    let mut previous_button = false;
    for (span, action) in items {
        if previous_button && action.is_some() {
            x = x.saturating_add(1);
        }
        previous_button = action.is_some();
        let width = (span.width() as u16).min(area.right().saturating_sub(x));
        if width == 0 {
            break;
        }
        if let Some(action) = action {
            targets.buttons.push((Rect::new(x, area.y, width, 1), action));
        }
        buf.set_span(x, area.y, &span, width);
        x += width;
    }
    x
}

fn footer(buf: &mut Buffer, area: Rect, app: &App, targets: &mut MouseTargets) {
    let label = |key: &str, name: &str| {
        if area.width >= 90 && key != name {
            format!(" {key} {name} ")
        } else {
            format!(" {name} ")
        }
    };
    let button = |key, name, action| (button_face(label(key, name), false), Some(action));
    let sort = |shortcut, name, key| {
        (button_face(label(shortcut, name), app.sort == key), Some(Action::Sort(key)))
    };
    let sorts = || [sort("c", "CPU", Sort::Cpu), sort("m", "Mem", Sort::Mem), sort("g", "GPU", Sort::Gpu)];
    let mut global = vec![
        button("q", "Quit", Action::Quit),
        button("Space", if app.paused { "Resume" } else { "Pause" }, Action::Pause),
    ];
    if app.tab == Tab::Overview {
        global.push((dim(if area.width >= 70 { "  Sort " } else { "  " }), None));
        global.extend(sorts());
    }
    global.extend([
        button("-", "Faster", Action::Faster),
        button("+", "Slower", Action::Slower),
        (dim(format!(" {:.2}s", app.interval)), None),
    ]);
    if app.tab == Tab::Procs {
        controls(
            buf,
            Rect::new(area.x, area.y, area.width, 1),
            targets,
            [(dim("Sort "), None)].into_iter().chain(sorts()).chain([
                (dim(" Scroll "), None),
                button("Home", "Top", Action::Top),
                button("PgUp", "PgUp", Action::Scroll(-10)),
                button("↑", "↑", Action::Scroll(-1)),
                button("↓", "↓", Action::Scroll(1)),
                button("PgDn", "PgDn", Action::Scroll(10)),
            ]),
        );
    }
    let y = area.bottom() - 1;
    let end = controls(buf, Rect::new(area.x, y, area.width, 1), targets, global);
    if app.cfg.log.is_some() && app.log_error.is_none() {
        let logging = span(" ● logging", OK);
        let remaining = area.right().saturating_sub(end);
        if logging.width() <= usize::from(remaining) {
            buf.set_span(end, y, &logging, remaining);
        }
    }
}
