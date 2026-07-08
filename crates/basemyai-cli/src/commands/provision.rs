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
            println!("\nProvisioning baseline model (fetching if absent)...");
        }
        let mp = basemyai::provision_with_progress(true, |recv, total| match total {
            Some(t) => eprint!("\r  {recv}/{t} bytes"),
            None => eprint!("\r  {recv} bytes"),
        })
        .await?;
        eprintln!();
        format.print(
            || {
                println!(
                    "model ready: {} (dim {}) at {}",
                    mp.model_id,
                    mp.dim,
                    mp.model_path.display()
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
                    println!(
                        "\nmodel already provisioned: {} at {}",
                        mp.model_id,
                        mp.model_path.display()
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
                    println!(
                        "\nbaseline model not provisioned. Re-run `basemyai setup --fetch` to download it (explicit consent)."
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
                    println!("\nprovisioned model: {} (dim {})", mp.model_id, mp.dim);
                    println!("  path: {}", mp.model_path.display());
                    println!("  files present: {present}");
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
            || println!("\nmodel not provisioned: {e}"),
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
    println!("Detected hardware:");
    println!("  RAM: {} MB", hw.total_ram_mb);
    println!("  CPU cores: {}", hw.cpu_cores);
    match hw.gpu_vram_mb {
        Some(v) => println!("  GPU VRAM: {v} MB"),
        None => println!("  GPU VRAM: (none detected)"),
    }
    println!("  Device: {:?}", hw.device);
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
                println!("no local LLM servers detected (Ollama / llama.cpp / OpenAI-compatible).");
                return;
            }
            println!("detected {} local LLM option(s):", opts.len());
            for o in &opts {
                let ram = o
                    .ram_mb
                    .map(|r| format!("{r} MB"))
                    .unwrap_or_else(|| "unknown".to_string());
                println!("  - {} via {:?} @ {} (RAM ~{ram})", o.model_id, o.backend, o.server_url);
            }
            if let Some(best) = best {
                println!("best for this machine: {}", best.model_id);
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
                println!("no additional models to suggest for this hardware.");
                return;
            }
            println!("suggested models (e.g. `ollama pull <tag>`):");
            for m in &suggestions {
                println!("  - {} (~{} MB) — {}", m.ollama_tag, m.ram_mb, m.description);
            }
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
