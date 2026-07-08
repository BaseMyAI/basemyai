// SPDX-License-Identifier: BUSL-1.1

use owo_colors::OwoColorize as _;

use crate::ui::color::{Stream, colors_enabled};
use crate::ui::settings;

fn enabled(stream: Stream) -> bool {
    colors_enabled(settings().color_mode, stream)
}

fn paint(stream: Stream, plain: &str, f: impl FnOnce(&str) -> String) -> String {
    if enabled(stream) { f(plain) } else { plain.to_string() }
}

pub(crate) fn accent(text: &str, stream: Stream) -> String {
    paint(stream, text, |t| t.truecolor(0xd7, 0xff, 0x3f).bold().to_string())
}

pub(crate) fn muted(text: &str, stream: Stream) -> String {
    paint(stream, text, |t| t.truecolor(0x74, 0x6f, 0x66).to_string())
}

pub(crate) fn success(text: &str, stream: Stream) -> String {
    paint(stream, text, |t| t.truecolor(0x00, 0xa8, 0x8a).to_string())
}

pub(crate) fn warning(text: &str, stream: Stream) -> String {
    paint(stream, text, |t| t.truecolor(0xff, 0x5a, 0x1f).to_string())
}

pub(crate) fn error(text: &str, stream: Stream) -> String {
    paint(stream, text, |t| t.bright_red().bold().to_string())
}

pub(crate) fn layer(layer: &str, stream: Stream) -> String {
    match layer {
        "short_term" => paint(stream, layer, |t| t.yellow().to_string()),
        "episodic" => paint(stream, layer, |t| t.cyan().to_string()),
        "procedural" => paint(stream, layer, |t| t.magenta().to_string()),
        "semantic" => paint(stream, layer, |t| t.green().to_string()),
        _ => layer.to_string(),
    }
}

pub(crate) fn ok_mark(stream: Stream) -> String {
    if enabled(stream) {
        "✓".to_string()
    } else {
        "(ok)".to_string()
    }
}

pub(crate) fn fail_mark(stream: Stream) -> String {
    if enabled(stream) {
        "✗".to_string()
    } else {
        "(fail)".to_string()
    }
}
