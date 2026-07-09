// SPDX-License-Identifier: BUSL-1.1

use crate::ui::color::Stream;
use crate::ui::settings;
use crate::ui::theme;

pub(crate) fn section(title: &str) {
    if settings().quiet {
        return;
    }
    println!("{}", theme::accent(title, Stream::Stdout));
}

pub(crate) fn info(message: &str) {
    if settings().quiet {
        return;
    }
    println!("{message}");
}

pub(crate) fn success(message: &str) {
    if settings().quiet {
        return;
    }
    println!("{}", theme::success(message, Stream::Stdout));
}

pub(crate) fn error(message: &str) {
    eprintln!("{}", theme::error(message, Stream::Stderr));
}

pub(crate) fn warning(message: &str) {
    eprintln!(
        "{} {}",
        theme::warning("warning:", Stream::Stderr),
        theme::muted(message, Stream::Stderr)
    );
}

pub(crate) fn hint(message: &str) {
    eprintln!("{} {message}", theme::accent("hint:", Stream::Stderr));
}

pub(crate) fn key_values(rows: &[(&str, String)]) {
    if settings().quiet {
        return;
    }
    let max_key = rows.iter().map(|(k, _)| k.len()).max().unwrap_or_default();
    for (key, value) in rows {
        println!("{key:<width$} {value}", width = max_key + 1);
    }
}
