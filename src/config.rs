//! Configuration file: `~/.config/gb10top/config.toml` (or `--config`).
//! Command-line options override it.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Seconds between samples.
    pub interval: f64,
    /// Samples of history kept per graph.
    pub history: usize,
    /// Append one CSV row per sample to this file.
    pub log: Option<PathBuf>,
    /// Ring the terminal bell when a critical alert is raised.
    pub bell: bool,
    pub alerts: Thresholds,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Thresholds {
    pub tj_warn_c: f64,
    pub tj_crit_c: f64,
    pub mem_available_warn_pct: f64,
    pub mem_available_crit_pct: f64,
    pub mem_pressure_warn_pct: f64,
    pub swap_used_warn_pct: f64,
    pub disk_temp_warn_c: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config { interval: 1.0, history: 1200, log: None, bell: false, alerts: Thresholds::default() }
    }
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            tj_warn_c: 85.0,
            tj_crit_c: 95.0,
            mem_available_warn_pct: 10.0,
            mem_available_crit_pct: 4.0,
            mem_pressure_warn_pct: 5.0,
            swap_used_warn_pct: 50.0,
            disk_temp_warn_c: 70.0,
        }
    }
}

/// The default configuration, as a commented file (`--print-config`).
pub const TEMPLATE: &str = r#"# gb10top configuration. Every key is optional; these are the defaults.

# Seconds between samples.
interval = 1.0

# Samples of history kept per graph (1200 = 20 minutes at 1 s).
history = 1200

# Append one CSV row per sample to this file.
# log = "/var/tmp/gb10top.csv"

# Ring the terminal bell when a critical alert is raised.
bell = false

[alerts]
# Hottest junction temperature, in Celsius. The firmware's critical trip is 104.8.
tj_warn_c = 85.0
tj_crit_c = 95.0

# Available memory as a percentage of total. GPU allocations share this pool.
mem_available_warn_pct = 10.0
mem_available_crit_pct = 4.0

# Share of the last 10 s in which tasks stalled waiting for memory.
mem_pressure_warn_pct = 5.0

# Swap in use as a percentage of swap size.
swap_used_warn_pct = 50.0

# NVMe temperature, in Celsius.
disk_temp_warn_c = 70.0
"#;

pub fn default_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("gb10top/config.toml"))
}

pub fn parse(text: &str) -> Result<Config> {
    let cfg: Config = toml::from_str(text)?;
    if !(cfg.interval > 0.0) {
        bail!("interval must be positive");
    }
    if cfg.history < 60 {
        bail!("history must be at least 60 samples");
    }
    let a = &cfg.alerts;
    if a.tj_warn_c > a.tj_crit_c {
        bail!("alerts.tj_warn_c must not exceed alerts.tj_crit_c");
    }
    if a.mem_available_crit_pct > a.mem_available_warn_pct {
        bail!("alerts.mem_available_crit_pct must not exceed alerts.mem_available_warn_pct");
    }
    Ok(cfg)
}

/// Load `explicit` (which must exist), or the default file if present, or
/// the built-in defaults.
pub fn load(explicit: Option<&Path>) -> Result<Config> {
    let path = match explicit {
        Some(p) => p.to_path_buf(),
        None => match default_path() {
            Some(p) if p.exists() => p,
            _ => return Ok(Config::default()),
        },
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading config {}", path.display()))?;
    parse(&text).with_context(|| format!("in config {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_is_the_defaults() {
        assert_eq!(parse(TEMPLATE).unwrap(), Config::default());
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn overrides_and_validation() {
        let c = parse("interval = 0.5\nlog = \"/tmp/x.csv\"\n[alerts]\ntj_warn_c = 80").unwrap();
        assert_eq!(c.interval, 0.5);
        assert_eq!(c.log, Some(PathBuf::from("/tmp/x.csv")));
        assert_eq!(c.alerts.tj_warn_c, 80.0);
        assert_eq!(c.alerts.tj_crit_c, 95.0);
        assert!(parse("intervall = 2").is_err()); // typo is rejected
        assert!(parse("interval = 0").is_err());
        assert!(parse("[alerts]\ntj_warn_c = 99").is_err());
    }
}
