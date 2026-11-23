//! Fixed-capacity ring buffer for metric history.

use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct Ring<T> {
    buf: VecDeque<T>,
    cap: usize,
}

impl<T> Ring<T> {
    pub fn new(cap: usize) -> Self {
        assert!(cap > 0);
        Ring { buf: VecDeque::with_capacity(cap), cap }
    }

    pub fn push(&mut self, v: T) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back(v);
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// The newest `n` values, oldest first.
    pub fn tail(&self, n: usize) -> Vec<T>
    where
        T: Clone,
    {
        self.buf.iter().skip(self.buf.len().saturating_sub(n)).cloned().collect()
    }

    /// Oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.buf.iter()
    }
}

/// min / mean / max over an iterator of values; None when empty.
pub fn stats(vals: impl Iterator<Item = f64>) -> Option<(f64, f64, f64)> {
    let (mut n, mut sum, mut lo, mut hi) = (0usize, 0.0, f64::INFINITY, f64::NEG_INFINITY);
    for v in vals {
        n += 1;
        sum += v;
        lo = lo.min(v);
        hi = hi.max(v);
    }
    (n > 0).then(|| (lo, sum / n as f64, hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_oldest() {
        let mut r = Ring::new(3);
        for i in 0..5 {
            r.push(i);
        }
        assert_eq!(r.len(), 3);
        assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![2, 3, 4]);
    }

    #[test]
    fn stats_basic() {
        assert_eq!(stats([1.0, 2.0, 3.0].into_iter()), Some((1.0, 2.0, 3.0)));
        assert_eq!(stats(std::iter::empty()), None);
    }
}
