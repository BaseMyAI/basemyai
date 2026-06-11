//! Store libSQL async + migrations + recherche vectorielle **native**.
//!
//! libSQL est SQLite-compatible et embarque le vecteur (`F32_BLOB`,
//! `libsql_vector_idx`, `vector_top_k`) — **aucune extension à linker**. Le
//! chiffrement au repos est intégré (feature `crypto`, qui exige CMake).

use std::fmt;
use std::path::{Path, PathBuf};

use libsql::{Builder, Connection, Database};

use crate::{CoreError, Filter, Neighbor, Result, Value};

/// Facteur de sur-échantillonnage du top-k natif quand un `Filter` est présent :
/// on demande `k * KNN_OVERSAMPLE` voisins avant d'appliquer le `WHERE`, pour
/// qu'il reste ~`k` résultats une fois filtrés.
const KNN_OVERSAMPLE: usize = 8;

/// Clé de chiffrement, **fournie à l'ouverture, jamais persistée**. `Debug` masqué.
#[derive(Clone)]
pub struct EncryptionKey(String);

impl EncryptionKey {
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncryptionKey(***)")
    }
}

/// Migration de schéma versionnée. Le **consommateur** déclare son schéma.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: u32,
    pub up_sql: &'static str,
}

/// Store libSQL. Garde une connexion partagée (libSQL synchronise l'accès en
/// interne) — nécessaire pour que les bases `:memory:` restent cohérentes.
pub struct Store {
    conn: Connection,
    path: Option<PathBuf>,
    encrypted: bool,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store").field("path", &self.path).field("encrypted", &self.encrypted).finish()
    }
}

impl Store {
    /// Ouvre (ou crée) un store sur fichier. `key = Some(_)` exige la feature
    /// `crypto` (chiffrement libSQL, nécessite CMake).
    ///
    /// # Errors
    /// [`CoreError::Encryption`] si une clé est fournie sans la feature `crypto`,
    /// [`CoreError::Storage`] si l'ouverture échoue.
    pub async fn open(path: &Path, key: Option<EncryptionKey>) -> Result<Self> {
        let encrypted = key.is_some();
        let db = build_local(path, key).await?;
        let conn = db.connect().map_err(map)?;
        Ok(Self { conn, path: Some(path.to_path_buf()), encrypted })
    }

    /// Ouvre un store en mémoire (tests).
    ///
    /// # Errors
    /// [`CoreError::Storage`] si l'initialisation échoue.
    pub async fn open_in_memory() -> Result<Self> {
        let db = Builder::new_local(":memory:").build().await.map_err(map)?;
        let conn = db.connect().map_err(map)?;
        Ok(Self { conn, path: None, encrypted: false })
    }

    /// Connexion partagée (clone). libSQL synchronise l'accès en interne.
    #[must_use]
    pub fn connect(&self) -> Connection {
        self.conn.clone()
    }

    /// Applique les migrations de version supérieure à la courante. Idempotent.
    ///
    /// # Errors
    /// [`CoreError::Storage`] en cas d'échec SQL.
    pub async fn migrate(&self, migrations: &[Migration]) -> Result<()> {
        let conn = self.connect();
        conn.execute_batch("CREATE TABLE IF NOT EXISTS _schema_version (version INTEGER NOT NULL);")
            .await
            .map_err(map)?;
        let current = scalar_i64(&conn, "SELECT COALESCE(MAX(version), 0) FROM _schema_version").await?;

        for m in migrations.iter().filter(|m| i64::from(m.version) > current) {
            conn.execute_batch(m.up_sql).await.map_err(map)?;
            conn.execute(
                "INSERT INTO _schema_version (version) VALUES (?1)",
                libsql::params![i64::from(m.version)],
            )
            .await
            .map_err(map)?;
        }
        Ok(())
    }

