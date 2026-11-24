//! Interactive dashboard.

mod detail;
mod panels;
mod theme;
mod widgets;

pub use crate::alerts::tj_max;
use crate::alerts::{self, AlertLog};
use crate::collect::io::Net;
use crate::collect::{Sampler, Snapshot};
use crate::config::Config;
use crate::csvlog::CsvLog;
use crate::history::{self, Ring};
use crate::sysfs::{read_str, Root};
use anyhow::Result;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Position, Rect};
use ratatui::Terminal;
use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

/// Named history series, created on first push.
pub struct Series {
    map: HashMap<String, Ring<f64>>,
    cap: usize,
}

impl Series {
    pub fn new(cap: usize) -> Self {
        Series { map: HashMap::new(), cap }
    }

    pub fn push(&mut self, key: &str, v: f64) {
        match self.map.get_mut(key) {
            Some(r) => r.push(v),
            None => {
                let mut r = Ring::new(self.cap);
                r.push(v);
                self.map.insert(key.to_string(), r);
            }
        }
    }

    /// The newest `n` values, oldest first; empty for an unknown series.
    pub fn tail(&self, key: &str, n: usize) -> Vec<f64> {
        self.map.get(key).map(|r| r.tail(n)).unwrap_or_default()
    }

    /// (min, mean, max) over the whole kept history.
    pub fn stats(&self, key: &str) -> Option<(f64, f64, f64)> {
        history::stats(self.map.get(key)?.iter().copied())
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tab {
    Overview,
    Power,
    Gpu,
    Cpu,
    Thermal,
    Io,
    Procs,
    Events,
}

impl Tab {
    pub const ALL: [Tab; 8] = [
        Tab::Overview,
        Tab::Power,
        Tab::Gpu,
        Tab::Cpu,
        Tab::Thermal,
        Tab::Io,
        Tab::Procs,
        Tab::Events,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Power => "Power",
            Tab::Gpu => "GPU",
            Tab::Cpu => "CPU",
            Tab::Thermal => "Thermal",
            Tab::Io => "I/O",
            Tab::Procs => "Procs",
            Tab::Events => "Events",
        }
    }

    /// Look a tab up by (case-insensitive) title, for `--tab`.
    pub fn parse(name: &str) -> Option<Tab> {
        let name = name.to_lowercase().replace('/', "");
        Tab::ALL.into_iter().find(|t| t.title().to_lowercase().replace('/', "") == name)
    }

    fn step(self, by: isize) -> Tab {
        let n = Tab::ALL.len() as isize;
        let i = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0) as isize;
        Tab::ALL[((i + by).rem_euclid(n)) as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sort {
    Pid,
    Name,
    Container,
    Share,
    Cpu,
    Mem,
    Gpu,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    Quit,
    Tab(Tab),
    Sort(Sort),
    Pause,
    Faster,
    Slower,
    Scroll(isize),
    Top,
}

#[derive(Default)]
struct MouseTargets {
    buttons: Vec<(Rect, Action)>,
    processes: Option<Rect>,
    scroll_end: Option<usize>,
}

impl MouseTargets {
    fn action(&self, mouse: MouseEvent) -> Option<Action> {
        let position = Position::new(mouse.column, mouse.row);
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            return self.buttons.iter()
                .find(|(area, _)| area.contains(position))
                .map(|(_, action)| *action);
        }
        if self.processes.is_some_and(|area| area.contains(position)) {
            match mouse.kind {
                MouseEventKind::ScrollUp => return Some(Action::Scroll(-3)),
                MouseEventKind::ScrollDown => return Some(Action::Scroll(3)),
                _ => {}
            }
        }
        None
    }
}

pub struct App {
    sampler: Sampler,
    root: Root,
    pub cfg: Config,
    pub snap: Snapshot,
    pub series: Series,
    pub alerts: AlertLog,
    pub tab: Tab,
    pub sort: Sort,
    /// First visible row of the process list on the Procs tab.
    pub scroll: usize,
    pub interval: f64,
    pub paused: bool,
    pub host: String,
    pub model: String,
    pub uptime_s: f64,
    /// Energy drawn by the whole system since the app started.
    pub wh: f64,
    pub peak_w: f64,
    pub elapsed_s: f64,
    log: Option<CsvLog>,
    /// Set when logging stopped because a write failed.
    pub log_error: Option<String>,
}

/// GPU power: the board's GPU rail when available (faster and more complete), else NVML.
pub fn gpu_watts(s: &Snapshot) -> Option<f64> {
    s.spbm
        .as_ref()
        .and_then(|p| p.rail("gpu"))
        .or_else(|| s.gpu.as_ref().and_then(|g| g.power_w))
}

/// The interface worth graphing: a physical one that is up, else any physical one.
pub fn primary_net(s: &Snapshot) -> Option<&Net> {
    s.nets
        .iter()
        .find(|n| n.physical && n.state == "up")
        .or_else(|| s.nets.iter().find(|n| n.physical))
}

fn sensor_name(label: &str) -> String {
    match label {
        "cpu_p_clu0" => "P clu0".into(),
        "cpu_p_clu1" => "P clu1".into(),
        "cpu_e_clu0" => "E clu0".into(),
        "cpu_e_clu1" => "E clu1".into(),
        "gpu" => "GPU".into(),
        "soc" => "SoC".into(),
        "nvme" => "NVMe".into(),
        l if l.starts_with("mt79") => "Wi-Fi".into(),
        l => l.chars().take(7).collect(),
    }
}

/// Every distinct temperature sensor as (display name, Celsius): the thermal
/// zones (minus the one that repeats Tj max), then one reading per other chip.
pub fn sensors(s: &Snapshot) -> Vec<(String, f64)> {
    let Some(t) = &s.thermal else { return Vec::new() };
    let mut out: Vec<(String, f64)> = t
        .zones
        .iter()
        .filter(|z| z.label.as_deref() != Some("tj_max"))
        .map(|z| (sensor_name(z.label.as_deref().unwrap_or(&z.name)), z.celsius))
        .collect();
    let mut seen_chip = "";
    for x in &t.sensors {
        if x.chip != seen_chip {
            out.push((sensor_name(&x.chip), x.celsius));
            seen_chip = &x.chip;
        }
    }
    out
}

/// Local wall-clock time of a unix timestamp as HH:MM:SS.
pub fn clock(unix: f64) -> String {
    let t = unix as libc::time_t;
    // SAFETY: localtime_r writes only into the zeroed struct we pass.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

impl App {
    pub fn new(sampler: Sampler, root: &Root, cfg: Config) -> Self {
        let dmi = |f: &str| read_str(root.path(&format!("/sys/class/dmi/id/{f}"))).unwrap_or_default();
        let model = match (dmi("sys_vendor").as_str(), dmi("product_name").as_str()) {
            ("", "") => "unknown system".to_string(),
            (v, p) if v.starts_with("ASUS") => format!("ASUS {p}"),
            (v, p) => format!("{v} {p}").trim().to_string(),
        };
        let (log, log_error) = match cfg.log.as_deref().map(CsvLog::open) {
            Some(Ok(l)) => (Some(l), None),
            Some(Err(e)) => (None, Some(format!("{e:#}"))),
            None => (None, None),
        };
        App {
            sampler,
            root: root.clone(),
            snap: Snapshot::default(),
            series: Series::new(cfg.history),
            alerts: AlertLog::new(),
            tab: Tab::Overview,
            sort: Sort::Cpu,
            scroll: 0,
            interval: cfg.interval,
            paused: false,
            host: read_str(root.path("/proc/sys/kernel/hostname")).unwrap_or_default(),
            model,
            uptime_s: 0.0,
            wh: 0.0,
            peak_w: 0.0,
            elapsed_s: 0.0,
            log,
            log_error,
            cfg,
        }
    }

    /// Take a sample and fold it into the app state.
    /// Returns true when a critical alert was newly raised.
    pub fn tick(&mut self) -> bool {
        let s = self.sampler.sample();
        self.ingest(s)
    }

    /// Fold one snapshot into history, alerts and the log. Returns true when
    /// a critical alert was newly raised.
    pub fn ingest(&mut self, s: Snapshot) -> bool {
        let h = &mut self.series;
        if let Some(p) = &s.spbm {
            for r in &p.rails {
                h.push(&format!("rail/{}", r.label), r.watts);
            }
            for l in &p.limits {
                h.push(&format!("limit/{}", l.label), l.watts);
            }
            if let Some(w) = p.rail("sys_total") {
                h.push("sys_w", w);
                self.wh += w * s.dt / 3600.0;
                self.peak_w = self.peak_w.max(w);
            }
        }
        self.elapsed_s += s.dt;
        if let Some(w) = gpu_watts(&s) {
            h.push("gpu_w", w);
        }
        if let Some(g) = &s.gpu {
            h.push("gpu_util", g.util.unwrap_or(0) as f64);
            let mut opt = |key: &str, v: Option<f64>| {
                if let Some(v) = v {
                    h.push(key, v);
                }
            };
            opt("gpu_clock", g.clock_mhz.map(f64::from));
            opt("gpu_temp", g.temp_c.map(f64::from));
            opt("gpu_nvml_w", g.power_w);
        }
        if let Some(c) = &s.cpu {
            h.push("cpu", c.util.unwrap_or(0.0));
            h.push("cpu_p", c.p_util.unwrap_or(0.0));
            h.push("cpu_e", c.e_util.unwrap_or(0.0));
            for core in &c.cores {
                h.push(&format!("core/{}", core.id), core.util.unwrap_or(0.0));
            }
        }
        if let Some(t) = tj_max(&s) {
            h.push("tj", t);
        }
        for (name, c) in sensors(&s) {
            h.push(&format!("temp/{name}"), c);
        }
        if let Some(m) = &s.mem {
            h.push("mem", 100.0 * m.used() as f64 / m.total.max(1) as f64);
        }
        let sum = |f: fn(&crate::collect::io::Disk) -> Option<f64>| -> f64 {
            s.disks.iter().filter_map(f).sum()
        };
        let (r, w) = (sum(|d| d.read_bps), sum(|d| d.write_bps));
        h.push("disk_r", r);
        h.push("disk_w", w);
        h.push("disk", r + w);
        if let Some(n) = primary_net(&s) {
            h.push("net_rx", n.rx_bps.unwrap_or(0.0));
            h.push("net_tx", n.tx_bps.unwrap_or(0.0));
        }
        self.uptime_s = read_str(self.root.path("/proc/uptime"))
            .and_then(|u| u.split_ascii_whitespace().next()?.parse().ok())
            .unwrap_or(0.0);

        let new_crit = self.alerts.update(s.t, alerts::evaluate(&s, &self.cfg.alerts));
        if let Some(log) = &mut self.log {
            if let Err(e) = log.write(&s) {
                self.log_error = Some(format!("{e:#}"));
                self.log = None;
            }
        }
        self.snap = s;
        new_crit
    }

    fn key_action(&self, code: KeyCode, mods: KeyModifiers) -> Option<Action> {
        Some(match code {
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => Action::Quit,
            KeyCode::Char(d @ '1'..='8') => Action::Tab(Tab::ALL[d as usize - '1' as usize]),
            KeyCode::Tab | KeyCode::Right => Action::Tab(self.tab.step(1)),
            KeyCode::BackTab | KeyCode::Left => Action::Tab(self.tab.step(-1)),
            KeyCode::Char('i') => Action::Sort(Sort::Pid),
            KeyCode::Char('n') => Action::Sort(Sort::Name),
            KeyCode::Char('o') => Action::Sort(Sort::Container),
            KeyCode::Char('s') => Action::Sort(Sort::Share),
            KeyCode::Char('c') => Action::Sort(Sort::Cpu),
            KeyCode::Char('m') => Action::Sort(Sort::Mem),
            KeyCode::Char('g') => Action::Sort(Sort::Gpu),
            KeyCode::Char(' ') | KeyCode::Char('p') => Action::Pause,
            KeyCode::Char('+') | KeyCode::Char('=') => Action::Slower,
            KeyCode::Char('-') => Action::Faster,
            KeyCode::Down | KeyCode::Char('j') => Action::Scroll(1),
            KeyCode::Up | KeyCode::Char('k') => Action::Scroll(-1),
            KeyCode::PageDown => Action::Scroll(10),
            KeyCode::PageUp => Action::Scroll(-10),
            KeyCode::Home => Action::Top,
            _ => return None,
        })
    }

    /// Returns false when the app should quit.
    fn apply(&mut self, action: Action, scroll_end: Option<usize>) -> bool {
        match action {
            Action::Quit => return false,
            Action::Tab(tab) => self.tab = tab,
            Action::Sort(sort) => self.sort = sort,
            Action::Pause => self.paused = !self.paused,
            Action::Slower => self.interval = (self.interval * 2.0).min(8.0),
            Action::Faster => self.interval = (self.interval / 2.0).max(0.25),
            Action::Scroll(rows) => {
                let end = scroll_end.unwrap_or_else(|| self.snap.procs.len().saturating_sub(1));
                self.scroll = self.scroll.min(end).saturating_add_signed(rows).min(end);
            }
            Action::Top => self.scroll = 0,
        }
        true
    }
}

struct MouseCapture;

impl Drop for MouseCapture {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
    }
}

/// Run the dashboard until the user quits.
pub fn run(sampler: Sampler, root: &Root, cfg: Config) -> Result<()> {
    let mut app = App::new(sampler, root, cfg);
    let mut term = ratatui::init();
    let result = (|| -> Result<()> {
        let _capture = MouseCapture;
        execute!(std::io::stdout(), EnableMouseCapture)?;
        let mut next = Instant::now();
        loop {
            if Instant::now() >= next {
                if !app.paused && app.tick() && app.cfg.bell {
                    bell();
                }
                next = Instant::now() + Duration::from_secs_f64(app.interval);
            }
            let mut targets = MouseTargets::default();
            term.draw(|f| {
                let area = f.area();
                targets = panels::draw(f.buffer_mut(), area, &app);
            })?;
            // Mouse capture also reports pointer motion. Wait for an action,
            // a resize or the sample deadline before drawing another frame.
            loop {
                if !event::poll(next.saturating_duration_since(Instant::now()))? {
                    break;
                }
                let action = match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        app.key_action(k.code, k.modifiers)
                    }
                    Event::Mouse(mouse) => targets.action(mouse),
                    Event::Resize(_, _) => break,
                    _ => None,
                };
                if let Some(action) = action {
                    if !app.apply(action, targets.scroll_end) {
                        return Ok(());
                    }
                    break;
                }
                if Instant::now() >= next {
                    break;
                }
            }
        }
    })();
    ratatui::restore();
    result
}

