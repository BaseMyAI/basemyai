// SPDX-License-Identifier: BUSL-1.1

pub(crate) mod color;
pub(crate) mod error;
pub(crate) mod progress;
pub(crate) mod render;
pub(crate) mod table;
pub(crate) mod theme;

use std::sync::OnceLock;

pub(crate) use color::ColorMode;

#[derive(Debug, Copy, Clone)]
pub(crate) struct UiSettings {
    pub(crate) color_mode: ColorMode,
    pub(crate) quiet: bool,
    pub(crate) no_progress: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            color_mode: ColorMode::Auto,
            quiet: false,
            no_progress: false,
        }
    }
}

static SETTINGS: OnceLock<UiSettings> = OnceLock::new();

pub(crate) fn init(settings: UiSettings) {
    let _ = SETTINGS.set(settings);
}

pub(crate) fn settings() -> &'static UiSettings {
    SETTINGS.get_or_init(UiSettings::default)
}
