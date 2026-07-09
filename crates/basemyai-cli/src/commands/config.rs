// SPDX-License-Identifier: BUSL-1.1
//! Commande `config` : lit/écrit `~/.basemyai/config.toml` (voir
//! `crate::persisted_config::CliConfig`). À ne pas confondre avec
//! `output::Format` (le format de sortie de toutes les autres commandes).

use std::path::PathBuf;

use crate::cli::ConfigAction;
use crate::error::CliError;
use crate::output::Format;
use crate::persisted_config::CliConfig;

pub(crate) fn run(
    action: ConfigAction,
    format: Format,
    cli_db: Option<PathBuf>,
    cli_agent: Option<String>,
) -> Result<(), CliError> {
    match action {
        ConfigAction::Show => {
            let cfg = CliConfig::load();
            let effective_path = cfg.resolve_path(cli_db).ok();
            let effective_agent = cfg.resolve_agent(cli_agent).ok();
            format.print(
                || {
                    crate::ui::render::section("CLI configuration");
                    crate::ui::render::key_values(&[
                        (
                            "config_file:",
                            CliConfig::file_path()
                                .map_or_else(|| "(unresolvable)".to_string(), |p| p.display().to_string()),
                        ),
                        (
                            "db_path:",
                            effective_path
                                .as_ref()
                                .map_or_else(|| "(unset)".to_string(), |p| p.display().to_string()),
                        ),
                        ("agent:", effective_agent.as_deref().unwrap_or("(unset)").to_string()),
                    ]);
                },
                || {
                    serde_json::json!({
                        "db_path": effective_path.clone().map(|p| p.display().to_string()),
                        "agent": effective_agent.clone(),
                    })
                },
            );
            Ok(())
        }
        ConfigAction::Set { key, value } => {
            let path = CliConfig::set(&key, &value).map_err(CliError::Config)?;
            format.print(
                || println!("set {key} = {value} ({})", path.display()),
                || serde_json::json!({ "key": key, "value": value, "file": path.display().to_string() }),
            );
            Ok(())
        }
        ConfigAction::Unset { key } => {
            let path = CliConfig::unset(&key).map_err(CliError::Config)?;
            format.print(
                || println!("unset {key} ({})", path.display()),
                || serde_json::json!({ "key": key, "file": path.display().to_string() }),
            );
            Ok(())
        }
        ConfigAction::Key { action } => super::config_key::run(action, format),
    }
}
