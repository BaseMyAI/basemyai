// SPDX-License-Identifier: BUSL-1.1
//! Schéma SQL de la couche mémoire. C'est **ici** (et pas dans le core) que
//! vivent `agent_id`, `valid_from`/`valid_until` et la notion de couche : le
//! core ne connaît que des tables + un index vecteur natif.
//!
//! La table doit s'appeler `memory` pour que l'index natif `memory_idx` soit
//! retrouvé par `Store::vector_knn("memory", ...)` (lookup `'{table}_idx'`).

use basemyai_core::Migration;

/// Dimension des embeddings du baseline (`all-MiniLM-L6-v2`).
pub const EMBEDDING_DIM: usize = 384;
/// Version publique du conteneur `.bmai`.
pub const BMAI_FORMAT_VERSION: u32 = 1;

const BMAI_META_SCHEMA_V5: &str = "\
CREATE TABLE IF NOT EXISTS bmai_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
INSERT OR IGNORE INTO bmai_meta (key, value) VALUES
  ('format', 'basemyai-memory'),
  ('format_version', '1'),
  ('storage_engine', 'libsql'),
  ('schema_family', 'agent-memory'),
  ('embedding_dim', '384');
";

const MEMORY_SCHEMA_V1: &str = "\
CREATE TABLE IF NOT EXISTS memory (
  id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  layer TEXT NOT NULL,
  content TEXT NOT NULL,
  valid_from INTEGER NOT NULL,
  valid_until INTEGER,
  emb F32_BLOB(384)
);
CREATE INDEX IF NOT EXISTS memory_idx ON memory(libsql_vector_idx(emb, 'metric=cosine'));
";

/// Graphe entités/relations (VISION §4.1, Phase 2). **Tables + CTE récursives**
/// dans le même fichier libSQL — pas de Kuzu/Neo4j. Comme la mémoire, chaque
/// ligne porte `agent_id` (isolation ADR-006) et une fenêtre de validité
/// (`valid_from`/`valid_until`, ADR-005). C'est du *sens*, donc ici, pas dans le
/// core (test d'agnosticité préservé).
const GRAPH_SCHEMA_V2: &str = "\
CREATE TABLE IF NOT EXISTS entity (
  agent_id TEXT NOT NULL,
  id TEXT NOT NULL,
  kind TEXT NOT NULL,
  label TEXT NOT NULL,
  valid_from INTEGER NOT NULL,
  valid_until INTEGER,
  importance REAL NOT NULL DEFAULT 0,
  PRIMARY KEY (agent_id, id)
);
CREATE INDEX IF NOT EXISTS entity_agent_idx ON entity(agent_id);

CREATE TABLE IF NOT EXISTS edge (
  agent_id TEXT NOT NULL,
  src TEXT NOT NULL,
  dst TEXT NOT NULL,
  relation TEXT NOT NULL,
  weight REAL NOT NULL DEFAULT 1,
  valid_from INTEGER NOT NULL,
  valid_until INTEGER,
  PRIMARY KEY (agent_id, src, dst, relation)
);
CREATE INDEX IF NOT EXISTS edge_src_idx ON edge(agent_id, src);
";

/// Répare les premières bases qui avaient des clés graphe globales au lieu de
/// clés composites par agent. Sans ça, deux agents ne pouvaient pas partager le
/// même identifiant logique (`alice`) ou la même relation (`alice -> acme`).
const GRAPH_AGENT_SCOPED_KEYS_V6: &str = "\
CREATE TABLE IF NOT EXISTS entity_v6 (
  agent_id TEXT NOT NULL,
  id TEXT NOT NULL,
  kind TEXT NOT NULL,
  label TEXT NOT NULL,
  valid_from INTEGER NOT NULL,
  valid_until INTEGER,
  importance REAL NOT NULL DEFAULT 0,
  PRIMARY KEY (agent_id, id)
);
INSERT OR IGNORE INTO entity_v6 (agent_id, id, kind, label, valid_from, valid_until, importance)
  SELECT agent_id, id, kind, label, valid_from, valid_until, importance FROM entity;
DROP TABLE entity;
ALTER TABLE entity_v6 RENAME TO entity;
CREATE INDEX IF NOT EXISTS entity_agent_idx ON entity(agent_id);

