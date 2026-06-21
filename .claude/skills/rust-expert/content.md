# Skill: rust-expert — BaseMyAI / ForgeMyAI

## Stack et édition

- **Rust édition 2024** sur tous les crates du workspace.
- **`thiserror`** pour toutes les erreurs en lib ; **`anyhow`** interdit en lib (autorisé en binaire/test).
- **`#[non_exhaustive]`** sur tous les enums d'erreur publics.
- **Candle** (pas ONNX, pas fastembed) pour les embeddings.
- **libSQL** async (pas rusqlite, pas sqlx) + vecteur **natif** (`vector_top_k`, `F32_BLOB`).

## Gate qualité — doit passer avant tout commit

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Lint policy définie dans `[workspace.lints]` du `Cargo.toml` racine ; chaque crate hérite via `[lints] workspace = true`.

Lints activés clés :
- `clippy::unwrap_used` — **zéro `unwrap()`** en code lib
- `clippy::await_holding_lock` — **pas de `Mutex` std tenu à travers `.await`**
- `clippy::clone_on_ref_ptr` — **utiliser `Arc::clone(&x)` jamais `x.clone()`** sur un Arc
- `clippy::todo` — pas de `todo!()` en lib
- `expect_used` **non activé volontairement** : `expect("message explicatif")` est autorisé

Les tests sont exemptés de `unwrap_used` via `clippy.toml` (`allow-unwrap-in-tests = true`).

## Règles absolues du codebase

| Règle | Détail |
|-------|--------|
| Pas de `unwrap()` sans message en lib | `expect("raison")` OK, `unwrap()` → lint error |
| Pas de `static mut` | Utiliser `OnceLock`, `RwLock`, `Mutex` |
| Pas de `Mutex` std à travers `.await` | `tokio::sync::Mutex` si besoin, ou restructurer |
| `Arc::clone(&x)` explicite | Jamais `x.clone()` quand `x: Arc<T>` |
| `&str` en paramètre | Pas `String` quand on ne prend pas ownership |
| Getters sans préfixe `get_` | `fn name(&self) -> &str` pas `fn get_name()` |
| Chiffrement obligatoire dans `basemyai` | `Memory::open` échoue sans `EncryptionKey` |
| `Embedder` ne télécharge pas | Reçoit chemin + `Device` déjà résolus (ADR-010) |

## Invariant d'agnosticité de `basemyai-core`

```bash
# DOIT retourner zéro — si ce n'est pas le cas, c'est un bug d'architecture
grep -rE 'agent_id|valid_until|episodic|Symbol|Edge|semantic|graph|entity' \
  crates/basemyai-core/src
```

`basemyai-core` ne connaît **ni** la sémantique mémoire (`agent_id`, `valid_until`, couches) **ni** le code (`Symbol`, `Edge`). Tout mécanisme métier est *injecté* via traits.

## Patterns de code idiomatiques

### Erreur dans une lib

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    #[error("storage: {0}")]
    Storage(#[from] libsql::Error),
    #[error("embed: {0}")]
    Embed(String),
}
```

### Async avec libSQL — pas de Mutex std

```rust
// BON
let conn = self.store.conn().await?;
conn.execute("INSERT ...", params).await?;

// MAUVAIS — Mutex std tenu à travers .await
let guard = self.mutex.lock(); // std::sync::Mutex
let _ = some_async_op().await; // clippy::await_holding_lock
```

### Arc explicite

```rust
// BON
let embedder = Arc::clone(&self.embedder);

// MAUVAIS — clone_on_ref_ptr
let embedder = self.embedder.clone();
```

### Filter paramétré (anti-injection SQL)

```rust
// BON — inputs via params liés
let filter = Filter {
    sql: "agent_id = ?1 AND valid_until IS NULL".into(),
    params: vec![Value::Text(agent_id.to_string())],
};

// MAUVAIS — interpolation directe
let filter = Filter {
    sql: format!("agent_id = '{}'", agent_id), // SQL injection
    params: vec![],
};
```

## Layout du workspace

```
crates/
  basemyai-core/src/
    storage/    ← Store libSQL + Filter + Neighbor
    embed/      ← Device enum + trait Embedder + CandleEmbedder
    maintenance.rs
    error.rs    ← CoreError
    lib.rs

  basemyai/src/
    memory/     ← Memory façade, 4 couches, AgentId, schema SQL
    cognition/  ← LlmInference trait, consolidation, graph
    provision/  ← detect_hardware, provision, KNOWN_MODELS
    maintenance/← ConsolidationTask, GC, AdaptiveForgetting
    retrieval.rs ← RRF
    temporal.rs  ← Validity + temporal_filter
    error.rs     ← MemoryError
```

## Checks de review

Avant de valider une PR ou une suggestion :

1. `cargo clippy --workspace --all-targets -- -D warnings` passe
2. Zéro `unwrap()` en lib (sauf test avec `expect`)
3. Agnosticité core vérifiée si modification de `basemyai-core`
4. Chaque input externe passe par `params` lié (jamais interpolé)
5. Tout `Arc` partagé utilise `Arc::clone(&x)`
6. Pas de `tokio::spawn` sans handle géré ou `detach` documenté
