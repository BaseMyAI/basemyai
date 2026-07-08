# Skill: feature-dev — Scaffolding de features BaseMyAI

## Checklist avant de coder

1. **Y a-t-il un ADR qui couvre cette décision ?** Si la feature implique un choix architectural (nouveau backend, nouveau protocole, changement d'interface publique) → rédiger l'ADR d'abord.
2. **Dans quel crate ça va ?** Utiliser la matrice ci-dessous.
3. **Est-ce que ça viole l'agnosticité du core ?** Si oui, restructurer.
4. **Est-ce que les tests suivent ?** Minimum : test d'intégration en mémoire (`:memory:`).

## Matrice : où ajouter une feature

| Type de feature | Crate cible |
|----------------|-------------|
| Primitif de stockage/vecteur | `basemyai-core/storage/` |
| Embedder ou Device | `basemyai-core/embed/` |
| Tâche de maintenance générique | `basemyai-core/maintenance.rs` |
| Couche mémoire / RAG temporel | `basemyai/memory/` |
| Consolidation / graphe / LLM | `basemyai/cognition/` |
| Oubli / GC | `basemyai/maintenance/` |
| Provisioning modèle/hardware | `basemyai/provision/` |
| Tool MCP (remember, recall…) | `crates/basemyai-mcp/` |
| Binding Python | `bindings/basemyai-py/` |
| Binding Node | `bindings/basemyai-node/` |
| Sémantique code (symboles, graphe d'appel) | hors scope — consommateur tiers de `basemyai-core` |

## Pattern : ajouter une feature à `basemyai` (sémantique)

```
1. Ajouter une migration SQL dans basemyai/src/memory/schema.rs
   → nouvelle constante V4_MIGRATION (incrémente la version)
   
2. Implémenter la logique dans le module sémantique concerné
   → memory/, cognition/, maintenance/ selon le domaine
   
3. Exposer via la façade Memory (basemyai/src/memory/mod.rs)
   → méthode publique, docstring minimale
   
4. Test d'intégration dans crates/basemyai/tests/
   → utiliser Memory::open_in_memory(agent_id) (feature "test-util")
   → tester le round-trip (insert → query → verify)
   
5. Vérifier le gate clippy
   → cargo clippy --workspace --all-targets -- -D warnings
```

## Pattern : ajouter une primitive à `basemyai-core`

```
1. Définir le trait ou la struct dans le module approprié
   → storage/ pour Store/Filter, embed/ pour Embedder/Device
   
2. Implémenter l'injection (pas de logique métier dans le core)
   → le core expose "knn(q, k, filter?)", c'est tout
   → le sens (agent_id, valid_until) reste chez le consommateur
   
3. Test unitaire dans crates/basemyai-core/tests/
   → cargo test --workspace fonctionne sans CMake (sauf feature crypto)
   → cargo build -p basemyai-core --features embed (Candle lourd)
   
4. Vérifier l'agnosticité
   → grep -rE 'agent_id|valid_until|episodic|Symbol|Edge' crates/basemyai-core/src
   → doit retourner zéro
```

## Pattern : ajouter un outil MCP

```
1. Créer tools/<nom>.rs avec :
   - struct <Nom>Params (avec #[derive(serde::Deserialize, schemars::JsonSchema)])
   - struct <Nom>Result
   - fn handle(&self, p: <Nom>Params) -> Result<CallToolResult, McpError>
   
2. Enregistrer dans server.rs via #[tool_router]

3. Tronquer les résultats si > max_result_bytes (Config)

4. Audit via emit_audit(tool_name, agent_id, outcome, ms)
   → JAMAIS logger le contenu (seulement tool + outcome + durée)
   
5. Test round-trip avec Store::open_in_memory + embedder stub
```

## Scaffolding d'un nouveau crate dans le workspace

```toml
# Cargo.toml racine — ajouter dans [workspace] members
members = [
  "crates/basemyai-core",
  "crates/basemyai",
  "crates/basemyai-mcp",  # ← nouveau
]

# crates/nouveau/Cargo.toml
[package]
name = "basemyai-nouveau"
version = "0.1.0"
edition = "2024"

[lints]
workspace = true  # hérite du gate clippy

[dependencies]
basemyai = { path = "../basemyai" }
thiserror = { workspace = true }
tokio = { workspace = true, features = ["rt-multi-thread"] }
```

## Feature flags courants

| Flag | Effet | Prérequis |
|------|-------|-----------|
| `embed` | Active Candle (lourd) | – |
| `crypto` | Chiffrement libSQL | CMake + `cp` Git sur PATH (Windows) |
| `test-util` | `open_in_memory`, stubs pour tests | – |
| `stdio` | Transport stdio MCP | – |
| `http` | Transport HTTP MCP (axum) | – |

**Important** : le code `test-util` (constructeurs `open_in_memory`) doit être gardé par `#[cfg(feature = "test-util")]` **avec** sa registration (bloc `#[napi]`/`#[pymethods]` séparé, ou helper gardé) — sinon le build par défaut casse (E0425).

## Commandes de dev

```bash
# Build rapide (allégé, pas Candle)
cargo build

# Build avec embeddings
cargo build -p basemyai-core --features embed

# Build avec crypto (exige CMake)
cargo build -p basemyai-core --features crypto

# Tests async
cargo test --workspace

# Gate qualité
cargo clippy --workspace --all-targets -- -D warnings

# Profiling
cargo build --profile profiling -p basemyai-rest
```

## Tests — structure recommandée

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use basemyai::memory::Memory;

    #[tokio::test]
    async fn round_trip_remember_recall() {
        // Utiliser open_in_memory (feature "test-util") — pas de fichier, pas de CMake
        let mem = Memory::open_in_memory("test-agent").await.expect("open");
        
        mem.remember("test content", MemoryLayer::Episodic).await.expect("remember");
        let results = mem.recall("test content", 5).await.expect("recall");
        
        assert!(!results.is_empty());
    }
}
```
