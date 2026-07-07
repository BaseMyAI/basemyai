// SPDX-License-Identifier: BUSL-1.1
//! Dispatch des sous-commandes vers leur implémentation. Chaque domaine a son
//! propre fichier (`config`, `container`, `memory`, `graph`, `maintenance`,
//! `provision`) ; ce module ne fait que router `cli::Command` vers eux.

mod config;
mod container;
mod graph;
mod maintenance;
mod memory;
mod provision;

use std::path::PathBuf;

use crate::cli::{Cli, Command, GraphAction, LlmAction, MaintenanceAction};
use crate::context::open_memory;
use crate::error::CliError;
use crate::output::Format;
use crate::persisted_config::CliConfig;

#[cfg(all(feature = "crypto", feature = "embed"))]
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
        Command::Init { path } => container::init(&path, format).await,
        Command::Inspect => container::inspect(&resolve_path()?, format).await,
        Command::Verify => container::verify(&resolve_path()?, format).await,
        Command::Migrate => container::migrate(&resolve_path()?, format).await,
        Command::Stats => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            let mem = open_memory(&path, &agent).await?;
            let s = mem.stats().await?;
            format.print(
                || {
                    println!("Agent '{agent}' — valid memories:");
                    println!("  short_term: {}", s.short_term);
                    println!("  episodic:   {}", s.episodic);
                    println!("  procedural: {}", s.procedural);
                    println!("  semantic:   {}", s.semantic);
                    println!("  total:      {}", s.total());
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
        Command::Import { file } => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            memory::import(&path, &agent, &file, format).await
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
        Command::Maintenance { action } => {
            let path = resolve_path()?;
            match action {
                MaintenanceAction::Gc => maintenance::gc(&path, format).await,
                MaintenanceAction::ForgetAdaptive {
                    capacity,
                    half_life_secs,
                } => maintenance::forget_adaptive(&path, capacity, half_life_secs, format).await,
            }
        }
        Command::Consolidate => {
            let path = resolve_path()?;
            let agent = resolve_agent()?;
            maintenance::consolidate(&path, &agent, format).await
        }
        Command::Llm { action } => match action {
            LlmAction::Detect => provision::llm_detect(format).await,
            LlmAction::Suggest => provision::llm_suggest(format).await,
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

#[cfg(not(all(feature = "crypto", feature = "embed")))]
pub(crate) async fn dispatch(_cli: Cli, _format: Format) -> Result<(), CliError> {
    Err(CliError::Config(
        "basemyai CLI must be built with the `crypto` and `embed` features (they are in the default feature set)"
            .to_string(),
    ))
}
