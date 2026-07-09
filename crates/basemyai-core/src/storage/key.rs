// SPDX-License-Identifier: BUSL-1.1
//! Résolution de la passphrase utilisateur (ADR-034) — indépendante du backend
//! crypto DEK/KEK (ADR-030). La clé n'est jamais loguée ; [`EncryptionKey`]'s
//! `Debug` reste masqué.

use std::fmt;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Chemin Docker secret standard (monté par l'orchestrateur).
pub const DOCKER_SECRET_PATH: &str = "/run/secrets/basemyai_db_key";

/// Clé de chiffrement, **fournie à l'ouverture, jamais persistée par le moteur**.
/// `Debug` masqué.
#[derive(Clone)]
pub struct EncryptionKey(String);

/// D'où [`EncryptionKey::resolve_with_source`] a lu la passphrase (diagnostic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeySource {
    /// Passée explicitement par l'appelant (SDK, API).
    Explicit,
    /// Variable d'environnement `BASEMYAI_DB_KEY`.
    EnvDbKey,
    /// Alias historique `BASEMYAI_ENCRYPTION_KEY` (ADR-034).
    EnvEncryptionKey,
    /// Fichier pointé par `BASEMYAI_DB_KEY_FILE`.
    KeyFileEnv,
    /// Fichier monté [`DOCKER_SECRET_PATH`].
    DockerSecret,
    /// Fichier par défaut `~/.basemyai/key`.
    DefaultKeyFile,
}

/// Échec de résolution ou de validation d'une passphrase.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum KeyResolveError {
    /// Aucune source utilisable.
    #[error("{0}")]
    Missing(String),

    /// Lecture du fichier impossible.
    #[error("reading encryption key file {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Fichier présent mais vide après trim.
    #[error("encryption key file {} is empty", path.display())]
    EmptyFile { path: PathBuf },

    /// Permissions Unix trop ouvertes sur `~/.basemyai` ou `~/.basemyai/key`.
    #[error("insecure permissions on {path}: mode {mode:#o} — {fix_hint}")]
    InsecurePermissions {
        path: PathBuf,
        mode: u32,
        fix_hint: &'static str,
    },
}

/// Résultat de [`EncryptionKey::resolve_with_source`].
#[derive(Debug, Clone)]
pub struct ResolvedKey {
    pub key: EncryptionKey,
    pub source: KeySource,
}

impl EncryptionKey {
    /// Wrap une passphrase. La valeur n'est jamais loguée ni affichée.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// Expose la passphrase brute — nécessaire pour ouvrir le moteur natif.
    /// À ne jamais loguer ni afficher.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Résout une passphrase sans exposer la provenance.
    ///
    /// # Errors
    /// [`KeyResolveError::Missing`] si aucune source n'est disponible.
    pub fn resolve(explicit: Option<&str>) -> Result<Self, KeyResolveError> {
        Ok(Self::resolve_with_source(explicit)?.key)
    }

    /// Résout une passphrase et indique d'où elle provient (ADR-034).
    ///
    /// Ordre : explicite → `BASEMYAI_DB_KEY` → `BASEMYAI_ENCRYPTION_KEY` →
    /// `BASEMYAI_DB_KEY_FILE` → [`DOCKER_SECRET_PATH`] → `~/.basemyai/key`.
    ///
    /// # Errors
    /// [`KeyResolveError`] si aucune source n'est utilisable.
    pub fn resolve_with_source(explicit: Option<&str>) -> Result<ResolvedKey, KeyResolveError> {
        if let Some(key) = trim_non_empty(explicit) {
            return Ok(ResolvedKey {
                key: Self::new(key),
                source: KeySource::Explicit,
            });
        }
        if let Ok(key) = std::env::var("BASEMYAI_DB_KEY")
            && let Some(key) = trim_non_empty(Some(key.as_str()))
        {
            return Ok(ResolvedKey {
                key: Self::new(key),
                source: KeySource::EnvDbKey,
            });
        }
        if let Ok(key) = std::env::var("BASEMYAI_ENCRYPTION_KEY")
            && let Some(key) = trim_non_empty(Some(key.as_str()))
        {
            return Ok(ResolvedKey {
                key: Self::new(key),
                source: KeySource::EnvEncryptionKey,
            });
        }
        if let Ok(path) = std::env::var("BASEMYAI_DB_KEY_FILE") {
            let path = PathBuf::from(path);
            let key = read_key_file(&path, false)?;
            return Ok(ResolvedKey {
                key: Self::new(key),
                source: KeySource::KeyFileEnv,
            });
        }
        let docker = Path::new(DOCKER_SECRET_PATH);
        if docker.is_file() {
            let key = read_key_file(docker, false)?;
            return Ok(ResolvedKey {
                key: Self::new(key),
                source: KeySource::DockerSecret,
            });
        }
        if let Some(path) = default_key_file_path()
            && path.is_file()
        {
            let key = read_key_file(&path, true)?;
            return Ok(ResolvedKey {
                key: Self::new(key),
                source: KeySource::DefaultKeyFile,
            });
        }
        Err(KeyResolveError::Missing(missing_key_message()))
    }

