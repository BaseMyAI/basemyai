// SPDX-License-Identifier: BUSL-1.1

use crate::error::CliError;
use crate::ui::color::Stream;
use crate::ui::theme;

pub(crate) fn print_cli_error(err: &CliError) {
    let rendered = err.to_string();
    let mut lines = rendered.lines();

    if let Some(first) = lines.next() {
        eprintln!("{} {first}", theme::error("error:", Stream::Stderr));
    } else {
        eprintln!("{}", theme::error("error", Stream::Stderr));
    }

    for line in lines {
        if line.starts_with("hint:") {
            eprintln!(
                "{} {}",
                theme::accent("hint:", Stream::Stderr),
                line.trim_start_matches("hint:").trim()
            );
        } else if line.starts_with("see:") {
            eprintln!(
                "{} {}",
                theme::accent("see:", Stream::Stderr),
                line.trim_start_matches("see:").trim()
            );
        } else if !line.trim().is_empty() {
            eprintln!("  {line}");
        }
    }
}
