// SPDX-License-Identifier: BUSL-1.1

use std::io::IsTerminal as _;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use crate::ui::settings;

fn enabled() -> bool {
    !settings().quiet && !settings().no_progress && std::io::stderr().is_terminal()
}

pub(crate) enum Spinner {
    Disabled,
    Active(ProgressBar),
}

impl Spinner {
    pub(crate) fn finish_and_clear(self) {
        if let Self::Active(pb) = self {
            pb.finish_and_clear();
        }
    }
}

pub(crate) fn spinner(message: &str) -> Spinner {
    if !enabled() {
        return Spinner::Disabled;
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::with_template("{spinner:.green} {msg}").expect("valid spinner template"));
    pb.enable_steady_tick(Duration::from_millis(90));
    pb.set_message(message.to_string());
    Spinner::Active(pb)
}

pub(crate) struct DownloadBar {
    bar: Option<ProgressBar>,
}

impl DownloadBar {
    pub(crate) fn new(label: &str) -> Self {
        if !enabled() {
            return Self { bar: None };
        }

        let pb = ProgressBar::new(0);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} {msg} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec})",
            )
            .expect("valid download template")
            .progress_chars("=>-"),
        );
        pb.set_message(label.to_string());
        Self { bar: Some(pb) }
    }

    pub(crate) fn update(&self, received: u64, total: Option<u64>) {
        if let Some(pb) = &self.bar {
            if let Some(t) = total {
                pb.set_length(t);
            }
            pb.set_position(received);
        } else {
            match total {
                Some(t) => eprint!("\r  {received}/{t} bytes"),
                None => eprint!("\r  {received} bytes"),
            }
        }
    }

    pub(crate) fn finish_and_clear(&self) {
        if let Some(pb) = &self.bar {
            pb.finish_and_clear();
        } else {
            eprintln!();
        }
    }
}