/// Draw the app's current state off-screen and return it as plain text.
pub fn to_text(app: &App, w: u16, h: u16) -> Result<String> {
    let mut term = Terminal::new(TestBackend::new(w, h))?;
    term.draw(|f| {
        let area = f.area();
        panels::draw(f.buffer_mut(), area, app);
    })?;
    let buf = term.backend().buffer();
    let mut out = String::new();
    for y in 0..h {
        for x in 0..w {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    Ok(out)
}

/// Render one tab after `samples` samples. Used for checking layouts
/// without a terminal.
pub fn render_text(
    sampler: Sampler,
    root: &Root,
    cfg: Config,
    samples: u64,
    tab: Tab,
    (w, h): (u16, u16),
) -> Result<String> {
    let mut app = App::new(sampler, root, cfg);
    app.tab = tab;
    for _ in 0..samples.max(1) {
        std::thread::sleep(Duration::from_secs_f64(app.interval));
        app.tick();
    }
    to_text(&app, w, h)
}

/// Ring the terminal bell.
fn bell() {
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    #[test]
    fn every_mouse_control_fits_at_the_minimum_size() {
        let root = Root::new("tests/fixtures/gx10");
        let mut app = App::new(Sampler::new(&root), &root, Config::default());
        let area = Rect::new(0, 0, 60, 14);
        let mut buf = Buffer::empty(area);
        let mouse = |kind, column, row| MouseEvent {
            kind, column, row, modifiers: KeyModifiers::NONE,
        };
        let assert_click = |buf: &Buffer, targets: &MouseTargets, label: &str, action: Action| {
            assert!(targets.buttons.iter().any(|(rect, _)| {
                let rendered: String = (rect.x..rect.right())
                    .map(|x| buf[(x, rect.y)].symbol())
                    .collect();
                rendered.trim() == label
                    && targets.action(mouse(MouseEventKind::Down(MouseButton::Left), rect.x, rect.y))
                        == Some(action)
            }), "missing button {label}");
        };

        for tab in Tab::ALL {
            app.tab = tab;
            let targets = panels::draw(&mut buf, area, &app);
            let click = |label, action| assert_click(&buf, &targets, label, action);
            for target in Tab::ALL {
                assert!(targets.buttons.iter().any(|(rect, _)| {
                    targets.action(mouse(MouseEventKind::Down(MouseButton::Left), rect.x, rect.y))
                        == Some(Action::Tab(target))
                }));
            }
            for (label, action) in [
                ("Quit", Action::Quit),
                ("Pause", Action::Pause),
                ("Faster", Action::Faster),
                ("Slower", Action::Slower),
            ] {
                click(label, action);
            }
            if matches!(tab, Tab::Overview | Tab::Procs) {
                // Inspect the table row itself, since the footer repeats some labels.
                let (y, headers) = (2..area.height - 2).map(|y| {
                    let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
                    (y, row)
                }).find(|(_, row)| row.contains("CONTAINER")).unwrap();
                for (label, key) in [
                    ("PID", Sort::Pid), ("NAME", Sort::Name), ("CONTAINER", Sort::Container),
                    ("CPU%▼", Sort::Cpu), ("MEM", Sort::Mem), ("GPU", Sort::Gpu),
                ] {
                    let start = headers.find(label).unwrap_or_else(|| panic!("missing header {label}"));
                    let x = headers[..start].chars().count() as u16;
                    assert_eq!(targets.action(mouse(MouseEventKind::Down(MouseButton::Left), x, y)),
                        Some(Action::Sort(key)));
                }
                click("CPU", Action::Sort(Sort::Cpu));
                click("Mem", Action::Sort(Sort::Mem));
                click("GPU", Action::Sort(Sort::Gpu));
            }
            if tab == Tab::Procs {
                click("Top", Action::Top);
                click("PgUp", Action::Scroll(-10));
                click("↑", Action::Scroll(-1));
                click("↓", Action::Scroll(1));
                click("PgDn", Action::Scroll(10));
                assert_eq!(targets.action(mouse(MouseEventKind::ScrollDown, 4, 4)), Some(Action::Scroll(3)));
                assert_eq!(targets.action(mouse(MouseEventKind::ScrollUp, 4, 4)), Some(Action::Scroll(-3)));
                assert_eq!(targets.action(mouse(MouseEventKind::ScrollDown, 4, 0)), None);
            }
        }
    }
}
