// SPDX-License-Identifier: BUSL-1.1
//! Résolution de la passphrase utilisateur (ADR-034) — indépendante du backend
//! crypto DEK/KEK (ADR-030). La clé n'est jamais loguée ; [`EncryptionKey`]'s
//! `Debug` reste masqué.

use std::fmt;
use std::path::{Path, PathBuf};

use thiserror::Error;
use zeroize::Zeroizing;

/// Chemin Docker secret standard (monté par l'orchestrateur).
pub const DOCKER_SECRET_PATH: &str = "/run/secrets/basemyai_db_key";
/// Sélection explicite du mode du secret résolu. Absent = `raw-key` pour la
/// compatibilité des stores historiques ; `passphrase` active Argon2id.
pub(crate) const KEY_MODE_ENV: &str = "BASEMYAI_DB_KEY_MODE";

/// Mode d'interprétation du secret fourni à l'ouverture d'un store.
///
/// Les deux modes sont intentionnellement disjoints : une même séquence
/// d'octets ne doit jamais être interprétée tantôt comme une clé brute, tantôt
/// comme une passphrase étirée par Argon2id (ADR-042).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EncryptionKeyMode {
    /// Matériau de clé à haute entropie, traité directement par le moteur.
    RawKey,
    /// Passphrase humaine, dérivée avec Argon2id par le moteur.
    Passphrase,
}