    /// Chemin du fichier de clé par défaut (`~/.basemyai/key`).
    #[must_use]
    pub fn default_key_file_path() -> Option<PathBuf> {
        default_key_file_path()
    }

    /// Répertoire `~/.basemyai`.
    #[must_use]
    pub fn default_config_dir() -> Option<PathBuf> {
        default_config_dir()
    }

    /// Génère une passphrase aléatoire (64 hex = 256 bits d'entropie).
    ///
    /// L'appelant ne doit jamais l'afficher sur stdout/stderr.
    #[must_use]
    pub fn generate_passphrase() -> String {
        use uuid::Uuid;
        format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
    }

    /// Écrit `key` dans `~/.basemyai/key` avec permissions restrictives (Unix).
    ///
    /// # Errors
    /// Erreur IO, home introuvable, ou fichier déjà présent sans `overwrite`.
    pub fn persist_to_default_file(key: &str, overwrite: bool) -> Result<PathBuf, std::io::Error> {
        let dir = default_config_dir()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve home directory"))?;
        let path = dir.join("key");
        persist_key_file(&dir, &path, key, overwrite)
    }
}

impl fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncryptionKey(***)")
    }
}

/// Libellé humain d'une [`KeySource`] (sans révéler la clé).
#[must_use]
pub fn key_source_label(source: KeySource) -> &'static str {
    match source {
        KeySource::Explicit => "explicit argument",
        KeySource::EnvDbKey => "BASEMYAI_DB_KEY",
        KeySource::EnvEncryptionKey => "BASEMYAI_ENCRYPTION_KEY (legacy alias)",
        KeySource::KeyFileEnv => "BASEMYAI_DB_KEY_FILE",
        KeySource::DockerSecret => DOCKER_SECRET_PATH,
        KeySource::DefaultKeyFile => "~/.basemyai/key",
    }
}

fn missing_key_message() -> String {
    let default = default_key_file_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.basemyai/key".to_string());
    format!(
        "encryption key required: pass it explicitly, set BASEMYAI_DB_KEY, \
         set BASEMYAI_DB_KEY_FILE, mount {DOCKER_SECRET_PATH}, create {default} \
         (run `basemyai config key generate`), or set BASEMYAI_ENCRYPTION_KEY (legacy)"
    )
}

fn trim_non_empty(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn trim_non_empty_owned(value: String) -> Option<String> {
    trim_non_empty(Some(value.as_str()))
}

fn read_key_file(path: &Path, check_default_permissions: bool) -> Result<String, KeyResolveError> {
    if check_default_permissions {
        validate_default_key_permissions(path)?;
    }
    let raw = std::fs::read_to_string(path).map_err(|source| KeyResolveError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    trim_non_empty_owned(raw).ok_or(KeyResolveError::EmptyFile {
        path: path.to_path_buf(),
    })
}

fn validate_default_key_permissions(key_path: &Path) -> Result<(), KeyResolveError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let Some(dir) = key_path.parent() else {
            return Ok(());
        };
        let dir_meta = std::fs::metadata(dir).map_err(|source| KeyResolveError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let dir_mode = dir_meta.permissions().mode() & 0o777;
        if dir_mode & 0o077 != 0 {
            return Err(KeyResolveError::InsecurePermissions {
                path: dir.to_path_buf(),
                mode: dir_mode,
                fix_hint: "run: chmod 700 ~/.basemyai",
            });
        }

        let file_meta = std::fs::metadata(key_path).map_err(|source| KeyResolveError::Io {
            path: key_path.to_path_buf(),
            source,
        })?;
        let file_mode = file_meta.permissions().mode() & 0o777;
        if file_mode & 0o177 != 0 {
            return Err(KeyResolveError::InsecurePermissions {
                path: key_path.to_path_buf(),
                mode: file_mode,
                fix_hint: "run: chmod 600 ~/.basemyai/key",
            });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = key_path;
    }
    Ok(())
}

fn persist_key_file(dir: &Path, path: &Path, key: &str, overwrite: bool) -> Result<PathBuf, std::io::Error> {
    if path.exists() && !overwrite {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "{} already exists (use --force to replace, or back up the file first)",
                path.display()
            ),
        ));
    }
    create_restricted_dir(dir)?;
    let contents = format!("{}\n", key.trim());
    write_key_file_restricted(path, contents.as_bytes())?;
    Ok(path.to_path_buf())
}

fn create_restricted_dir(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

fn write_key_file_restricted(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
    }
}

fn default_config_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".basemyai"))
}

