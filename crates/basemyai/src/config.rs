// SPDX-License-Identifier: BUSL-1.1
//! Défauts partagés pour `db_path`/`agent`, résolus depuis
//! `~/.basemyai/config.toml` (section `[cli]`) puis l'environnement
//! (`BASEMYAI_DB_PATH`/`BASEMYAI_AGENT`, qui gagne sur le fichier).
//!
//! Même format que celui écrit par `basemyai config set` (CLI, ADR — voir
//! `basemyai-cli::persisted_config`) : un consommateur binding (Node/Python)
//! qui ouvre une `Memory` sans `path`/`agent_id` explicites retombe sur la
//! même config qu'une session CLI sur la même machine. Lecture seule ici —
//! l'écriture (`set`/`unset`) reste un geste CLI explicite.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    cli: CliSection,
}

#[derive(Debug, Default, Deserialize)]
struct CliSection {
    db_path: Option<PathBuf>,
    agent: Option<String>,
}

/// Défauts résolus pour `db_path`/`agent`. `None` si ni le fichier ni
/// l'environnement ne renseignent la valeur — l'appelant décide alors s'il
/// exige un flag explicite.
#[derive(Debug, Default, Clone)]
pub struct ConfigDefaults {
    pub db_path: Option<PathBuf>,
    pub agent: Option<String>,
}

/// Nom de fichier du store "zéro-config" (SDK, aucun `path` configuré nulle
/// part) — **relatif au répertoire courant**, jamais un chemin global partagé
/// sous `~/.basemyai/...` : deux projets différents sur la même machine qui
/// appellent tous deux `Memory.open()` sans configuration ne doivent jamais
/// atterrir dans le même fichier par accident. L'isolation par agent
/// (ADR-006) protège plusieurs agents *dans* un même store, pas deux stores
/// différents qui se chevauchent par accident.
pub const DEFAULT_DB_FILENAME: &str = "basemyai.bmai";

/// Agent "zéro-config" — raisonnable uniquement parce que le store lui-même
/// est déjà scopé au projet courant ([`DEFAULT_DB_FILENAME`]). Un usage
/// multi-agent explicite doit toujours passer `agent_id`.
pub const DEFAULT_AGENT: &str = "default";

impl ConfigDefaults {
    /// Charge les défauts : fichier `~/.basemyai/config.toml` puis
    /// environnement (qui écrase le fichier). Ne lève jamais d'erreur : un
    /// fichier absent ou invalide retombe silencieusement sur "rien de
    /// configuré" — c'est à l'appelant de décider que l'absence est fatale.
    #[must_use]
    pub fn load() -> Self {
        let mut cfg = load_file_config().unwrap_or_default();
        if let Ok(p) = std::env::var("BASEMYAI_DB_PATH") {
            cfg.db_path = Some(PathBuf::from(p));
        }
        if let Ok(a) = std::env::var("BASEMYAI_AGENT") {
            cfg.agent = Some(a);
        }
        cfg
    }

    /// Chemin standard du fichier de config (`~/.basemyai/config.toml`).
    #[must_use]
    pub fn file_path() -> Option<PathBuf> {
        home_dir().map(|h| h.join(".basemyai").join("config.toml"))
    }

    /// Résolution "SDK, zéro config" pour `path` : explicite → config
    /// partagée (fichier/env) → défaut intégré ([`DEFAULT_DB_FILENAME`],
    /// relatif au répertoire courant). Contrairement à la CLI
    /// (`basemyai-cli::persisted_config::resolve_path`, qui exige un flag ou
    /// une config explicite), ne renvoie jamais d'absence : un premier
    /// `Memory.open()` côté binding doit fonctionner sans étape préalable.
    #[must_use]
    pub fn resolve_open_path(&self, explicit: Option<PathBuf>) -> PathBuf {
        explicit
            .or_else(|| self.db_path.clone())
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DB_FILENAME))
    }

    /// Résolution "SDK, zéro config" pour `agent` : explicite → config
    /// partagée → défaut intégré ([`DEFAULT_AGENT`]). Voir
    /// [`Self::resolve_open_path`] pour le principe.
    #[must_use]
    pub fn resolve_open_agent(&self, explicit: Option<String>) -> String {
        explicit
            .or_else(|| self.agent.clone())
            .unwrap_or_else(|| DEFAULT_AGENT.to_string())
    }
}

