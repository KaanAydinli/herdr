//! Parse forward in short batches. Preserve multiline grammar state at block boundaries.
use ratatui::style::Color;
use std::{
    path::Path,
    sync::LazyLock,
    time::{Duration, Instant},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{Color as Rgb, HighlightState, StyleModifier, Theme, ThemeItem},
    parsing::{ParseState, SyntaxDefinition, SyntaxSet},
};

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_nonewlines);
static TOML: LazyLock<SyntaxSet> = LazyLock::new(|| {
    let mut builder = syntect::parsing::SyntaxSetBuilder::new();
    builder.add(
        SyntaxDefinition::load_from_str(include_str!("toml.sublime-syntax"), false, None)
            .expect("bundled TOML grammar"),
    );
    builder.build()
});
static THEME: LazyLock<Theme> = LazyLock::new(|| {
    let mut theme = Theme::default();
    theme.settings.foreground = Some(Rgb {
        r: 204,
        g: 204,
        b: 204,
        a: 255,
    });
    let rules: Vec<(&str, &str)> =
        serde_json::from_str(include_str!("dark-modern.json")).expect("bundled Dark Modern colors");
    for (selector, hex) in rules {
        let Ok(rgb) = u32::from_str_radix(hex.trim_start_matches('#'), 16) else {
            continue;
        };
        let foreground = Rgb {
            r: (rgb >> 16) as u8,
            g: (rgb >> 8) as u8,
            b: rgb as u8,
            a: 255,
        };
        for name in selector.split(',') {
            if let Ok(scope) = name.trim().parse() {
                theme.scopes.push(ThemeItem {
                    scope,
                    style: StyleModifier {
                        foreground: Some(foreground),
                        ..Default::default()
                    },
                });
            }
        }
    }
    theme
});

const BLOCK: usize = 64;
#[derive(Default)]
pub struct Colors {
    spans: Vec<Vec<(usize, Color)>>,
    checkpoints: Vec<(HighlightState, ParseState)>,
    parser: Option<HighlightLines<'static>>,
    pub pending: bool,
}

impl Colors {
    pub fn invalidate(&mut self, row: usize) {
        if row >= self.spans.len() {
            return;
        }
        let block = row / BLOCK;
        self.spans.truncate(block * BLOCK);
        self.checkpoints.truncate(block);
        self.parser = self
            .checkpoints
            .last()
            .cloned()
            .map(|(h, p)| HighlightLines::from_state(&THEME, h, p));
    }

    pub fn advance(&mut self, path: &Path, lines: &[String], end: usize) {
        let end = end.min(lines.len());
        self.pending = false;
        if self.spans.len() >= end {
            return;
        }
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        let set = if ext == "toml" { &*TOML } else { &*SYNTAXES };
        let Some(syntax) = set.find_syntax_by_extension(ext) else {
            return;
        };
        let mut parser = self
            .parser
            .take()
            .unwrap_or_else(|| HighlightLines::new(syntax, &THEME));
        let start = Instant::now();
        for line in &lines[self.spans.len()..end] {
            let mut offset = 0;
            let mut spans = Vec::new();
            // Keep minified or generated lines from monopolizing the input loop.
            if line.len() <= 16 * 1024 {
                if let Ok(parts) = parser.highlight_line(line, set) {
                    for (style, text) in parts {
                        offset += text.len();
                        let c = style.foreground;
                        spans.push((offset, Color::Rgb(c.r, c.g, c.b)));
                    }
                }
            }
            self.spans.push(spans);
            if self.spans.len().is_multiple_of(BLOCK) {
                let (h, p) = parser.state();
                self.checkpoints.push((h.clone(), p.clone()));
                parser = HighlightLines::from_state(&THEME, h, p);
            }
            if start.elapsed() >= Duration::from_millis(4) {
                break;
            }
        }
        self.parser = Some(parser);
        self.pending = self.spans.len() < end;
    }
    pub fn line(&self, row: usize) -> &[(usize, Color)] {
        self.spans.get(row).map_or(&[], Vec::as_slice)
    }
}
