# ADR-009 — Trois surfaces de binding + wheels précompilés

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Le marché des builders d'agents IA est majoritairement Python, secondairement JS/TS, et résiduellement d'autres langages (Go, Ruby). Le cœur de BaseMyAI est en Rust. Pour être adopté, il doit être consommable **idiomatiquement** dans ces langages — et son installation ne doit pas exiger un compilateur C/Rust chez le client, sous peine d'écraser le taux d'adoption.

**Décision**

Trois surfaces de binding au-dessus du **même** `basemyai` :

| Surface | Techno | Cible | Packaging |
|---|---|---|---|
| SDK Python | PyO3 | builders Python (LangChain, LlamaIndex) | **wheel précompilé** (`pip install basemyai`) |
| SDK Node | NAPI-RS | builders JS/TS | **prebuild précompilé** (`npm install basemyai`) |
| Sidecar REST | axum | Go, Ruby, autres langages | binaire autonome unique |

(La 4ᵉ surface, le crate Rust natif consommé par ForgeMyAI, vise `basemyai-core` et fait l'objet d'ADR-001 ; elle n'est pas un « binding ».)

**Wheels et prebuilds précompilés** : `pip install` / `npm install` ne doivent **jamais** exiger un compilateur chez le client. La compilation se fait en CI, par plateforme.

**Conséquences**

✅ Adoption frictionless : `pip install basemyai`, deux lignes de code, mémoire opérationnelle.
✅ Un seul cœur Rust ; les trois bindings n'en sont que des façades — cohérence garantie.
✅ Le sidecar REST couvre les langages sans binding natif, sans dupliquer la logique.
⚠️ Matrice de build à maintenir : (Linux/Windows/macOS) × (Python versions / Node versions). CI lourde.
⚠️ Les dépendances C (sqlite-vec, sqlcipher) doivent compiler sur toutes les cibles de la matrice → testées dès le 1ᵉʳ commit du core.
⚠️ Le sidecar REST réintroduit du réseau pour ses consommateurs (assumé : c'est leur seul moyen sans binding natif ; reste local/loopback par défaut).

**Alternatives rejetées**

Exiger un compilateur chez le client (`pip install` qui compile) — écrase l'adoption ; la plupart des utilisateurs Python n'ont pas de toolchain Rust.

Réécrire le cœur dans chaque langage — trois implémentations à maintenir, divergence garantie, perte du bénéfice Rust.

REST seul (pas de bindings natifs) — impose un serveur et du réseau à tout le monde, latence et complexité de déploiement pour le cas Python/JS qui est le marché principal.
