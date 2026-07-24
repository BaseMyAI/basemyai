// SPDX-License-Identifier: BUSL-1.1
//! Configuration du CLI développeur. Résolue par précédence : flag explicite
//! (géré par l'appelant, pas ici) → variables d'environnement → fichier
//! `~/.basemyai/config.toml` section `[cli]` → absent (erreur explicite côté
//! appelant). Même pattern que `basemyai-mcp/src/config.rs` / `basemyai-rest/src/config.rs`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Reflet TOML de la section `[cli]`.
#[derive(Debug, Default, Deserialize, Serialize)]
struct FileConfig {
    #[serde(default)]
    cli: CliSection,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct CliSection {
    db_path: Option<PathBuf>,
    agent: Option<String>,
}

/// Configuration effective : défauts (absents) ← fichier TOML ← environnement.
#[derive(Debug, Default, Clone)]
pub(crate) struct CliConfig {
    pub db_path: Option<PathBuf>,
    pub agent: Option<String>,
}

impl CliConfig {
    /// Charge la config depuis le fichier puis l'environnement (l'environnement
    /// gagne). Délègue la résolution à [`basemyai::ConfigDefaults`] — même
    /// source de vérité que celle utilisée par `Memory.open()` côté bindings
    /// (Node/Python) quand `path`/`agent_id` sont omis, pour que la CLI et les
    /// bindings retombent sur la même config sur une même machine. Ne lève
    /// jamais d'erreur fatale : un fichier absent ou un TOML invalide retombe
    /// sur les défauts — la résolution finale du `path`/`agent` est ce qui
    /// doit échouer, pas le chargement de la config elle-même.
    #[must_use]
    pub(crate) fn load() -> Self {
        let defaults = basemyai::ConfigDefaults::load();
        Self {
            db_path: defaults.db_path,
            agent: defaults.agent,
        }
    }

    /// Chemin standard du fichier de config (`~/.basemyai/config.toml`).
    #[must_use]
    pub(crate) fn file_path() -> Option<PathBuf> {
        basemyai::ConfigDefaults::file_path()
    }

    /// Résout le chemin du conteneur `.bmai` : flag explicite, sinon config/env.
    pub(crate) fn resolve_path(&self, explicit: Option<PathBuf>) -> Result<PathBuf, String> {
        explicit.or_else(|| self.db_path.clone()).ok_or_else(|| {
            "no .bmai path: pass it explicitly, set BASEMYAI_DB_PATH, or run `basemyai config set db-path <path>`"
                .to_string()
        })
    }

    /// Résout l'agent : flag explicite, sinon config/env.
    pub(crate) fn resolve_agent(&self, explicit: Option<String>) -> Result<String, String> {
        explicit.or_else(|| self.agent.clone()).ok_or_else(|| {
            "no agent id: pass --agent explicitly, set BASEMYAI_AGENT, or run `basemyai config set agent <id>`"
                .to_string()
        })
    }

    /// Réécrit `key` (`db-path` ou `agent`) dans le fichier de config, en
    /// préservant le reste du fichier s'il existe déjà.
    pub(crate) fn set(key: &str, value: &str) -> Result<PathBuf, String> {
        let path = Self::file_path().ok_or("cannot resolve home directory")?;
        let mut current = read_raw_table(&path)?;
        match key {
            "db-path" => current.db_path = Some(PathBuf::from(value)),
            "agent" => current.agent = Some(value.to_string()),
            other => return Err(format!("unknown config key '{other}' (expected db-path|agent)")),
        }
        write_raw_table(&path, current)?;
        Ok(path)
    }

    /// Retire `key` du fichier de config.
    pub(crate) fn unset(key: &str) -> Result<PathBuf, String> {
        let path = Self::file_path().ok_or("cannot resolve home directory")?;
        let mut current = read_raw_table(&path)?;
        match key {
            "db-path" => current.db_path = None,
            "agent" => current.agent = None,
            other => return Err(format!("unknown config key '{other}' (expected db-path|agent)")),
        }
        write_raw_table(&path, current)?;
        Ok(path)
    }
}

/// Lit la section `[cli]` brute pour `set`/`unset`. Contrairement à `load()`
/// (qui dégrade vers les défauts pour ne pas bloquer les commandes en lecture),
/// `set`/`unset` s'arrêtent sur un TOML invalide : réécrire le fichier sur la
/// base de défauts silencieux effacerait le contenu existant (même cassé) que
/// l'utilisateur voudrait corriger, pas perdre.
fn read_raw_table(path: &std::path::Path) -> Result<CliSection, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let file: FileConfig = toml::from_str(&s).map_err(|e| format!("parsing {}: {e}", path.display()))?;
            Ok(file.cli)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CliSection::default()),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

fn write_raw_table(path: &std::path::Path, cli: CliSection) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    let rendered = toml::to_string_pretty(&FileConfig { cli }).map_err(|e| format!("serializing config: {e}"))?;
    std::fs::write(path, rendered).map_err(|e| format!("writing {}: {e}", path.display()))
}
