// SPDX-License-Identifier: BUSL-1.1

use std::io::{IsTerminal as _, stderr, stdout};

use clap::ValueEnum;

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some()
}

fn force_color() -> bool {
    std::env::var_os("FORCE_COLOR").is_some()
}

fn is_tty(stream: Stream) -> bool {
    match stream {
        Stream::Stdout => stdout().is_terminal(),
        Stream::Stderr => stderr().is_terminal(),
    }
}

pub(crate) fn colors_enabled(mode: ColorMode, stream: Stream) -> bool {
    match mode {
        ColorMode::Never => false,
        ColorMode::Always => true,
        ColorMode::Auto => {
            if no_color() {
                return false;
            }
            if force_color() {
                return true;
            }
            is_tty(stream)
        }
    }
}
