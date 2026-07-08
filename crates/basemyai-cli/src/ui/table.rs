// SPDX-License-Identifier: BUSL-1.1

use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Cell, ContentArrangement, Table};
use textwrap::wrap;

use crate::ui::settings;

pub(crate) fn print_table(headers: &[&str], rows: Vec<Vec<String>>) {
    if settings().quiet {
        return;
    }
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(headers.to_vec());

    for row in rows {
        table.add_row(row.into_iter().map(Cell::new).collect::<Vec<_>>());
    }
    println!("{table}");
}

pub(crate) fn wrap_excerpt(text: &str, width: usize) -> String {
    if text.is_empty() {
        return String::new();
    }
    let effective = width.clamp(20, 120);
    wrap(text, effective)
        .into_iter()
        .map(|line| line.into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}
