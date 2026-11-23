//! Alert rules and the log of alerts being raised and cleared.

use crate::collect::Snapshot;
use crate::config::Thresholds;
use crate::history::Ring;

/// An alert stays active this long after its condition was last seen, so a
/// condition that flickers (the package power cap does) is one event.
const HOLD_SECS: f64 = 5.0;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Warn,
    Crit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    /// Stable identity of the rule.
    pub key: &'static str,
    pub level: Level,
    /// Short label for the header.
    pub badge: &'static str,
    pub text: String,
}

fn alert(key: &'static str, level: Level, badge: &'static str, text: String) -> Alert {
    Alert { key, level, badge, text }
}

/// Hottest junction temperature: the power driver's figure, or the hottest thermal zone.
pub fn tj_max(s: &Snapshot) -> Option<f64> {
    s.spbm.as_ref().and_then(|p| p.tj_max_c).or_else(|| {
        s.thermal
            .as_ref()
            .and_then(|t| t.zones.iter().map(|z| z.celsius).reduce(f64::max))
    })
}

/// Every alert condition that holds in this snapshot.
pub fn evaluate(s: &Snapshot, t: &Thresholds) -> Vec<Alert> {
    use Level::*;
    let mut out = Vec::new();

    if let Some(p) = &s.spbm {
        if p.pl_level.is_some_and(|l| l >= 2) {
            let cap = p.limits.iter().find(|l| l.label == "pl2").and_then(|l| l.cap_w);
            let text = match cap {
                Some(c) => format!("package power is being held at its {c:.0} W limit"),
                None => "package power is being held at its limit".into(),
            };
            out.push(alert("power_cap", Warn, "POWER CAP", text));
        }
        // 1 is the value seen at rest and under every load tested.
        if let Some(v) = p.prochot.filter(|v| *v != 1) {
            out.push(alert(
                "prochot",
                Warn,
                "PROCHOT",
                format!("prochot signal changed to {v} (resting value is 1)"),
            ));
        }
    }
    if let Some(g) = s.gpu.as_ref().filter(|g| !g.throttle.is_empty()) {
        out.push(alert(
            "gpu_throttle",
            Crit,
            "GPU THROTTLE",
            format!("GPU clocks reduced: {}", g.throttle.join(", ")),
        ));
    }
    if let Some(th) = &s.thermal {
        if th.cpu_cooling_max_state > 0 {
            out.push(alert(
                "cpu_throttle",
                Crit,
                "CPU THROTTLE",
                format!("CPU thermal throttling at state {}", th.cpu_cooling_max_state),
            ));
        }
        let disk = th.sensors.iter().filter(|x| x.chip == "nvme").map(|x| x.celsius).reduce(f64::max);
        if let Some(c) = disk.filter(|c| *c >= t.disk_temp_warn_c) {
            out.push(alert("disk_temp", Warn, "DISK HOT", format!("NVMe drive at {c:.0} °C")));
        }
    }
    if let Some(tj) = tj_max(s) {
        if tj >= t.tj_crit_c {
            out.push(alert("tj", Crit, "HOT", format!("junction temperature {tj:.0} °C")));
        } else if tj >= t.tj_warn_c {
            out.push(alert("tj", Warn, "WARM", format!("junction temperature {tj:.0} °C")));
        }
    }
    if let Some(m) = s.mem.as_ref().filter(|m| m.total > 0) {
        let avail = 100.0 * m.available as f64 / m.total as f64;
        let text = format!("only {avail:.1}% of memory available");
        if avail <= t.mem_available_crit_pct {
            out.push(alert("mem_low", Crit, "MEM LOW", text));
        } else if avail <= t.mem_available_warn_pct {
            out.push(alert("mem_low", Warn, "MEM LOW", text));
        }
        if m.swap_total > 0 {
            let used = 100.0 * (m.swap_total - m.swap_free) as f64 / m.swap_total as f64;
            if used >= t.swap_used_warn_pct {
                out.push(alert("swap", Warn, "SWAPPING", format!("{used:.0}% of swap in use")));
            }
        }
    }
    if let Some(pr) = s.psi.as_ref().and_then(|p| p.memory.as_ref()) {
        if pr.some10 >= t.mem_pressure_warn_pct {
            out.push(alert(
                "mem_pressure",
                Warn,
                "MEM PRESSURE",
                format!("tasks stalled on memory {:.1}% of the last 10 s", pr.some10),
            ));
        }
    }
    for d in &s.smart {
        if d.critical_warning != 0 {
            out.push(alert(
                "disk_health",
                Crit,
                "DISK HEALTH",
                format!("{} reports SMART critical warning {:#x}", d.dev, d.critical_warning),
            ));
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Unix time.
    pub at: f64,
    pub raised: bool,
    pub level: Level,
    pub text: String,
}

struct Active {
    alert: Alert,
    last_seen: f64,
}

pub struct AlertLog {
    active: Vec<Active>,
    pub events: Ring<Event>,
}

impl AlertLog {
    pub fn new() -> Self {
        AlertLog { active: Vec::new(), events: Ring::new(500) }
    }

    pub fn active(&self) -> impl Iterator<Item = &Alert> {
        self.active.iter().map(|a| &a.alert)
    }

    /// Fold in the alerts of one sample taken at unix time `now`. Returns
    /// true when a critical alert was newly raised.
    pub fn update(&mut self, now: f64, current: Vec<Alert>) -> bool {
        let mut new_crit = false;
        for a in current {
            match self.active.iter_mut().find(|x| x.alert.key == a.key) {
                Some(x) => {
                    // An escalation is worth its own event; a changed reading is not.
                    if a.level > x.alert.level {
                        self.events.push(Event { at: now, raised: true, level: a.level, text: a.text.clone() });
                        new_crit |= a.level == Level::Crit;
                    }
                    x.alert = a;
                    x.last_seen = now;
                }
                None => {
                    self.events.push(Event { at: now, raised: true, level: a.level, text: a.text.clone() });
                    new_crit |= a.level == Level::Crit;
                    self.active.push(Active { alert: a, last_seen: now });
                }
            }
        }
        let (keep, cleared): (Vec<_>, Vec<_>) =
            self.active.drain(..).partition(|x| now - x.last_seen < HOLD_SECS);
        self.active = keep;
        for x in cleared {
            self.events.push(Event { at: now, raised: false, level: x.alert.level, text: x.alert.text });
        }
        self.active.sort_by(|a, b| b.alert.level.cmp(&a.alert.level));
        new_crit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::mem::Mem;
    use crate::collect::spbm::Spbm;

    fn snap_with_tj(tj: f64) -> Snapshot {
        Snapshot { spbm: Some(Spbm { tj_max_c: Some(tj), prochot: Some(1), ..Default::default() }), ..Default::default() }
    }

    #[test]
    fn temperature_levels() {
        let t = Thresholds::default();
        assert!(evaluate(&snap_with_tj(60.0), &t).is_empty());
        let warm = evaluate(&snap_with_tj(88.0), &t);
        assert_eq!((warm[0].key, warm[0].level, warm[0].badge), ("tj", Level::Warn, "WARM"));
        let hot = evaluate(&snap_with_tj(96.0), &t);
        assert_eq!((hot[0].level, hot[0].badge), (Level::Crit, "HOT"));
    }

    #[test]
    fn memory_and_power_rules() {
        let t = Thresholds::default();
        let mut s = Snapshot::default();
        s.mem = Some(Mem { total: 1000, available: 80, swap_total: 100, swap_free: 40, ..Default::default() });
        s.spbm = Some(Spbm { pl_level: Some(2), prochot: Some(0), ..Default::default() });
        let keys: Vec<_> = evaluate(&s, &t).iter().map(|a| (a.key, a.level)).collect();
        assert_eq!(
            keys,
            vec![
                ("power_cap", Level::Warn),
                ("prochot", Level::Warn),
                ("mem_low", Level::Warn),
                ("swap", Level::Warn)
            ]
        );
        s.mem.as_mut().unwrap().available = 30;
        assert!(evaluate(&s, &t).iter().any(|a| a.key == "mem_low" && a.level == Level::Crit));
    }

    #[test]
    fn log_holds_flickering_alerts_and_records_escalation() {
        let t = Thresholds::default();
        let mut log = AlertLog::new();
        assert!(!log.update(0.0, evaluate(&snap_with_tj(88.0), &t)));
        assert_eq!(log.events.len(), 1);
        // Condition gone for 2 s: still active, no new event.
        log.update(2.0, vec![]);
        assert_eq!((log.active().count(), log.events.len()), (1, 1));
        // Back and now critical: one escalation event, reported as new critical.
        assert!(log.update(3.0, evaluate(&snap_with_tj(97.0), &t)));
        assert_eq!(log.events.len(), 2);
        // Gone for longer than the hold: cleared.
        log.update(9.0, vec![]);
        assert_eq!(log.active().count(), 0);
        let last = log.events.tail(1).pop().unwrap();
        assert!(!last.raised);
    }
}