/// Clé de chiffrement, **fournie à l'ouverture, jamais persistée par le moteur**.
/// `Debug` masqué et matériel zeroizé au drop.
#[derive(Clone)]
pub struct EncryptionKey {
    material: Zeroizing<String>,
    mode: EncryptionKeyMode,
}

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

    /// Mode demandé par [`KEY_MODE_ENV`] inconnu.
    #[error("invalid {KEY_MODE_ENV} value {value:?} (expected raw-key or passphrase)")]
    InvalidMode { value: String },

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
    /// Wrap une clé brute. La valeur n'est jamais loguée ni affichée.
    ///
    /// Conserve la sémantique historique de `new` : les stores existants
    /// restent ouverts en mode [`EncryptionKeyMode::RawKey`]. Utiliser
    /// [`Self::passphrase`] pour créer ou ouvrir un store Argon2id.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self::raw(key)
    }

    /// Wrap une clé brute à haute entropie.
    #[must_use]
    pub fn raw(key: impl Into<String>) -> Self {
        Self {
            material: Zeroizing::new(key.into()),
            mode: EncryptionKeyMode::RawKey,
        }
    }

    /// Wrap une passphrase humaine qui sera étirée avec Argon2id.
    #[must_use]
    pub fn passphrase(passphrase: impl Into<String>) -> Self {
        Self {
            material: Zeroizing::new(passphrase.into()),
            mode: EncryptionKeyMode::Passphrase,
        }
    }

    /// Expose le matériau de clé brut — nécessaire pour ouvrir le moteur natif.
    /// À ne jamais loguer ni afficher.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.material
    }

    /// Mode dans lequel le moteur doit interpréter ce secret.
    #[must_use]
    pub const fn mode(&self) -> EncryptionKeyMode {
        self.mode
    }

    /// Consomme cette clé et demande son interprétation comme passphrase,
    /// sans recopier le matériau secret.
    #[must_use]
    pub fn into_passphrase(mut self) -> Self {
        self.mode = EncryptionKeyMode::Passphrase;
        self
    }

    /// Résout une clé brute sans exposer la provenance.
    ///
    /// # Errors
    /// [`KeyResolveError::Missing`] si aucune source n'est disponible.
    pub fn resolve(explicit: Option<&str>) -> Result<Self, KeyResolveError> {
        Ok(Self::resolve_with_source(explicit)?.key)
    }

    /// Résout une clé brute et indique d'où elle provient (ADR-034).
    ///
    /// Ordre : explicite → `BASEMYAI_DB_KEY` → `BASEMYAI_ENCRYPTION_KEY` →
    /// `BASEMYAI_DB_KEY_FILE` → [`DOCKER_SECRET_PATH`] → `~/.basemyai/key`.
    ///
    /// # Errors
    /// [`KeyResolveError`] si aucune source n'est utilisable.
    pub fn resolve_with_source(explicit: Option<&str>) -> Result<ResolvedKey, KeyResolveError> {
        let mode = resolved_mode()?;
        if let Some(key) = trim_non_empty(explicit) {
            return Ok(ResolvedKey {
                key: Self::resolved(key, mode),
                source: KeySource::Explicit,
            });
        }
        if let Ok(key) = std::env::var("BASEMYAI_DB_KEY")
            && let Some(key) = trim_non_empty_owned(key)
        {
            return Ok(ResolvedKey {
                key: Self::resolved(key, mode),
                source: KeySource::EnvDbKey,
            });
        }
        if let Ok(key) = std::env::var("BASEMYAI_ENCRYPTION_KEY")
            && let Some(key) = trim_non_empty_owned(key)
        {
            return Ok(ResolvedKey {
                key: Self::resolved(key, mode),
                source: KeySource::EnvEncryptionKey,
            });
        }
        if let Ok(path) = std::env::var("BASEMYAI_DB_KEY_FILE") {
            let path = PathBuf::from(path);
            let key = read_key_file(&path, false)?;
            return Ok(ResolvedKey {
                key: Self::resolved(key, mode),
                source: KeySource::KeyFileEnv,
            });
        }
        let docker = Path::new(DOCKER_SECRET_PATH);
        if docker.is_file() {
            let key = read_key_file(docker, false)?;
            return Ok(ResolvedKey {
                key: Self::resolved(key, mode),
                source: KeySource::DockerSecret,
            });
        }
        if let Some(path) = default_key_file_path()
            && path.is_file()
        {
            let key = read_key_file(&path, true)?;
            return Ok(ResolvedKey {
                key: Self::resolved(key, mode),
                source: KeySource::DefaultKeyFile,
            });
        }
        Err(KeyResolveError::Missing(missing_key_message()))
    }

    fn resolved(material: Zeroizing<String>, mode: EncryptionKeyMode) -> Self {
        Self { material, mode }
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
    pub fn generate_passphrase() -> Self {
        use uuid::Uuid;
        Self::raw(format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()))
    }

    /// Écrit `key` dans `~/.basemyai/key` avec permissions restrictives (Unix).
    ///
    /// # Errors
    /// Erreur IO, home introuvable, ou fichier déjà présent sans `overwrite`.
    pub fn persist_to_default_file(key: &Self, overwrite: bool) -> Result<PathBuf, std::io::Error> {
        let dir = default_config_dir()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve home directory"))?;
        let path = dir.join("key");
        persist_key_file(&dir, &path, key, overwrite)
    }

    /// Comme [`Self::resolve_with_source`], mais si **aucune** source n'est
    /// disponible, génère une passphrase et la persiste dans le fichier par
    /// défaut au lieu d'échouer — exactement la recette de
    /// `basemyai config key generate`, déclenchée automatiquement.
    ///
    /// Réservé aux surfaces SDK "installe et utilise" (bindings Node/Python) :
    /// contrairement au téléchargement de modèle (ADR-010, réseau, coûteux,
    /// donc soumis à consentement explicite), générer une clé est une
    /// opération **locale, instantanée, hors-ligne** — exiger un geste CLI
    /// préalable pour ça est de la friction sans bénéfice de sécurité. La CLI
    /// elle-même garde son geste explicite ([`Self::resolve`]) : un outil
    /// d'ops qui gère potentiellement plusieurs stores/clés bénéficie de
    /// rester explicite, un premier appel SDK non.
    ///
    /// Ne régénère **jamais** une clé existante : seule
    /// [`KeyResolveError::Missing`] déclenche la génération. Toute autre
    /// erreur (permissions non sûres, fichier vide, mode invalide) est un
    /// vrai signal de sécurité et reste fatale, propagée telle quelle.
    ///
    /// Renvoie la clé, et `Some(path)` si elle vient d'être générée et
    /// persistée à l'instant (`None` si une source existante a été utilisée)
    /// — l'appelant est censé notifier ce chemin (la clé n'est récupérable
    /// nulle part ailleurs si ce fichier est perdu).
    ///
    /// # Errors
    /// Propage toute erreur de résolution autre que `Missing`, et toute
    /// erreur d'écriture lors de la persistance de la clé générée.
    pub fn resolve_or_generate(explicit: Option<&str>) -> Result<(Self, Option<PathBuf>), KeyResolveError> {
        match Self::resolve_with_source(explicit) {
            Ok(resolved) => Ok((resolved.key, None)),
            Err(KeyResolveError::Missing(_)) => {
                let key = Self::generate_passphrase();
                let path = Self::persist_to_default_file(&key, false).map_err(|source| KeyResolveError::Io {
                    path: default_key_file_path().unwrap_or_default(),
                    source,
                })?;
                Ok((key, Some(path)))
            }
            Err(other) => Err(other),
        }
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

fn resolved_mode() -> Result<EncryptionKeyMode, KeyResolveError> {
    let Some(value) = std::env::var_os(KEY_MODE_ENV) else {
        return Ok(EncryptionKeyMode::RawKey);
    };
    let value = value.to_string_lossy().trim().to_ascii_lowercase();
    match value.as_str() {
        "raw" | "raw-key" | "raw_key" => Ok(EncryptionKeyMode::RawKey),
        "passphrase" => Ok(EncryptionKeyMode::Passphrase),
        _ => Err(KeyResolveError::InvalidMode { value }),
    }
}

fn trim_non_empty(value: Option<&str>) -> Option<Zeroizing<String>> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|value| Zeroizing::new(value.to_owned()))
}