/// `USERPROFILE` sur Windows, `HOME` ailleurs — un override de process
/// (test, sandbox) est toujours respecté, contrairement au crate `dirs`.
fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn load_file_config() -> Option<ConfigDefaults> {
    let path = ConfigDefaults::file_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let file: FileConfig = toml::from_str(&raw).ok()?;
    Some(ConfigDefaults {
        db_path: file.cli.db_path,
        agent: file.cli.agent,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::{ConfigDefaults, DEFAULT_AGENT, DEFAULT_DB_FILENAME};

    // `std::env::set_var` mute un état process-global : sérialise les tests
    // de ce module pour qu'ils ne s'entrelacent pas (même pattern que
    // `basemyai-core::storage::key` / `basemyai-cli::persisted_config`).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_isolated_env<F: FnOnce()>(home: &std::path::Path, f: F) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        // Sûr : `ENV_LOCK` garantit l'exclusivité mutuelle sur les variables
        // d'environnement touchées par ce test au sein du process de test.
        unsafe {
            std::env::set_var(home_var, home);
            std::env::remove_var("BASEMYAI_DB_PATH");
            std::env::remove_var("BASEMYAI_AGENT");
        }
        f();
        unsafe {
            std::env::remove_var(home_var);
            std::env::remove_var("BASEMYAI_DB_PATH");
            std::env::remove_var("BASEMYAI_AGENT");
        }
    }

    #[test]
    fn load_returns_empty_defaults_without_file_or_env() {
        let dir = tempfile::tempdir().expect("tempdir");
        with_isolated_env(dir.path(), || {
            let cfg = ConfigDefaults::load();
            assert!(cfg.db_path.is_none());
            assert!(cfg.agent.is_none());
        });
    }

    #[test]
    fn load_reads_file_then_env_overrides() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_dir = dir.path().join(".basemyai");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir");
        std::fs::write(
            cfg_dir.join("config.toml"),
            "[cli]\ndb_path = \"/from/file.bmai\"\nagent = \"file-agent\"\n",
        )
        .expect("write config");

        with_isolated_env(dir.path(), || {
            let cfg = ConfigDefaults::load();
            assert_eq!(cfg.db_path, Some(std::path::PathBuf::from("/from/file.bmai")));
            assert_eq!(cfg.agent.as_deref(), Some("file-agent"));

            // Sûr : `with_isolated_env` détient `ENV_LOCK` pour toute la durée
            // de cette closure — aucune course avec un autre test.
            unsafe {
                std::env::set_var("BASEMYAI_AGENT", "env-agent");
            }
            let cfg = ConfigDefaults::load();
            assert_eq!(
                cfg.db_path,
                Some(std::path::PathBuf::from("/from/file.bmai")),
                "l'environnement ne renseigne pas db_path ici : le fichier reste la source"
            );
            assert_eq!(
                cfg.agent.as_deref(),
                Some("env-agent"),
                "l'environnement gagne sur le fichier"
            );
        });
    }

    #[test]
    fn resolve_open_falls_back_to_built_in_defaults() {
        let cfg = ConfigDefaults::default();
        assert_eq!(
            cfg.resolve_open_path(None),
            std::path::PathBuf::from(DEFAULT_DB_FILENAME)
        );
        assert_eq!(cfg.resolve_open_agent(None), DEFAULT_AGENT);
    }

    #[test]
    fn resolve_open_prefers_explicit_then_configured_then_default() {
        let cfg = ConfigDefaults {
            db_path: Some(std::path::PathBuf::from("/configured.bmai")),
            agent: Some("configured-agent".to_string()),
        };
        assert_eq!(
            cfg.resolve_open_path(Some(std::path::PathBuf::from("/explicit.bmai"))),
            std::path::PathBuf::from("/explicit.bmai"),
            "an explicit path always wins"
        );
        assert_eq!(
            cfg.resolve_open_path(None),
            std::path::PathBuf::from("/configured.bmai"),
            "falls back to the shared config when nothing explicit is given"
        );
        assert_eq!(
            cfg.resolve_open_agent(Some("explicit-agent".to_string())),
            "explicit-agent"
        );
        assert_eq!(cfg.resolve_open_agent(None), "configured-agent");
    }
}
