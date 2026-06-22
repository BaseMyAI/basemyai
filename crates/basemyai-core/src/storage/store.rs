//! Store libSQL async + migrations + recherche vectorielle **native**.
//!
//! libSQL est SQLite-compatible et embarque le vecteur (`F32_BLOB`,
//! `libsql_vector_idx`, `vector_top_k`) — **aucune extension à linker**. Le
//! chiffrement au repos est intégré (feature `crypto`, qui exige CMake).

use std::fmt;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use libsql::{Builder, Connection, Database, TransactionBehavior};
use tokio::sync::{Mutex, MutexGuard};

use super::{Filter, Metric, Neighbor, Value};
use crate::{CoreError, EngineCapabilities, Result, StorageEngine};

/// Taille par défaut du pool de connexions lecteur ouvert par [`Store::open`].
/// Les lectures (KNN) se répartissent en round-robin sur ces connexions ;
/// l'écriture reste sérialisée sur l'unique writer.
const DEFAULT_READ_POOL: usize = 4;

/// Facteur de sur-échantillonnage du top-k natif quand un `Filter` est présent :
/// on demande `k * KNN_OVERSAMPLE` voisins avant d'appliquer le `WHERE`, pour
/// qu'il reste ~`k` résultats une fois filtrés.
const KNN_OVERSAMPLE: usize = 8;

/// Sur-échantillonnage pour le re-classement par métrique non native
/// (euclidienne/hamming) : on récupère `k * RERANK_OVERSAMPLE` candidats cosinus
/// avant de trier en Rust sur la métrique demandée.
const RERANK_OVERSAMPLE: usize = 16;

/// Clé de chiffrement, **fournie à l'ouverture, jamais persistée**. `Debug` masqué.
#[derive(Clone)]
pub struct EncryptionKey(String);

impl EncryptionKey {
    /// Wrap une clé de chiffrement. La valeur n'est jamais loguée ni affichée.
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

/// Store libSQL avec **pool de lecteurs**. Un unique `writer` sérialisé (WAL +
/// `busy_timeout`) porte toutes les écritures ; un pool de `readers` (round-robin)
/// porte les lectures, qui se parallélisent sous WAL sans bloquer le writer. La
/// `Database` est conservée vivante pour pouvoir rouvrir des connexions.
///
/// **Dégénérescence `:memory:`** : chaque `db.connect()` sur une base en mémoire
/// ouvre une *base distincte*, donc impossible de pooler ; le store garde alors
/// l'unique writer partagé (libSQL synchronise l'accès en interne), `readers`
/// est vide et [`reader`](Self::reader) retombe sur le writer. Pas de PRAGMA WAL
/// en mémoire.
pub struct Store {
    /// Conservée vivante pour que les connexions (writer + pool) restent
    /// valides — `Connection` ne garde pas la `Database` en vie. Lue uniquement
    /// comme propriétaire ; jamais déréférencée après l'ouverture.
    #[allow(dead_code)]
    db: Database,
    /// L'unique connexion d'écriture, sérialisée via `write_lock`.
    writer: Connection,
    /// Pool de connexions lecteur (round-robin). **Vide** pour `:memory:`.
    readers: Vec<Connection>,
    /// Curseur round-robin sur `readers`.
    next: AtomicUsize,
    path: Option<PathBuf>,
    encrypted: bool,
    /// Sérialise les [`WriteTxn`] : deux `BEGIN` concurrents sur le writer
    /// s'imbriqueraient (erreur SQLite) sans ce verrou.
    write_lock: Mutex<()>,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("path", &self.path)
            .field("encrypted", &self.encrypted)
            .finish()
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
        Self::open_with(path, key, DEFAULT_READ_POOL).await
    }

    /// Comme [`open`](Self::open) mais avec une taille de pool lecteur explicite.
    /// `pool_size = 0` ⇒ aucune connexion lecteur (toutes les lectures retombent
    /// sur le writer, comme en mémoire).
    ///
    /// Toute l'ouverture (writer + lecteurs) se fait **séquentiellement sous le
    /// même `native_open_lock`** : la race native `sqlite3_open_v2` (voir le doc
    /// de [`native_open_lock`]) interdit deux ouvertures concurrentes.
    ///
    /// # Errors
    /// [`CoreError::Encryption`] si une clé est fournie sans la feature `crypto`,
    /// [`CoreError::Storage`] si l'ouverture échoue.
    pub async fn open_with(path: &Path, key: Option<EncryptionKey>, pool_size: usize) -> Result<Self> {
        let encrypted = key.is_some();
        let _guard = native_open_lock().lock().await;
        let db = build_local(path, key).await?;

        // Writer + PRAGMAs WAL (file-backed uniquement).
        let writer = db.connect().map_err(map)?;
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL;\n\
                 PRAGMA synchronous=NORMAL;\n\
                 PRAGMA busy_timeout=5000;",
            )
            .await
            .map_err(map)?;

