//! Colours and gradients. Everything is 24-bit; the terminal background is
//! left alone so the app works on any dark theme.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

pub type Rgb = (u8, u8, u8);

pub const FG: Rgb = (224, 226, 240);
pub const DIM: Rgb = (124, 131, 160);
pub const FAINT: Rgb = (72, 77, 104);
pub const TRACK: Rgb = (46, 49, 70);

pub const ACCENT: Rgb = (94, 205, 255);
pub const GPU: Rgb = (118, 200, 20);
pub const PCORE: Rgb = (255, 121, 198);
pub const ECORE: Rgb = (120, 220, 255);
pub const SOC: Rgb = (189, 147, 249);
pub const BOARD: Rgb = (110, 125, 175);
pub const APPS: Rgb = (255, 184, 108);
pub const CACHE: Rgb = (88, 104, 160);
pub const POWER: Rgb = (255, 203, 107);
pub const RX: Rgb = (80, 250, 123);
pub const TX: Rgb = (255, 121, 198);

pub const OK: Rgb = (80, 250, 123);
pub const WARN: Rgb = (255, 203, 107);
pub const BAD: Rgb = (255, 85, 100);

pub fn color(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

pub fn span(text: impl Into<String>, c: Rgb) -> Span<'static> {
    Span::styled(text.into(), Style::new().fg(color(c)))
}

pub fn bold(text: impl Into<String>, c: Rgb) -> Span<'static> {
    Span::styled(text.into(), Style::new().fg(color(c)).add_modifier(Modifier::BOLD))
}

pub fn dim(text: impl Into<String>) -> Span<'static> {
    span(text, DIM)
}

/// A filled label, used for status badges.
pub fn badge(text: &str, c: Rgb) -> Span<'static> {
    Span::styled(
        format!(" {text} "),
        Style::new().fg(Color::Rgb(16, 18, 28)).bg(color(c)).add_modifier(Modifier::BOLD),
    )
}

fn lerp(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let f = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    (f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

/// Evenly spaced colour stops, sampled with a fraction in 0..=1.
#[derive(Clone, Debug)]
pub struct Grad(Vec<Rgb>);

impl Grad {
    /// Green through yellow and orange to red: load, heat, fullness.
    pub fn heat() -> Self {
        Grad(vec![(80, 250, 123), (190, 245, 120), (255, 203, 107), (255, 140, 80), (255, 85, 100)])
    }

    /// Dark to bright shades of a single hue.
    pub fn mono(c: Rgb) -> Self {
        Grad(vec![lerp((0, 0, 0), c, 0.45), c, lerp(c, (255, 255, 255), 0.45)])
    }

    pub fn between(a: Rgb, b: Rgb) -> Self {
        Grad(vec![a, b])
    }

    pub fn at(&self, t: f64) -> Rgb {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        let pos = t * (self.0.len() - 1) as f64;
        let i = (pos.floor() as usize).min(self.0.len() - 2);
        lerp(self.0[i], self.0[i + 1], pos - i as f64)
    }
}

/// Colour for a value on the heat scale.
pub fn heat(t: f64) -> Rgb {
    Grad::heat().at(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_ends_and_middle() {
        let g = Grad::between((0, 0, 0), (200, 100, 50));
        assert_eq!(g.at(0.0), (0, 0, 0));
        assert_eq!(g.at(1.0), (200, 100, 50));
        assert_eq!(g.at(0.5), (100, 50, 25));
        assert_eq!(g.at(7.0), (200, 100, 50));
        assert_eq!(g.at(f64::NAN), (0, 0, 0));
    }
}
