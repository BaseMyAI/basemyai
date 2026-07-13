# `crates/basemyai/tests/` — carte des suites d'intégration

Chaque fichier `.rs` directement sous `tests/` est un binaire de test Cargo
distinct (`cargo test --test <nom>`). Cette page regroupe les ~24 fichiers par
domaine pour la navigation ; elle ne change ni les noms de fichiers ni la
structure — `.github/workflows/ci.yml` et `xtask/src/main.rs` référencent la
plupart de ces noms en dur, et certains sont cités tels quels dans des ADR
(qui ne se modifient jamais). Voir `cargo xtask ci` pour la matrice exacte.

## Mémoire & sémantique temporelle

Testent la façade `Memory` et la logique pure (temps, fusion de rang), pas un
backend concret.

- **`memory.rs`** — roundtrip remember/recall, isolation par agent,
  expiration temporelle, via un `Embedder` fake déterministe.
- **`contracts.rs`** — contrats de la sémantique mémoire : logique temporelle,
  isolation, couches, conversion d'erreur, assemblage par injection de
  dépendance.
- **`retrieval.rs`** — Reciprocal Rank Fusion (`rrf_fuse`) : score, traçabilité
  des signaux contributeurs, tri déterministe, cas limites.
- **`temporal_dedup_consolidation.rs`** — `exact_fact_exists` respecte la
  validité temporelle (dédup consolidation).
- **`temporal_replacement_ci.rs`** — port CI de `examples/temporal_replacement.rs`
  (invalidation + recall).

## Storage natif & contrat multi-backend

Testent `MemoryStore`/`NativeMemoryStore` directement, sous la façade `Memory`.

- **`memory_tests.rs`** (+ `memory_tests/mod.rs`, `memory_tests/scenarios.rs`)
  — suite déclarative multi-backend (N2/N5.3) : un `Scenario` décrit une
  séquence d'opérations + postconditions, rejouée contre n'importe quelle
  implémentation de `MemoryStore` via `backend_suite!`. C'est la suite de
  référence pour le contrat du trait (a remplacé `storage_contract.rs`,
  supprimé une fois portée à 100 %).
- **`format.rs`** — contrat de métadonnées du conteneur `.bmai` (ADR-033).
- **`plaintext_open_forbidden.rs`** — `open` persistant en clair n'existe
  qu'avec `test-util` ; la prod passe par `open_encrypted` (ADR-030).
- **`native_memory_store_bench.rs`** — bench KNN via le chemin complet
  `MemoryStore::recall_vector` (N5.5), distinct des bench bruts de
  `basemyai-engine` (`vector_bench`/`vector_recall` sur `PersistentVectorIndex`).

## Isolation adversariale & sécurité

Chaque fichier prouve qu'un agent A ne peut jamais atteindre les données d'un
agent B, sur une surface donnée. Volontairement séparés par surface plutôt que
fusionnés (isolation du blast radius si une régression touche une seule
surface). *Note : le préfixe `p1_` sur `p1_isolation_adversarial.rs` est
incohérent avec le suffixe `_isolation_adversarial` des autres — non corrigé
ici car le nom est en dur dans `ci.yml`, `xtask/src/main.rs`, et cité dans au
moins un ADR.*

- **`p1_isolation_adversarial.rs`** — preuve d'isolation publique pour la
  différenciation marché P1 : `agent_id`/texte/requêtes FTS/ids
  hostiles, recall général.
- **`export_isolation_adversarial.rs`** — export JSONL : agent A ne doit
  jamais exporter les souvenirs de agent B.
- **`isolation_recall_graph_adversarial.rs`** — graph traverse : un agent ne
  doit pas atteindre les entités d'un autre agent.
- **`events.rs`** — preuve adversariale pour les abonnements mémoire en
  direct (`Memory::watch`) : une souscription ne délivre jamais les
  événements d'un autre agent.
- **`poisoning_procedural_recall.rs`** — la couche `procedural` est exclue du
  recall général par défaut (memory poisoning).
- **`provenance_trust.rs`** — provenance typée, anti-spoofing à l'import,
  filtre recall (ADR-036).

## Cognition (consolidation)

- **`consolidation.rs`** — fournisseur LLM fake déterministe (zéro réseau) :
  promotion des faits en `semantic`, peuplement du graphe, déduplication.
- **`consolidation_e2e.rs`** — E2E réel via AnythingLLM (ADR-016) ; nécessite
  `BASEMYAI_ANYTHINGLLM_KEY` et une instance active, sinon les tests se
  no-op/skip.

## Provisioning hardware-aware

- **`llm_provision.rs`** — provisioning LLM : logique pure toujours verte,
  détection réseau tolérante (liste vide si rien trouvé).
- **`provisioning.rs`** — provisioning de l'embedder ; aucun test ne
  télécharge (ADR-010). Nommé `provisioning` et non `setup` à dessein : un
  binaire `setup-*.exe` déclenche la détection d'installeur Windows (UAC).

## Maintenance

- **`maintenance_worker.rs`** — wiring `MaintenanceWorker` (M0.2) :
  `ConsolidationTask` et les tâches injectées tournent correctement via
  `MaintenanceTask`.

## Porting

- **`porting.rs`** — export/import JSONL : roundtrip complet (souvenirs +
  graphe + validité), idempotence, rejet des flux invalides.

## Surfaces externes

- **`testutil_facade.rs`** — façades pour consommateurs externes (MCP, REST,
  bindings), gated `test-util` : constructeur `:memory:` sans modèle,
  `remember` renvoyant l'UUID, accès graphe via `Memory::graph()`.

## Garanties infra

- **`zero_network_recall.rs`** — remember/recall ne nécessitent aucun socket
  une fois l'embedder fourni.

## Helpers partagés

- **`support/mod.rs`** — ouverture de store natif en mémoire, embedder fake,
  utilitaires communs importés via `mod support;` dans la plupart des
  fichiers ci-dessus.