        // Pool lecteur : ouvertures SÉQUENTIELLES, toujours sous le même guard.
        let mut readers = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let r = db.connect().map_err(map)?;
            r.execute_batch("PRAGMA busy_timeout=5000;").await.map_err(map)?;
            readers.push(r);
        }

        Ok(Self {
            db,
            writer,
            readers,
            next: AtomicUsize::new(0),
            path: Some(path.to_path_buf()),
            encrypted,
            write_lock: Mutex::new(()),
        })
    }

    /// Ouvre un store en mémoire (tests). **Dégénéré** : pas de pool (chaque
    /// `db.connect()` en mémoire est une base distincte), `readers` vide, pas de
    /// PRAGMA WAL. La `Database` est conservée vivante.
    ///
    /// # Errors
    /// [`CoreError::Storage`] si l'initialisation échoue.
    pub async fn open_in_memory() -> Result<Self> {
        let _guard = native_open_lock().lock().await;
        let db = Builder::new_local(":memory:").build().await.map_err(map)?;
        let writer = db.connect().map_err(map)?;
        Ok(Self {
            db,
            writer,
            readers: Vec::new(),
            next: AtomicUsize::new(0),
            path: None,
            encrypted: false,
            write_lock: Mutex::new(()),
        })
    }

    /// Connexion **writer** (clone) — pour les requêtes, préférer
    /// [`reader`](Self::reader). libSQL synchronise l'accès en interne ; les
    /// écritures ad hoc via cette connexion restent correctes et sérialisées
    /// (WAL + `busy_timeout`).
    #[must_use]
    pub fn connect(&self) -> Connection {
        self.writer.clone()
    }

    /// Connexion **lecteur** (clone) issue du pool, en round-robin. Si le pool
    /// est vide (`:memory:` ou `pool_size = 0`), retombe sur le writer. Les
    /// connexions SERIALIZED sont sûres à partager sans checkout.
    #[must_use]
    pub fn reader(&self) -> Connection {
        if self.readers.is_empty() {
            self.writer.clone()
        } else {
            let i = self.next.fetch_add(1, Ordering::Relaxed) % self.readers.len();
            self.readers[i].clone()
        }
    }

    /// Applique les migrations de version supérieure à la courante. Idempotent.
    ///
    /// # Errors
    /// [`CoreError::Storage`] en cas d'échec SQL.
    pub async fn migrate(&self, migrations: &[Migration]) -> Result<()> {
        let _migration = migration_lock().lock().await;
        let txn = self.begin_write().await?;
        txn.execute_batch("CREATE TABLE IF NOT EXISTS _schema_version (version INTEGER NOT NULL);")
            .await
            .map_err(map)?;
        let current = scalar_i64(&txn, "SELECT COALESCE(MAX(version), 0) FROM _schema_version").await?;

        for m in migrations.iter().filter(|m| i64::from(m.version) > current) {
            txn.execute_batch(m.up_sql).await.map_err(map)?;
            txn.execute(
                "INSERT INTO _schema_version (version) VALUES (?1)",
                libsql::params![i64::from(m.version)],
            )
            .await
            .map_err(map)?;
        }
        txn.commit().await?;
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
    /// facteur `KNN_OVERSAMPLE` (×8) dès qu'un filtre est présent, puis on tronque à
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
        let conn = self.reader();

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
            out.push(Neighbor {
                id: row.get::<String>(0).map_err(map)?,
                distance,
            });
        }
        Ok(out)
    }

    /// KNN avec **métrique explicite**.
    ///
    /// [`Metric::Cosine`] emprunte le chemin natif ([`vector_knn`](Self::vector_knn)).
    /// [`Metric::Euclidean`] / [`Metric::Hamming`] sur-échantillonnent les candidats
    /// cosinus (`k * RERANK_OVERSAMPLE`) puis les **re-classent en Rust** sur les
    /// vecteurs réels (rappel piloté par l'ANN cosinus, tri par la métrique cible).
    ///
    /// # Errors
    /// [`CoreError::Vector`] si `table` est invalide, ou échec SQL.
    pub async fn vector_knn_metric(
        &self,
        table: &str,
        query: &[f32],
        k: usize,
        filter: Option<&Filter>,
        metric: Metric,
    ) -> Result<Vec<Neighbor>> {
        match metric {
            Metric::Cosine => self.vector_knn(table, query, k, filter).await,
            Metric::Euclidean | Metric::Hamming => self.vector_knn_reranked(table, query, k, filter, metric).await,
        }
    }

    /// Re-classement en Rust : récupère les vecteurs des candidats cosinus
    /// sur-échantillonnés, recalcule la distance pour `metric`, trie, tronque à `k`.
    async fn vector_knn_reranked(
        &self,
        table: &str,
        query: &[f32],
        k: usize,
        filter: Option<&Filter>,
        metric: Metric,
    ) -> Result<Vec<Neighbor>> {
        let table = ident(table)?;
        let conn = self.reader();
        let inner_k = k.saturating_mul(RERANK_OVERSAMPLE);

        // Placeholders : 1) query (vector_top_k) 2) inner_k 3..) params du filtre.
        let mut params: Vec<libsql::Value> = vec![
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

        let sql = format!(
            "SELECT t.id, vector_extract(t.emb) AS v \
             FROM vector_top_k('{table}_idx', vector(?), ?) AS top \
             JOIN {table} AS t ON t.rowid = top.id{where_clause}",
        );

        let mut rows = conn.query(&sql, params).await.map_err(map)?;
        let mut scored: Vec<Neighbor> = Vec::new();
        while let Some(row) = rows.next().await.map_err(map)? {
            let id = row.get::<String>(0).map_err(map)?;
            let vtext = row.get::<String>(1).map_err(map)?;
            let candidate = parse_vector(&vtext);
            scored.push(Neighbor {
                id,
                distance: metric_distance(query, &candidate, metric),
            });
        }

        scored.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        Ok(scored)
    }

    /// Ouvre une [`WriteTxn`] : transaction d'écriture **sérialisée**
    /// (`BEGIN IMMEDIATE` + verrou writer interne). Toute écriture multi-tables
    /// du consommateur doit passer par ici pour être atomique — un échec en
    /// cours de route annule tout (rollback automatique au drop).
    ///
    /// # Errors
    /// [`CoreError::Storage`] si le `BEGIN` échoue.
    pub async fn begin_write(&self) -> Result<WriteTxn<'_>> {
        let writer = self.write_lock.lock().await;
        let txn = self
            .writer
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(map)?;
        Ok(WriteTxn { txn, _writer: writer })
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

impl StorageEngine for Store {
    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities::libsql(self.encrypted)
    }
}