fn trim_non_empty_owned(value: String) -> Option<Zeroizing<String>> {
    let value = Zeroizing::new(value);
    trim_non_empty(Some(value.as_str()))
}

fn read_key_file(path: &Path, check_default_permissions: bool) -> Result<Zeroizing<String>, KeyResolveError> {
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
    #[cfg(windows)]
    {
        // CRYPTO-2 (BaseMyAI adversarial audit, 2026-07-22): this was a
        // complete no-op — no ACL verification at all on Windows, unlike
        // the Unix branch above. `write_key_file_restricted`/
        // `create_restricted_dir` now write a protected, single-grantee
        // DACL; re-verify it here on every read, the same "actively
        // re-validate, don't just trust what was written once" posture as
        // the Unix mode-bit check.
        let sid = crate::storage::key_acl::current_user_sid().map_err(|source| KeyResolveError::Io {
            path: key_path.to_path_buf(),
            source,
        })?;
        let restricted = crate::storage::key_acl::is_restricted_to_current_user(key_path, &sid).map_err(|source| {
            KeyResolveError::Io {
                path: key_path.to_path_buf(),
                source,
            }
        })?;
        if !restricted {
            return Err(KeyResolveError::InsecurePermissions {
                path: key_path.to_path_buf(),
                mode: 0,
                fix_hint: "the key file's ACL is not restricted to the current user only — \
                           recreate it (delete and let BaseMyAI regenerate it) or repair its \
                           permissions manually via Windows security settings",
            });
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = key_path;
    }
    Ok(())
}

fn persist_key_file(dir: &Path, path: &Path, key: &EncryptionKey, overwrite: bool) -> Result<PathBuf, std::io::Error> {
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
    let trimmed = key.expose().trim();
    let mut contents = Zeroizing::new(String::with_capacity(trimmed.len() + 1));
    contents.push_str(trimmed);
    contents.push('\n');
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
    #[cfg(windows)]
    {
        // CRYPTO-2: create, then restrict the DACL to the current user only
        // (PROTECTED — blocks inherited ACEs from whatever the parent
        // directory would otherwise contribute). Best-effort in the sense
        // that a failure here surfaces as this call's own `Err` (never
        // silently ignored) — `persist_key_file`'s caller sees it exactly
        // like any other I/O error creating the directory.
        std::fs::create_dir_all(dir)?;
        let sid = crate::storage::key_acl::current_user_sid()?;
        crate::storage::key_acl::restrict_to_current_user(dir, &sid)
    }
    #[cfg(not(any(unix, windows)))]
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
    #[cfg(windows)]
    {
        // CRYPTO-2: write, then restrict the DACL — same posture as
        // `create_restricted_dir` above. The file is written first (so its
        // content is exactly what the caller intended) and the ACL applied
        // immediately after, before this function returns — no window
        // where the caller could plausibly observe the file mid-write with
        // default permissions and treat it as done.
        std::fs::write(path, contents)?;
        let sid = crate::storage::key_acl::current_user_sid()?;
        crate::storage::key_acl::restrict_to_current_user(path, &sid)
    }
    #[cfg(not(any(unix, windows)))]
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

    /// CRYPTO-2 regression (positive case): a key file created through the
    /// normal `persist_key_file`/`resolve` path must end up with a DACL
    /// restricted to the current user only — verified by actually reading
    /// the ACL back via the Win32 APIs, not just asserting the call
    /// succeeded.
    #[cfg(windows)]
    #[test]
    fn default_key_file_and_dir_are_acl_restricted_to_current_user() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let basemyai = dir.path().join(".basemyai");
        let key_path = basemyai.join("key");
        persist_key_file(&basemyai, &key_path, &EncryptionKey::raw("acl-test-key"), false).expect("persist");

        let sid = crate::storage::key_acl::current_user_sid().expect("current user sid");
        assert!(
            crate::storage::key_acl::is_restricted_to_current_user(&key_path, &sid).expect("read key file ACL"),
            "the key file's DACL must be restricted to exactly the current user after persist_key_file"
        );
        assert!(
            crate::storage::key_acl::is_restricted_to_current_user(&basemyai, &sid).expect("read key dir ACL"),
            "the .basemyai directory's DACL must be restricted to exactly the current user after persist_key_file"
        );

        // And `validate_default_key_permissions` — the read-time
        // counterpart — must accept what the write path just produced.
        assert!(validate_default_key_permissions(&key_path).is_ok());
    }

    /// CRYPTO-2 regression (negative case): a key file whose ACL grants
    /// access beyond the current user (simulated here by re-sealing it to
    /// the well-known "Everyone" SID instead) must be rejected by
    /// `validate_default_key_permissions` with an explicit, typed error —
    /// never silently accepted, mirroring the Unix `chmod 644` regression
    /// test above.
    #[cfg(windows)]
    #[test]
    fn resolve_rejects_key_file_with_a_permissive_windows_acl() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let basemyai = dir.path().join(".basemyai");
        let key_path = basemyai.join("key");
        persist_key_file(&basemyai, &key_path, &EncryptionKey::raw("insecure-acl-key"), false).expect("persist");

        // Re-seal the key file's DACL to "Everyone" instead of the current
        // user — simulates a misconfigured/inherited ACL.
        let everyone = crate::storage::key_acl::everyone_sid_for_test();
        crate::storage::key_acl::restrict_to_current_user(&key_path, &everyone).expect("reseal ACL to Everyone");

        let _home = EnvVarGuard::set("HOME", Some(dir.path().to_str().expect("utf8")));
        let _profile = EnvVarGuard::set("USERPROFILE", Some(dir.path().to_str().expect("utf8")));
        let _db = EnvVarGuard::set("BASEMYAI_DB_KEY", None);
        let _enc = EnvVarGuard::set("BASEMYAI_ENCRYPTION_KEY", None);
        let _file = EnvVarGuard::set("BASEMYAI_DB_KEY_FILE", None);

        let err = EncryptionKey::resolve(None).expect_err("permissive ACL must be rejected");
        assert!(
            matches!(err, KeyResolveError::InsecurePermissions { .. }),
            "expected InsecurePermissions, got {err:?}"
        );
    }

    #[test]
    fn persist_refuses_overwrite_without_force() {
        let dir = tempfile::tempdir().expect("tempdir");
        let basemyai = dir.path().join(".basemyai");
        let key_path = basemyai.join("key");
        persist_key_file(&basemyai, &key_path, &EncryptionKey::raw("first"), false).expect("first write");
        let err =
            persist_key_file(&basemyai, &key_path, &EncryptionKey::raw("second"), false).expect_err("no overwrite");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn persist_trims_into_a_protected_buffer_and_writes_one_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let basemyai = dir.path().join(".basemyai");
        let key_path = basemyai.join("key");
        let key = EncryptionKey::raw("  protected secret\r\n");

        persist_key_file(&basemyai, &key_path, &key, false).expect("persist key");

        assert_eq!(std::fs::read(&key_path).expect("read key"), b"protected secret\n");
        assert_eq!(key.expose(), "  protected secret\r\n");
    }

    #[test]
    fn generate_passphrase_is_unique_and_non_empty() {
        let a: EncryptionKey = EncryptionKey::generate_passphrase();
        let b: EncryptionKey = EncryptionKey::generate_passphrase();
        assert!(!a.expose().is_empty());
        assert_ne!(a.expose(), b.expose());
        assert_eq!(a.mode(), EncryptionKeyMode::RawKey);
    }

    #[test]
    fn key_mode_is_explicit_and_new_preserves_raw_key_compatibility() {
        assert_eq!(EncryptionKey::new("legacy raw key").mode(), EncryptionKeyMode::RawKey);
        assert_eq!(EncryptionKey::raw("raw key").mode(), EncryptionKeyMode::RawKey);
        assert_eq!(
            EncryptionKey::passphrase("human secret").mode(),
            EncryptionKeyMode::Passphrase
        );
    }

    #[test]
    fn resolve_or_generate_creates_a_key_only_when_none_is_configured() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", Some(dir.path().to_str().expect("utf8")));
        #[cfg(windows)]
        let _profile = EnvVarGuard::set("USERPROFILE", Some(dir.path().to_str().expect("utf8")));
        let _db = EnvVarGuard::set("BASEMYAI_DB_KEY", None);
        let _enc = EnvVarGuard::set("BASEMYAI_ENCRYPTION_KEY", None);
        let _file = EnvVarGuard::set("BASEMYAI_DB_KEY_FILE", None);

        let (first, generated_at) = EncryptionKey::resolve_or_generate(None).expect("generates a key");
        let generated_at = generated_at.expect("no prior source: a key file must have been created");
        assert!(generated_at.is_file(), "the generated key is actually persisted");

        // Un second appel doit relire la clé fraîchement persistée, pas en
        // regénérer une autre : `generated_at` doit redevenir `None`.
        let (second, generated_again) = EncryptionKey::resolve_or_generate(None).expect("resolves the same key");
        assert!(
            generated_again.is_none(),
            "an existing key file must never be silently regenerated"
        );
        assert_eq!(first.expose(), second.expose());
    }

    #[test]
    fn resolve_or_generate_propagates_non_missing_errors() {
        let _lock = env_lock();
        let _db = EnvVarGuard::set("BASEMYAI_DB_KEY", None);
        let _mode = EnvVarGuard::set(KEY_MODE_ENV, Some("unexpected"));
        let err = EncryptionKey::resolve_or_generate(Some("secret")).expect_err("invalid mode is fatal");
        assert!(
            matches!(err, KeyResolveError::InvalidMode { .. }),
            "a real configuration error must never be papered over by auto-generation"
        );
    }

    #[test]
    fn resolve_mode_is_explicit_and_defaults_to_raw_key() {
        let _lock = env_lock();
        let _db = EnvVarGuard::set("BASEMYAI_DB_KEY", None);
        let _mode = EnvVarGuard::set(KEY_MODE_ENV, None);
        assert_eq!(
            EncryptionKey::resolve(Some("secret")).expect("default mode").mode(),
            EncryptionKeyMode::RawKey
        );

        unsafe { std::env::set_var(KEY_MODE_ENV, "passphrase") };
        assert_eq!(
            EncryptionKey::resolve(Some("secret")).expect("passphrase mode").mode(),
            EncryptionKeyMode::Passphrase
        );

        unsafe { std::env::set_var(KEY_MODE_ENV, "unexpected") };
        assert!(matches!(
            EncryptionKey::resolve(Some("secret")),
            Err(KeyResolveError::InvalidMode { .. })
        ));
    }
}
