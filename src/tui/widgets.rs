//! Drawing primitives: braille area graphs, gradient meters, stacked bars
//! and bordered panels.

use super::theme::{bold, color, span, Grad, Rgb, FAINT, TRACK};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Widget};

/// Dot bits of a braille cell, indexed [column][row from the top].
const DOTS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// Number of lit dots (from the bottom) for each of the newest samples that
/// fit in `cols` dot columns, right-aligned. Every sample lights at least one
/// dot so the graph keeps a visible baseline.
fn dot_heights(data: &[f64], max: f64, cols: usize, rows: usize) -> Vec<usize> {
    let shown = &data[data.len().saturating_sub(cols)..];
    let mut out = vec![0; cols - shown.len()];
    out.extend(shown.iter().map(|v| {
        let frac = if max > 0.0 && v.is_finite() { (v / max).clamp(0.0, 1.0) } else { 0.0 };
        ((frac * rows as f64).round() as usize).max(1)
    }));
    out
}

/// Filled area graph in braille dots: two samples per cell horizontally, four
/// levels per cell vertically. Newest sample on the right; each row takes its
/// colour from the gradient, bottom to top.
pub fn graph(buf: &mut Buffer, area: Rect, data: &[f64], max: f64, grad: &Grad) {
    let area = area.intersection(buf.area);
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (w, h) = (area.width as usize, area.height as usize);
    let heights = dot_heights(data, max, w * 2, h * 4);
    for cy in 0..h {
        let row_from_bottom = h - 1 - cy;
        let t = if h > 1 { row_from_bottom as f64 / (h - 1) as f64 } else { 0.6 };
        let style = Style::new().fg(color(grad.at(t)));
        for cx in 0..w {
            let mut bits = 0u8;
            for (col, col_bits) in DOTS.iter().enumerate() {
                let lit = heights[cx * 2 + col];
                for (r, bit) in col_bits.iter().enumerate() {
                    // Level of this dot counted from the bottom of the graph.
                    let level = row_from_bottom * 4 + (3 - r);
                    if level < lit {
                        bits |= bit;
                    }
                }
            }
            if bits != 0 {
                let ch = char::from_u32(0x2800 + bits as u32).unwrap_or(' ');
                buf[(area.x + cx as u16, area.y + cy as u16)].set_char(ch).set_style(style);
            }
        }
    }
}

/// Thin horizontal meter. The filled part is coloured along the gradient by
/// position, so a full heat meter runs green to red.
pub fn meter(width: usize, frac: f64, grad: &Grad) -> Vec<Span<'static>> {
    let frac = if frac.is_finite() { frac.clamp(0.0, 1.0) } else { 0.0 };
    let halves = (frac * width as f64 * 2.0).round() as usize;
    (0..width)
        .map(|i| {
            let t = if width > 1 { i as f64 / (width - 1) as f64 } else { 0.0 };
            if halves >= (i + 1) * 2 {
                span("━", grad.at(t))
            } else if halves == i * 2 + 1 {
                span("╸", grad.at(t))
            } else {
                span("━", TRACK)
            }
        })
        .collect()
}

/// Bar split into coloured segments, each sized by its share of `total`.
pub fn stacked(width: usize, segs: &[(f64, Rgb)], total: f64) -> Vec<Span<'static>> {
    (0..width)
        .map(|i| {
            let pos = (i as f64 + 0.5) / width as f64 * total;
            let mut acc = 0.0;
            for (v, c) in segs {
                acc += v.max(0.0);
                if pos < acc {
                    return span("█", *c);
                }
            }
            span("█", TRACK)
        })
        .collect()
}

/// A rectangle inside a buffer with row-oriented helpers that silently clip.
pub struct Pane<'a> {
    pub buf: &'a mut Buffer,
    pub area: Rect,
}

impl Pane<'_> {
    pub fn width(&self) -> usize {
        self.area.width as usize
    }

    /// Write a line of spans at `row`, starting `x` cells in, clipped to the pane.
    pub fn line_at(&mut self, x: u16, row: u16, spans: Vec<Span<'static>>) {
        if row < self.area.height && x < self.area.width {
            self.buf.set_line(
                self.area.x + x,
                self.area.y + row,
                &Line::from(spans),
                self.area.width - x,
            );
        }
    }

    pub fn line(&mut self, row: u16, spans: Vec<Span<'static>>) {
        self.line_at(0, row, spans);
    }

    /// Sub-rectangle in pane coordinates, clipped to the pane.
    pub fn rect(&self, x: u16, row: u16, w: u16, h: u16) -> Rect {
        Rect::new(self.area.x + x, self.area.y + row, w, h).intersection(self.area)
    }

    pub fn graph(&mut self, x: u16, row: u16, w: u16, h: u16, data: &[f64], max: f64, grad: &Grad) {
        let r = self.rect(x, row, w, h);
        graph(self.buf, r, data, max, grad);
    }
}

/// Draw a rounded panel with a coloured title and a right-aligned summary;
/// returns the pane inside the border.
pub fn panel<'a>(
    buf: &'a mut Buffer,
    area: Rect,
    title: &str,
    accent: Rgb,
    right: Vec<Span<'static>>,
) -> Pane<'a> {
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color(FAINT)))
        .title(Line::from(vec![Span::raw(" "), bold(title.to_string(), accent), Span::raw(" ")]));
    if !right.is_empty() {
        let mut spans = vec![Span::raw(" ")];
        spans.extend(right);
        spans.push(Span::raw(" "));
        block = block.title_top(Line::from(spans).right_aligned());
    }
    let inner = block.inner(area);
    block.render(area, buf);
    Pane { buf, area: inner }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heights_right_aligned_with_baseline() {
        // 3 samples into 6 dot columns of an 8-level graph.
        assert_eq!(dot_heights(&[0.0, 50.0, 100.0], 100.0, 6, 8), vec![0, 0, 0, 1, 4, 8]);
        // More samples than columns keeps the newest.
        assert_eq!(dot_heights(&[1.0, 2.0, 3.0, 4.0], 4.0, 2, 4), vec![3, 4]);
        // Out-of-range and bad values are clamped.
        assert_eq!(dot_heights(&[500.0, f64::NAN], 100.0, 2, 4), vec![4, 1]);
    }

    #[test]
    fn graph_draws_full_and_empty_cells() {
        let area = Rect::new(0, 0, 2, 1);
        let mut buf = Buffer::empty(area);
        graph(&mut buf, area, &[100.0, 100.0], 100.0, &Grad::heat());
        assert_eq!(buf[(0, 0)].symbol(), " "); // no data yet on the left
        assert_eq!(buf[(1, 0)].symbol(), "⣿");
    }

    #[test]
    fn meter_fill() {
        let text = |f: f64| -> String {
            meter(4, f, &Grad::heat()).iter().map(|s| s.content.to_string()).collect()
        };
        let filled = |f: f64| meter(4, f, &Grad::heat()).iter().filter(|s| s.style.fg != Some(color(TRACK))).count();
        assert_eq!(filled(0.0), 0);
        assert_eq!(filled(0.5), 2);
        assert_eq!(filled(1.0), 4);
        assert_eq!(text(0.625), "━━╸━");
    }

    #[test]
    fn stacked_shares() {
        let cells = stacked(10, &[(20.0, (1, 1, 1)), (30.0, (2, 2, 2))], 100.0);
        let count = |c: Rgb| cells.iter().filter(|s| s.style.fg == Some(color(c))).count();
        assert_eq!((count((1, 1, 1)), count((2, 2, 2)), count(TRACK)), (2, 3, 5));
    }
}
