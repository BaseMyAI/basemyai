// SPDX-License-Identifier: BUSL-1.1
//! Dispatch des sous-commandes vers leur implémentation. Chaque domaine a son
//! propre fichier (`config`, `container`, `memory`, `graph`, `maintenance`,
//! `provision`) ; ce module ne fait que router `cli::Command` vers eux.

mod compile_context;
mod config;
mod config_key;
mod container;
#[cfg(feature = "eval-lab")]
mod eval;
mod graph;
mod maintenance;
mod memory;
mod provision;

use std::path::PathBuf;

#[cfg(feature = "eval-lab")]
use crate::cli::EvalAction;
use crate::cli::{Cli, Command, GraphAction, LlmAction};
use crate::context::{context_profile, context_render_format, context_source_policy, open_memory};
use crate::error::CliError;
use crate::output::Format;
use crate::persisted_config::CliConfig;

#[cfg(feature = "embed")]
pub(crate) async fn dispatch(cli: Cli, format: Format) -> Result<(), CliError> {
    let cfg = CliConfig::load();
    let resolve_path =
        || -> Result<PathBuf, CliError> { cfg.resolve_path(cli.db.clone()).map_err(CliError::NotConfigured) };
    let resolve_agent =
        || -> Result<String, CliError> { cfg.resolve_agent(cli.agent.clone()).map_err(CliError::NotConfigured) };

    match cli.command {
        Command::Setup { fetch } => provision::setup(fetch, format).await,
        Command::Status => provision::status(format).await,
        Command::Config { action } => config::run(action, format, cli.db.clone(), cli.agent.clone()),
        Command::Init { path, low_memory } => container::init(&path, low_memory, format).await,
        Command::Inspect => container::inspect(&resolve_path()?, format).await,
        Command::Verify { physical, logical } => {
            let mode = if logical {
                basemyai::storage::integrity::VerifyMode::FullLogical
            } else if physical {
                basemyai::storage::integrity::VerifyMode::FullPhysical
            } else {
                basemyai::storage::integrity::VerifyMode::Quick
            };
            container::verify(&resolve_path()?, mode, format).await
        }
        Command::Repair { dry_run } => container::repair(&resolve_path()?, dry_run, format).await,
        Command::RebuildIndexes => container::rebuild_indexes(&resolve_path()?, format).await,
        Command::Compact => container::compact(&resolve_path()?, format).await,
        Command::Reembed { all, ids } => {
            let path = resolve_path()?;
            let embedder = crate::context::load_embedder().await?;
            if all || !ids.is_empty() {
                let agent = resolve_agent()?;
                container::reembed_scoped(&path, &agent, all, ids, embedder, format).await
            } else {
                container::reembed_missing(&path, embedder, format).await
            }
        }
        Command::Migrate => container::migrate(&resolve_path()?, format).await,
        Command::Stats => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            let mem = open_memory(&path, &agent).await?;
            let s = mem.stats().await?;
            format.print(
                || {
                    crate::ui::render::section(&format!("Agent '{agent}' — valid memories"));
                    crate::ui::table::print_table(
                        &["Layer", "Count"],
                        vec![
                            vec!["short_term".to_string(), s.short_term.to_string()],
                            vec!["episodic".to_string(), s.episodic.to_string()],
                            vec!["procedural".to_string(), s.procedural.to_string()],
                            vec!["semantic".to_string(), s.semantic.to_string()],
                            vec!["total".to_string(), s.total().to_string()],
                        ],
                    );
                },
                || {
                    serde_json::json!({
                        "agent": agent,
                        "short_term": s.short_term,
                        "episodic": s.episodic,
                        "procedural": s.procedural,
                        "semantic": s.semantic,
                        "total": s.total(),
                    })
                },
            );
            Ok(())
        }
        Command::Remember { layer, text, file } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            let mem = open_memory(&path, &agent).await?;
            memory::remember(&mem, layer, text, file, format).await
        }
        Command::Recall {
            query,
            k,
            hybrid,
            layer,
            graph,
        } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            let mem = open_memory(&path, &agent).await?;
            memory::recall(&mem, &query, k, hybrid, layer, graph, format).await
        }
        Command::Context {
            query,
            token_budget,
            candidate_limit,
            include_procedural,
            source_policy,
            profile,
            render_format,
            explain,
        } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            let mem = open_memory(&path, &agent).await?;
            compile_context::run(
                &mem,
                &query,
                token_budget,
                candidate_limit,
                include_procedural,
                context_source_policy(source_policy),
                context_profile(profile),
                context_render_format(render_format),
                explain,
                format,
            )
            .await
        }
        Command::List {
            layer,
            limit,
            include_invalid,
        } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            memory::list(&path, &agent, layer, limit, include_invalid, format).await
        }
        Command::Forget { id } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            memory::forget(&path, &agent, &id, format).await
        }
        Command::Invalidate { id } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            memory::invalidate(&path, &agent, &id, format).await
        }
        Command::Purge { yes } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            memory::purge(&path, &agent, yes, format).await
        }
        Command::Export { out } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            memory::export(&path, &agent, out, format).await
        }
        Command::Import { file, trusted } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            memory::import(&path, &agent, &file, trusted, format).await
        }
        Command::RotateKey {
            new_key,
            passphrase,
            low_memory,
            full,
        } => {
            let path = resolve_path()?;
            container::rotate_key(&path, new_key, passphrase, low_memory, full, format).await
        }
        Command::Graph { action } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            match action {
                GraphAction::AddEntity { id, kind, label } => {
                    graph::add_entity(&path, &agent, &id, &kind, &label, format).await
                }
                GraphAction::AddEdge {
                    src,
                    relation,
                    dst,
                    weight,
                } => graph::add_edge(&path, &agent, &src, &relation, &dst, weight, format).await,
                GraphAction::Traverse { start, depth } => graph::traverse(&path, &agent, &start, depth, format).await,
            }
        }
        Command::Consolidate => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            maintenance::consolidate(&path, &agent, format).await
        }
        Command::ForgetAdaptive {
            capacity,
            half_life_secs,
            dry_run,
        } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            maintenance::forget_adaptive(&path, &agent, capacity, half_life_secs, dry_run, format).await
        }
        Command::Gc { page_size, dry_run } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            maintenance::gc(&path, &agent, page_size, dry_run, format).await
        }
        Command::Llm { action } => match action {
            LlmAction::Detect => provision::llm_detect(format).await,
            LlmAction::Suggest => provision::llm_suggest(format).await,
        },
        #[cfg(feature = "eval-lab")]
        Command::Eval { action } => match action {
            EvalAction::Run {
                dataset,
                output,
                human,
                timings,
            } => eval::run(&dataset, &output, human, timings, format).await,
            EvalAction::Compare {
                baseline,
                current,
                output,
                human,
                fail_on_regression,
            } => eval::compare(&baseline, &current, output, human, fail_on_regression, format).await,
        },
        Command::Completions { shell } => {
            completions(shell);
            Ok(())
        }
    }
}

fn completions(shell: clap_complete::Shell) {
    let mut cmd = <Cli as clap::CommandFactory>::command();
    clap_complete::generate(shell, &mut cmd, "basemyai", &mut std::io::stdout());
}

#[cfg(not(feature = "embed"))]
pub(crate) async fn dispatch(_cli: Cli, _format: Format) -> Result<(), CliError> {
    Err(CliError::Config(
        "basemyai CLI must be built with the `embed` feature (it is in the default feature set)".to_string(),
    ))
}