    /// Crée (si absent) la table + l'index vectoriel natif `metric=cosine`.
    ///
    /// # Errors
    /// [`CoreError::Vector`] si `table` n'est pas un identifiant sûr, ou échec SQL.
    pub async fn ensure_vector_table(&self, table: &str, dim: usize) -> Result<()> {
        let table = ident(table)?;
        let conn = self.connect();
        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {table} (id TEXT PRIMARY KEY, emb F32_BLOB({dim}));\n\
             CREATE INDEX IF NOT EXISTS {table}_idx ON {table}(libsql_vector_idx(emb, 'metric=cosine'));",
        ))
        .await
        .map_err(map)?;
        Ok(())
    }

    /// Insère ou met à jour le vecteur d'un identifiant.
    ///
    /// # Errors
    /// [`CoreError::Vector`] si `table` est invalide, ou échec SQL.
    pub async fn vector_upsert(&self, table: &str, id: &str, vector: &[f32]) -> Result<()> {
        let table = ident(table)?;
        let conn = self.connect();
        conn.execute(
            &format!(
                "INSERT INTO {table} (id, emb) VALUES (?1, vector(?2)) \
                 ON CONFLICT(id) DO UPDATE SET emb = vector(?2)",
            ),
            libsql::params![id, vec_to_json(vector)],
        )
        .await
        .map_err(map)?;
        Ok(())
    }

    /// `k` plus proches voisins (cosine, ANN natif), sous le `filter` paramétré
    /// optionnel fourni par l'appelant.
    ///
    /// La distance retournée est la **distance cosinus réelle** (`[0, 2]`,
    /// `0` = identique), recalculée via `vector_distance_cos` — pas un placeholder.
    ///
    /// Le filtre s'applique *après* le top-k natif. Pour qu'il reste `k`
    /// résultats une fois filtrés, on **sur-échantillonne** le top-k natif d'un
    /// facteur [`KNN_OVERSAMPLE`] dès qu'un filtre est présent, puis on tronque à
    /// `k` après le `WHERE`. C'est best-effort (un filtre très sélectif sur un
    /// gros index peut encore rendre < `k`), mais ça couvre le cas courant sans
    /// boucle d'élargissement.
    ///
    /// # Errors
    /// [`CoreError::Vector`] si `table` est invalide, ou échec SQL.
    pub async fn vector_knn(
        &self,
        table: &str,
        query: &[f32],
        k: usize,
        filter: Option<&Filter>,
    ) -> Result<Vec<Neighbor>> {
        let table = ident(table)?;
        let conn = self.connect();

        let filtered = matches!(filter, Some(f) if !f.where_sql.is_empty());
        let inner_k = if filtered { k.saturating_mul(KNN_OVERSAMPLE) } else { k };

        // Placeholders anonymes, liés dans l'ordre textuel du SQL ci-dessous :
        //   1) query (distance dans le SELECT)  2) query (vector_top_k)
        //   3) inner_k (vector_top_k)            4..) params du filtre  N) k (LIMIT)
        let mut params: Vec<libsql::Value> = vec![
            libsql::Value::Text(vec_to_json(query)),
            libsql::Value::Text(vec_to_json(query)),
            libsql::Value::Integer(i64::try_from(inner_k).unwrap_or(i64::MAX)),
        ];
        let where_clause = match filter {
            Some(f) if !f.where_sql.is_empty() => {
                params.extend(f.params.iter().map(to_libsql_value));
                format!(" WHERE {}", f.where_sql)
            }
            _ => String::new(),
        };
        params.push(libsql::Value::Integer(i64::try_from(k).unwrap_or(i64::MAX)));

        let sql = format!(
            "SELECT t.id, vector_distance_cos(t.emb, vector(?)) AS dist \
             FROM vector_top_k('{table}_idx', vector(?), ?) AS v \
             JOIN {table} AS t ON t.rowid = v.id{where_clause} \
             ORDER BY dist LIMIT ?",
        );

        let mut rows = conn.query(&sql, params).await.map_err(map)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(map)? {
            #[allow(clippy::cast_possible_truncation)]
            let distance = row.get::<f64>(1).map_err(map)? as f32;
            out.push(Neighbor { id: row.get::<String>(0).map_err(map)?, distance });
        }
        Ok(out)
    }

    /// Chemin du fichier (`None` si en mémoire).
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// `true` si le store est chiffré.
    #[must_use]
    pub fn is_encrypted(&self) -> bool {
        self.encrypted
    }
}

#[cfg(feature = "crypto")]
async fn build_local(path: &Path, key: Option<EncryptionKey>) -> Result<Database> {
    let mut builder = Builder::new_local(path);
    if let Some(key) = key {
        let cfg = libsql::EncryptionConfig::new(libsql::Cipher::Aes256Cbc, key.expose().as_bytes().to_vec().into());
        builder = builder.encryption_config(cfg);
    }
    builder.build().await.map_err(map)
}

#[cfg(not(feature = "crypto"))]
async fn build_local(path: &Path, key: Option<EncryptionKey>) -> Result<Database> {
    if key.is_some() {
        return Err(CoreError::Encryption); // chiffrement = feature `crypto` (CMake)
    }
    Builder::new_local(path).build().await.map_err(map)
}

async fn scalar_i64(conn: &Connection, sql: &str) -> Result<i64> {
    let mut rows = conn.query(sql, ()).await.map_err(map)?;
    let row = rows.next().await.map_err(map)?.ok_or_else(|| CoreError::Storage("empty scalar query".into()))?;
    row.get::<i64>(0).map_err(map)
}

/// Valide un identifiant de table (anti-injection ; les noms ne sont pas paramétrables).
fn ident(s: &str) -> Result<&str> {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(s)
    } else {
        Err(CoreError::Vector(format!("invalid table identifier: {s:?}")))
    }
}

fn vec_to_json(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}

fn to_libsql_value(v: &Value) -> libsql::Value {
    match v {
        Value::Integer(i) => libsql::Value::Integer(*i),
        Value::Real(r) => libsql::Value::Real(*r),
        Value::Text(t) => libsql::Value::Text(t.clone()),
        Value::Blob(b) => libsql::Value::Blob(b.clone()),
        Value::Null => libsql::Value::Null,
    }
}

fn map(e: libsql::Error) -> CoreError {
    CoreError::Storage(e.to_string())
}
