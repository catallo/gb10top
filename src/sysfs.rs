//! Small helpers for reading /proc and /sys, rooted so tests can use fixtures.

use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Filesystem root that all absolute /proc and /sys paths are resolved against.
#[derive(Clone, Debug)]
pub struct Root(PathBuf);

impl Root {
    pub fn new(p: impl Into<PathBuf>) -> Self {
        Root(p.into())
    }

    pub fn is_live(&self) -> bool {
        self.0 == Path::new("/")
    }

    pub fn path(&self, abs: &str) -> PathBuf {
        self.0.join(abs.trim_start_matches('/'))
    }
}

pub fn read_str(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

pub fn read_num<T: FromStr>(p: impl AsRef<Path>) -> Option<T> {
    read_str(p)?.parse().ok()
}

/// Directory entries sorted by name with numeric suffixes in natural order
/// (hwmon2 before hwmon12).
pub fn list(dir: impl AsRef<Path>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => return Vec::new(),
    };
    v.sort_by_key(|p| natural_key(&file_name(p)));
    v
}

pub fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn natural_key(name: &str) -> (String, u64) {
    let split = name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    (name[..split].to_string(), name[split..].parse().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        assert!(natural_key("hwmon2") < natural_key("hwmon12"));
        assert!(natural_key("cpu9") < natural_key("cpu10"));
    }

    #[test]
    fn root_join() {
        let r = Root::new("/tmp/fix");
        assert_eq!(r.path("/sys/class"), PathBuf::from("/tmp/fix/sys/class"));
        assert!(Root::new("/").is_live());
    }
}