/// Transaction d'écriture sérialisée, ouverte par [`Store::begin_write`].
///
/// Déréférence vers la [`Connection`] sous-jacente : les `execute`/`query`
/// passés dessus font partie de la transaction. Sans [`commit`](Self::commit),
/// le drop **annule** tout (rollback). Le verrou writer est tenu pendant toute
/// la vie de la transaction — les autres `WriteTxn` attendent.
pub struct WriteTxn<'a> {
    txn: libsql::Transaction,
    _writer: MutexGuard<'a, ()>,
}

impl WriteTxn<'_> {
    /// Valide la transaction et relâche le verrou writer.
    ///
    /// # Errors
    /// [`CoreError::Storage`] si le `COMMIT` échoue.
    pub async fn commit(self) -> Result<()> {
        self.txn.commit().await.map_err(map)
    }
}

impl Deref for WriteTxn<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        &self.txn
    }
}

/// Sérialise toutes les ouvertures natives (`sqlite3_open_v2` via libSQL), process-wide.
///
/// libSQL configure son threading SQLite (`SQLITE_CONFIG_SERIALIZED` +
/// `sqlite3_initialize`) derrière un `Once` interne au premier `Database::new` ; ce
/// verrou évite que deux ouvertures concurrentes touchent cette init globale en
/// même temps. **Insuffisant seul** contre la rare race native Windows observée à
/// haute concurrence de threads (`STATUS_ACCESS_VIOLATION`, voir
/// `RUST_TEST_THREADS=1` dans `.cargo/config.toml`) — celle-ci se reproduit même
/// avec ce verrou en place, donc ailleurs dans le runtime libSQL. Conservé pour
/// la sémantique correcte qu'il garantit en prod ; les ouvertures sont rares (une
/// par session), donc le coût est négligeable.
fn native_open_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Sérialise les migrations process-wide.
///
/// Chaque [`Store`] a son propre verrou writer, donc deux stores ouverts sur le
/// même fichier froid pourraient sinon créer `_schema_version` et appliquer le
/// même lot de DDL en parallèle. Le verrou global garde le chemin cold-open
/// simple et déterministe ; la transaction writer rend chaque lot tout-ou-rien.
fn migration_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
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
    let row = rows
        .next()
        .await
        .map_err(map)?
        .ok_or_else(|| CoreError::Storage("empty scalar query".into()))?;
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

/// Parse la sortie texte de `vector_extract` (`"[a,b,c]"`) en `Vec<f32>`.
fn parse_vector(s: &str) -> Vec<f32> {
    s.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .filter_map(|x| {
            let t = x.trim();
            if t.is_empty() { None } else { t.parse::<f32>().ok() }
        })
        .collect()
}

/// Distance entre `query` et `candidate` pour la métrique de re-classement.
/// `Cosine` n'emprunte pas ce chemin (fallback défensif neutre).
fn metric_distance(query: &[f32], candidate: &[f32], metric: Metric) -> f32 {
    match metric {
        Metric::Euclidean => query
            .iter()
            .zip(candidate)
            .map(|(x, y)| {
                let d = x - y;
                d * d
            })
            .sum::<f32>()
            .sqrt(),
        Metric::Hamming => {
            let mut differing = 0.0_f32;
            for (x, y) in query.iter().zip(candidate) {
                if x.is_sign_negative() != y.is_sign_negative() {
                    differing += 1.0;
                }
            }
            differing
        }
        Metric::Cosine => 1.0,
    }
}
