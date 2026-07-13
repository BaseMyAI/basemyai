// SPDX-License-Identifier: BUSL-1.1
//! Primitives internes de [`NativeInner`] : recherche vectorielle filtrée,
//! `touch` de `last_access`, insertion batch. Appelées par [`super::trait_impl`]
//! depuis l'intérieur des closures `with_inner`/`with_inner_read`
//! (`pub(super)` : visibles dans tout `native_store`, jamais hors du module).

use basemyai_engine::NewMemoryRecord;

use super::{NativeInner, OVERSAMPLE, record_valid_at, storage};
use crate::storage::NewMemory;
use crate::{AgentId, MemoryLayer, Result};

impl NativeInner {
    /// KNN oversamplé puis post-filtré : les (id, record, distance) des `k`
    /// plus proches souvenirs **de cet agent, valides à `now`**, couche
    /// optionnelle — la brique commune de tous les chemins vectoriels.
    pub(super) fn search_filtered(
        &self,
        agent: &AgentId,
        query: &[f32],
        k: usize,
        layer: Option<MemoryLayer>,
        now: i64,
        include_procedural: bool,
    ) -> Result<Vec<(String, basemyai_engine::MemoryRecord, f32)>> {
        let Self {
            engine,
            vectors,
            memory,
            ..
        } = self;
        let oversampled = k.saturating_mul(OVERSAMPLE);
        let hits = vectors.search_scored(engine, query, oversampled).map_err(storage)?;

        let mut out = Vec::with_capacity(k);
        for (vec_id, distance) in hits {
            let Some(mapping) = memory.resolve(engine, vec_id).map_err(storage)? else {
                // Id sans mapping : reliquat bénin d'un forget interrompu
                // (ADR-027 §3) — jamais un résultat.
                continue;
            };
            if mapping.agent != agent.as_str() {
                continue;
            }
            let Some(record) = memory.get(engine, &mapping.agent, &mapping.id).map_err(storage)? else {
                continue;
            };
            if !record_valid_at(&record, now) {
                continue;
            }
            if let Some(l) = layer
                && record.layer != l.table()
            {
                continue;
            }
            if !include_procedural && layer.is_none() && record.layer == MemoryLayer::Procedural.table() {
                continue;
            }
            out.push((mapping.id, record, distance));
            if out.len() == k {
                break;
            }
        }
        Ok(out)
    }

    /// Marque `last_access = now` sur `ids` (un seul batch atomique côté
    /// moteur, ids absents ignorés — parité `UPDATE` no-op).
    pub(super) fn touch(&mut self, agent: &AgentId, ids: &[String], now: i64) -> Result<()> {
        let Self { engine, memory, .. } = self;
        memory
            .touch_last_access(engine, agent.as_str(), ids.iter().map(String::as_str), now)
            .map_err(storage)
    }

    pub(super) fn put_one(&mut self, agent: &AgentId, item: &NewMemory<'_>) -> Result<()> {
        self.put_many(agent, std::slice::from_ref(item))
    }

    /// Insère plusieurs souvenirs de `agent` en **un seul** batch atomique
    /// (N5.5, `PersistentMemoryIndex::put_many`) : plus l'écart « atomique
    /// par item » d'ADR-027 §6 — un `put_memory_batch` natif est désormais
    /// UN enregistrement WAL, tout-ou-rien (équivalent sémantique d'une txn unique).
    pub(super) fn put_many(&mut self, agent: &AgentId, items: &[NewMemory<'_>]) -> Result<()> {
        let Self {
            engine,
            vectors,
            memory,
            fts,
            ..
        } = self;
        let entries: Vec<(&str, NewMemoryRecord<'_>, Vec<f32>)> = items
            .iter()
            .map(|item| {
                (
                    item.id.as_str(),
                    NewMemoryRecord {
                        layer: item.layer.table(),
                        content: item.text,
                        source: item.source,
                        valid_from: item.validity.valid_from,
                        valid_until: item.validity.valid_until,
                        importance: item.importance,
                        last_access: item.validity.valid_from,
                    },
                    item.vector.to_vec(),
                )
            })
            .collect();
        memory
            .put_many(engine, vectors, fts, agent.as_str(), &entries)
            .map_err(storage)?;
        Ok(())
    }
}
