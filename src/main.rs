mod alerts;
mod collect;
mod config;
mod csvlog;
mod history;
mod report;
mod sysfs;
mod tui;

#[cfg(test)]
mod fixture_tests;

use anyhow::{bail, Context, Result};
use collect::{Sampler, Snapshot};
use config::Config;
use history::Ring;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use sysfs::Root;

const USAGE: &str = "\
gb10top - system monitor for NVIDIA GB10 machines (ASUS Ascent GX10, DGX Spark)

Usage: gb10top [OPTIONS]

With no options, opens the interactive dashboard.

  --interval <SEC>  Seconds between samples [default: 1.0]
  --log <FILE>      Append one CSV row of headline metrics per sample to FILE
  --config <FILE>   Read this config file instead of ~/.config/gb10top/config.toml
  --print-config    Print the default config file and exit
  -V, --version     Print the gb10top version and exit

Plain output instead of the dashboard:
  --once            Print one human-readable sample and exit
  --json            Print samples as JSON lines instead of text
  --count <N>       Print N samples (0 = until interrupted)

Development:
  --render <WxH>    Draw the dashboard once at the given size and print it as text
  --tab <NAME>      Tab to draw with --render (overview, power, gpu, cpu, thermal,
                    io, procs, events)
  --root <DIR>      Read /proc and /sys from under DIR (recorded fixtures)
  -h, --help        Show this help

Run as root (sudo gb10top) to add NVMe wear and error counters.";

struct Args {
    json: bool,
    /// None means the interactive dashboard.
    count: Option<u64>,
    render: Option<(u16, u16)>,
    tab: tui::Tab,
    interval: Option<f64>,
    log: Option<PathBuf>,
    config: Option<PathBuf>,
    root: String,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        json: false,
        count: None,
        render: None,
        tab: tui::Tab::Overview,
        interval: None,
        log: None,
        config: None,
        root: "/".into(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().with_context(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--once" => a.count = Some(1),
            "--json" => a.json = true,
            "--count" => a.count = Some(value("--count")?.parse().context("--count")?),
            "--render" => {
                let v = value("--render")?;
                let (w, h) = v.split_once('x').context("--render expects WxH, e.g. 140x45")?;
                a.render =
                    Some((w.parse().context("--render width")?, h.parse().context("--render height")?));
            }
            "--tab" => {
                let v = value("--tab")?;
                a.tab = tui::Tab::parse(&v).with_context(|| format!("unknown tab {v:?}"))?;
            }
            "--interval" => a.interval = Some(value("--interval")?.parse().context("--interval")?),
            "--log" => a.log = Some(value("--log")?.into()),
            "--config" => a.config = Some(value("--config")?.into()),
            "--print-config" => {
                print!("{}", config::TEMPLATE);
                std::process::exit(0);
            }
            "--root" => a.root = value("--root")?,
            "-V" | "--version" => {
                println!("gb10top {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("unknown argument {other:?}\n\n{USAGE}"),
        }
    }
    if a.interval.is_some_and(|i| !(i > 0.0)) {
        bail!("--interval must be positive");
    }
    Ok(a)
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let mut cfg = config::load(args.config.as_deref())?;
    if let Some(i) = args.interval {
        cfg.interval = i;
    }
    if args.log.is_some() {
        cfg.log = args.log.clone();
    }
    let root = Root::new(&args.root);
    let sampler = Sampler::new(&root);

    if let Some(size) = args.render {
        let samples = args.count.unwrap_or(3);
        print!("{}", tui::render_text(sampler, &root, cfg, samples, args.tab, size)?);
        return Ok(());
    }
    match args.count {
        Some(count) => print_samples(sampler, &args, &cfg, count),
        None if args.json => print_samples(sampler, &args, &cfg, 1),
        None if !std::io::stdout().is_terminal() => {
            bail!("the dashboard needs a terminal; use --once or --json for plain output")
        }
        None => tui::run(sampler, &root, cfg),
    }
}

/// Text or JSON-lines output, `count` samples (0 = forever).
fn print_samples(mut sampler: Sampler, args: &Args, cfg: &Config, count: u64) -> Result<()> {
    let interval = Duration::from_secs_f64(cfg.interval);
    let mut log = cfg.log.as_deref().map(csvlog::CsvLog::open).transpose()?;
    let mut history: Ring<Snapshot> = Ring::new(3600);
    let mut taken = 0u64;
    let mut next = Instant::now() + interval;
    while count == 0 || taken < count {
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        next += interval;
        let snap = sampler.sample();
        if args.json {
            println!("{}", serde_json::to_string(&snap)?);
        } else {
            if taken > 0 {
                println!();
            }
            print!("{}", report::text(&snap));
        }
        if let Some(log) = &mut log {
            log.write(&snap)?;
        }
        history.push(snap);
        taken += 1;
    }

    if history.len() > 1 {
        eprintln!("{} samples from collectors: {}", history.len(), sampler.names().join(", "));
        let sys = history
            .iter()
            .filter_map(|s| s.spbm.as_ref().and_then(|p| p.rail("sys_total")));
        if let Some((lo, mean, hi)) = history::stats(sys) {
            eprintln!("sys_total: min {lo:.1} W, mean {mean:.1} W, max {hi:.1} W");
        }
    }
    Ok(())
}