CREATE TABLE IF NOT EXISTS edge_v6 (
  agent_id TEXT NOT NULL,
  src TEXT NOT NULL,
  dst TEXT NOT NULL,
  relation TEXT NOT NULL,
  weight REAL NOT NULL DEFAULT 1,
  valid_from INTEGER NOT NULL,
  valid_until INTEGER,
  PRIMARY KEY (agent_id, src, dst, relation)
);
INSERT OR IGNORE INTO edge_v6 (agent_id, src, dst, relation, weight, valid_from, valid_until)
  SELECT agent_id, src, dst, relation, weight, valid_from, valid_until FROM edge;
DROP TABLE edge;
ALTER TABLE edge_v6 RENAME TO edge;
CREATE INDEX IF NOT EXISTS edge_src_idx ON edge(agent_id, src);
";

/// Recherche **hybride** (ADR-014) : index full-text BM25 natif libSQL (FTS5) en
/// complément du vecteur. Les deux signaux sont fusionnés par RRF (`rrf_fuse`)
/// dans `recall_hybrid` — un terme exact que l'embedding rate (sigle, identifiant,
/// nom propre rare) remonte quand même par BM25.
///
/// Table **autonome** (pas external-content) : `id` et `agent_id` non indexés
/// (filtrage/jointure), seul `content` est tokenisé (`porter` = racinisation,
/// `remove_diacritics` = pliage des accents). Tenue à jour par la façade `Memory`
/// (insert au `remember`, delete au `forget`/`purge_agent`). Le `INSERT … SELECT`
/// **backfill** les souvenirs déjà présents lors de la migration.
const MEMORY_FTS_SCHEMA_V4: &str = "\
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
  id UNINDEXED,
  agent_id UNINDEXED,
  content,
  tokenize = 'porter unicode61 remove_diacritics 2'
);
INSERT INTO memory_fts (id, agent_id, content)
  SELECT id, agent_id, content FROM memory;
";

/// Oubli adaptatif (VISION §5.2, Phase 2). Ajoute à `memory` les deux signaux
/// nécessaires au score de rétention : `importance` (pondération métier, défaut
/// neutre `0`) et `last_access` (dernier accès Unix, *nullable* — le fallback
/// est `valid_from`). On **ajoute** une migration, on ne réécrit jamais un
/// schéma déjà appliqué. Les `INSERT` existants listent leurs colonnes
/// explicitement : les défauts garantissent leur compatibilité.
const MEMORY_SCHEMA_V3: &str = "\
ALTER TABLE memory ADD COLUMN importance REAL NOT NULL DEFAULT 0;
ALTER TABLE memory ADD COLUMN last_access INTEGER;
";

/// Provenance des faits (ADR-018 / audit sécurité, memory poisoning).
/// Distingue un souvenir mémorisé directement par l'agent (`'user'`, défaut)
/// d'un fait **promu par consolidation LLM** (`'consolidation'`) : ce dernier
/// a traversé une étape d'inférence sur du contenu potentiellement non fiable
/// (les épisodes), donc une confiance différente — l'escalade `episodic →
/// semantic` ne doit pas se faire silencieusement au même niveau de confiance
/// qu'un fait direct. On **ajoute** une colonne, on ne réécrit jamais un
/// schéma déjà appliqué (même pattern que `MEMORY_SCHEMA_V3`).
const MEMORY_SCHEMA_V7: &str = "\
ALTER TABLE memory ADD COLUMN source TEXT NOT NULL DEFAULT 'user';
";

/// Migrations de la couche mémoire + graphe, à passer à `Store::migrate`.
#[must_use]
pub fn schema() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            up_sql: MEMORY_SCHEMA_V1,
        },
        Migration {
            version: 2,
            up_sql: GRAPH_SCHEMA_V2,
        },
        Migration {
            version: 3,
            up_sql: MEMORY_SCHEMA_V3,
        },
        Migration {
            version: 4,
            up_sql: MEMORY_FTS_SCHEMA_V4,
        },
        Migration {
            version: 5,
            up_sql: BMAI_META_SCHEMA_V5,
        },
        Migration {
            version: 6,
            up_sql: GRAPH_AGENT_SCOPED_KEYS_V6,
        },
        Migration {
            version: 7,
            up_sql: MEMORY_SCHEMA_V7,
        },
    ]
}