fn default_key_file_path() -> Option<PathBuf> {
    default_config_dir().map(|d| d.join("key"))
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn resolve_prefers_explicit_over_env() {
        let _lock = env_lock();
        let _db = EnvVarGuard::set("BASEMYAI_DB_KEY", Some("from-env"));
        let resolved = EncryptionKey::resolve_with_source(Some("explicit")).expect("resolve");
        assert_eq!(resolved.source, KeySource::Explicit);
        assert_eq!(resolved.key.expose(), "explicit");
    }

    #[test]
    fn resolve_prefers_env_over_file() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let key_path = dir.path().join("key");
        std::fs::write(&key_path, "file-key\n").expect("write");
        let _file = EnvVarGuard::set("BASEMYAI_DB_KEY_FILE", Some(key_path.to_str().expect("utf8")));
        let _enc = EnvVarGuard::set("BASEMYAI_ENCRYPTION_KEY", None);
        let _db = EnvVarGuard::set("BASEMYAI_DB_KEY", Some("env-wins"));
        let resolved = EncryptionKey::resolve_with_source(None).expect("resolve");
        assert_eq!(resolved.source, KeySource::EnvDbKey);
        assert_eq!(resolved.key.expose(), "env-wins");
    }

    #[test]
    fn resolve_reads_db_key_file_env() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("custom.key");
        std::fs::write(&path, "file-key\n").expect("write");
        let _db = EnvVarGuard::set("BASEMYAI_DB_KEY", None);
        let _enc = EnvVarGuard::set("BASEMYAI_ENCRYPTION_KEY", None);
        let _file = EnvVarGuard::set("BASEMYAI_DB_KEY_FILE", Some(path.to_str().expect("utf8")));
        let resolved = EncryptionKey::resolve_with_source(None).expect("resolve");
        assert_eq!(resolved.source, KeySource::KeyFileEnv);
        assert_eq!(resolved.key.expose(), "file-key");
    }

    #[test]
    fn resolve_reads_default_key_file() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let basemyai = dir.path().join(".basemyai");
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&basemyai)
                .expect("dir");
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(&basemyai).expect("dir");
        }
        let key_path = basemyai.join("key");
        write_key_file_restricted(&key_path, b"default-file-key\n").expect("write key");
        let _home = EnvVarGuard::set("HOME", Some(dir.path().to_str().expect("utf8")));
        #[cfg(windows)]
        let _profile = EnvVarGuard::set("USERPROFILE", Some(dir.path().to_str().expect("utf8")));
        let _db = EnvVarGuard::set("BASEMYAI_DB_KEY", None);
        let _enc = EnvVarGuard::set("BASEMYAI_ENCRYPTION_KEY", None);
        let _file = EnvVarGuard::set("BASEMYAI_DB_KEY_FILE", None);
        let resolved = EncryptionKey::resolve_with_source(None).expect("resolve");
        assert_eq!(resolved.source, KeySource::DefaultKeyFile);
        assert_eq!(resolved.key.expose(), "default-file-key");
    }

    #[test]
    fn resolve_rejects_empty_file() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("empty.key");
        std::fs::write(&path, "\n").expect("write");
        let _db = EnvVarGuard::set("BASEMYAI_DB_KEY", None);
        let _enc = EnvVarGuard::set("BASEMYAI_ENCRYPTION_KEY", None);
        let _file = EnvVarGuard::set("BASEMYAI_DB_KEY_FILE", Some(path.to_str().expect("utf8")));
        let err = EncryptionKey::resolve(None).expect_err("empty");
        assert!(matches!(err, KeyResolveError::EmptyFile { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_rejects_insecure_default_key_permissions() {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

        let _lock = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let basemyai = dir.path().join(".basemyai");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(&basemyai)
            .expect("dir");
        let key_path = basemyai.join("key");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&key_path)
            .expect("open");
        file.sync_all().ok();
        drop(file);
        std::fs::write(&key_path, b"secret\n").expect("write");

        let _home = EnvVarGuard::set("HOME", Some(dir.path().to_str().expect("utf8")));
        let _db = EnvVarGuard::set("BASEMYAI_DB_KEY", None);
        let _enc = EnvVarGuard::set("BASEMYAI_ENCRYPTION_KEY", None);
        let _file = EnvVarGuard::set("BASEMYAI_DB_KEY_FILE", None);

        let err = EncryptionKey::resolve(None).expect_err("insecure");
        match err {
            KeyResolveError::InsecurePermissions { fix_hint, .. } => {
                assert!(fix_hint.contains("chmod"));
            }
            other => panic!("expected InsecurePermissions, got {other:?}"),
        }

        let meta = std::fs::metadata(&key_path).expect("meta");
        assert_ne!(meta.permissions().mode() & 0o077, 0);
    }

    #[test]
    fn persist_refuses_overwrite_without_force() {
        let dir = tempfile::tempdir().expect("tempdir");
        let basemyai = dir.path().join(".basemyai");
        let key_path = basemyai.join("key");
        persist_key_file(&basemyai, &key_path, "first", false).expect("first write");
        let err = persist_key_file(&basemyai, &key_path, "second", false).expect_err("no overwrite");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn generate_passphrase_is_unique_and_non_empty() {
        let a = EncryptionKey::generate_passphrase();
        let b = EncryptionKey::generate_passphrase();
        assert!(!a.is_empty());
        assert_ne!(a, b);
    }
}
