# ADR-003 — Candle pour l'inférence in-process

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Le RAG exige de transformer du texte en vecteurs. Trois familles de solutions : (a) appel à une API d'embedding cloud — exclu (zéro-cloud) ; (b) un runtime ONNX embarqué (fastembed/ort) — dépendance C lourde, fragile à compiler sur Windows (MSVC), toolchain externe ; (c) une inférence pure Rust.

BaseMyAI veut un binaire autonome, sans service ML séparé, qui se lie proprement sur les trois OS et se package en wheel/prebuild sans imposer de compilateur au client.

**Décision**

Inférence **in-process via Candle** (pur Rust). Modèle : `all-MiniLM-L6-v2` (384 dimensions). Pas d'ONNX, pas de fastembed.

```rust
trait Embedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn model_id(&self) -> &str;   // ex. "all-MiniLM-L6-v2"
    fn dim(&self) -> usize;       // 384
}
```

**L'`Embedder` n'auto-télécharge JAMAIS le modèle.** Il reçoit un **chemin local**. Le fetch (et sa vérification d'intégrité) est orchestré par le produit, jamais par le core — pour garantir « zéro réseau par défaut ». BaseMyAI cache le modèle dans `~/.basemyai/models/` après un fetch explicite ; ForgeMyAI le fetch uniquement pendant `fmyai setup`.

**Conséquences**

✅ Pur Rust : se lie proprement sur Linux/Windows/macOS, pas de toolchain ONNX/MSVC fragile.
✅ Inférence in-process : pas de service ML séparé, un seul binaire.
✅ 384 dims, compatible avec le `nomic-embed-text-v1.5` (384) côté ForgeMyAI.
✅ `model_id()` permet de détecter un changement de modèle et de régénérer les vecteurs.
⚠️ Candle est plus jeune qu'ONNX — couverture de modèles plus restreinte. Acceptable : un seul modèle visé en V1.
⚠️ Inférence ML embarquée = risque de fuite mémoire à surveiller (stress-test 1h, profiling).
⚠️ Modèles multiples / multilingues reportés en V2.

**Alternatives rejetées**

ONNX Runtime / fastembed — dépendance C lourde, compilation Windows fragile (c'est le risque produit n°1 dans l'écosystème), toolchain externe.

API d'embedding cloud (OpenAI, Cohere) — viole le zéro-cloud, latence réseau, coût.

Service ML Python séparé (sidecar sentence-transformers) — casse le « un seul binaire », deux runtimes à gérer.
