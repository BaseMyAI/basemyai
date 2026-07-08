// SPDX-License-Identifier: BUSL-1.1
//! Provisionnement hardware-aware (ADR-010) : `setup`, `status`, `llm
//! detect`/`llm suggest`. Jamais de téléchargement implicite — `--fetch` est
//! le seul déclencheur.

use crate::error::CliError;
use crate::output::Format;

pub(crate) async fn setup(fetch: bool, format: Format) -> Result<(), CliError> {
    let hw = basemyai::detect_hardware();
    if format == Format::Text {
        print_hardware(&hw);
    }

    if fetch {
        if format == Format::Text {
            crate::ui::render::section("Provisioning baseline model (fetching if absent)");
        }
        let bar = crate::ui::progress::DownloadBar::new("Downloading model");
        let mp = basemyai::provision_with_progress(true, |recv, total| bar.update(recv, total)).await?;
        bar.finish_and_clear();
        format.print(
            || {
                crate::ui::table::print_table(
                    &["Field", "Value"],
                    vec![
                        vec!["model_id".to_string(), mp.model_id.clone()],
                        vec!["dim".to_string(), mp.dim.to_string()],
                        vec!["path".to_string(), mp.model_path.display().to_string()],
                        vec!["provisioned".to_string(), "true".to_string()],
                    ],
                );
            },
            || {
                serde_json::json!({
                    "hardware": hardware_json(&hw),
                    "model_id": mp.model_id,
                    "dim": mp.dim,
                    "path": mp.model_path.display().to_string(),
                    "provisioned": true,
                })
            },
        );
    } else {
        match basemyai::provision(false).await {
            Ok(mp) => format.print(
                || {
                    crate::ui::render::section("Model already provisioned");
                    crate::ui::table::print_table(
                        &["Field", "Value"],
                        vec![
                            vec!["model_id".to_string(), mp.model_id.clone()],
                            vec!["dim".to_string(), mp.dim.to_string()],
                            vec!["path".to_string(), mp.model_path.display().to_string()],
                            vec!["provisioned".to_string(), "true".to_string()],
                        ],
                    );
                },
                || {
                    serde_json::json!({
                        "hardware": hardware_json(&hw),
                        "model_id": mp.model_id,
                        "dim": mp.dim,
                        "path": mp.model_path.display().to_string(),
                        "provisioned": true,
                    })
                },
            ),
            Err(_) => format.print(
                || {
                    crate::ui::render::warning(
                        "baseline model not provisioned. Re-run `basemyai setup --fetch` to download it (explicit consent).",
                    );
                },
                || {
                    serde_json::json!({
                        "hardware": hardware_json(&hw),
                        "model_id": null,
                        "dim": null,
                        "path": null,
                        "provisioned": false,
                    })
                },
            ),
        }
    }
    Ok(())
}

pub(crate) async fn status(format: Format) -> Result<(), CliError> {
    let hw = basemyai::detect_hardware();
    if format == Format::Text {
        print_hardware(&hw);
    }
    match basemyai::provision(false).await {
        Ok(mp) => {
            let present = mp.model_path.exists();
            format.print(
                || {
                    crate::ui::render::section("Provisioned model");
                    crate::ui::table::print_table(
                        &["Field", "Value"],
                        vec![
                            vec!["model_id".to_string(), mp.model_id.clone()],
                            vec!["dim".to_string(), mp.dim.to_string()],
                            vec!["path".to_string(), mp.model_path.display().to_string()],
                            vec!["files_present".to_string(), present.to_string()],
                        ],
                    );
                },
                || {
                    serde_json::json!({
                        "hardware": hardware_json(&hw),
                        "model_id": mp.model_id,
                        "dim": mp.dim,
                        "path": mp.model_path.display().to_string(),
                        "provisioned": present,
                    })
                },
            );
        }
        Err(e) => format.print(
            || crate::ui::render::warning(&format!("model not provisioned: {e}")),
            || {
                serde_json::json!({
                    "hardware": hardware_json(&hw),
                    "model_id": null,
                    "dim": null,
                    "path": null,
                    "provisioned": false,
                })
            },
        ),
    }
    Ok(())
}

fn print_hardware(hw: &basemyai::HardwareProfile) {
    crate::ui::render::section("Detected hardware");
    crate::ui::render::key_values(&[
        ("ram_mb:", hw.total_ram_mb.to_string()),
        ("cpu_cores:", hw.cpu_cores.to_string()),
        (
            "gpu_vram_mb:",
            hw.gpu_vram_mb
                .map(|v| v.to_string())
                .unwrap_or_else(|| "(none detected)".to_string()),
        ),
        ("device:", format!("{:?}", hw.device)),
    ]);
}

fn hardware_json(hw: &basemyai::HardwareProfile) -> serde_json::Value {
    serde_json::json!({
        "ram_mb": hw.total_ram_mb,
        "cpu_cores": hw.cpu_cores,
        "gpu_vram_mb": hw.gpu_vram_mb,
        "device": format!("{:?}", hw.device),
    })
}

pub(crate) async fn llm_detect(format: Format) -> Result<(), CliError> {
    let opts = basemyai::detect_llm_options().await;
    let best = basemyai::best_llm_option(&opts);
    format.print(
        || {
            if opts.is_empty() {
                crate::ui::render::warning("no local LLM servers detected (Ollama / llama.cpp / OpenAI-compatible).");
                return;
            }
            crate::ui::render::section(&format!("Detected {} local LLM option(s)", opts.len()));
            crate::ui::table::print_table(
                &["model_id", "backend", "server_url", "ram_mb"],
                opts.iter()
                    .map(|o| {
                        vec![
                            o.model_id.clone(),
                            format!("{:?}", o.backend),
                            o.server_url.clone(),
                            o.ram_mb
                                .map(|r| format!("{r}"))
                                .unwrap_or_else(|| "unknown".to_string()),
                        ]
                    })
                    .collect::<Vec<_>>(),
            );
            if let Some(best) = best {
                crate::ui::render::success(&format!("best for this machine: {}", best.model_id));
            }
        },
        || {
            serde_json::json!({
                "options": opts.iter().map(|o| serde_json::json!({
                    "model_id": o.model_id,
                    "backend": format!("{:?}", o.backend),
                    "server_url": o.server_url,
                    "ram_mb": o.ram_mb,
                })).collect::<Vec<_>>(),
                "best": best.map(|b| b.model_id.clone()),
            })
        },
    );
    Ok(())
}

pub(crate) async fn llm_suggest(format: Format) -> Result<(), CliError> {
    let installed = basemyai::detect_llm_options().await;
    let suggestions = basemyai::propose_models_to_install(&installed);
    format.print(
        || {
            if suggestions.is_empty() {
                crate::ui::render::info("no additional models to suggest for this hardware.");
                return;
            }
            crate::ui::render::section("Suggested models (e.g. `ollama pull <tag>`)");
            crate::ui::table::print_table(
                &["ollama_tag", "ram_mb", "description"],
                suggestions
                    .iter()
                    .map(|m| {
                        vec![
                            m.ollama_tag.to_string(),
                            m.ram_mb.to_string(),
                            m.description.to_string(),
                        ]
                    })
                    .collect::<Vec<_>>(),
            );
        },
        || {
            serde_json::json!({
                "suggestions": suggestions.iter().map(|m| serde_json::json!({
                    "ollama_tag": m.ollama_tag,
                    "ram_mb": m.ram_mb,
                    "description": m.description,
                })).collect::<Vec<_>>(),
            })
        },
    );
    Ok(())
}
