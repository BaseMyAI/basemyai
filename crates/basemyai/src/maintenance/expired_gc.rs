// SPDX-License-Identifier: BUSL-1.1
//! GC temporel (ADR-038) : supprime physiquement les souvenirs dont
//! `valid_until <= now` — la réintroduction native du mécanisme retiré par
//! ADR-033 (dépendait d'un `DELETE ... WHERE valid_until <= ?` SQL). Même
//! discipline que [`crate::maintenance::adaptive_forgetting`] (ADR-037) :
//! un scan applicatif borné plutôt qu'une requête fenêtrée, deux points
//! d'entrée qui partagent la même page mais divergent sur l'éviction.
//!
//! **Non-chevauchement avec l'oubli adaptatif.** Les deux mécanismes opèrent
//! sur des ensembles disjoints par construction :
//! [`crate::storage::MemoryStore::scan_for_forgetting`] ne renvoie que les
//! souvenirs **actifs** (`valid_until` `None` ou `> now`) ; ce module ne
//! considère que les souvenirs `valid_until <= now`. Un même souvenir ne peut
//! jamais être candidat aux deux passes à la fois.
//!
//! **Portée : par agent, jamais globale.** Il n'existe aujourd'hui aucune
//! primitive pour énumérer les agents d'un store (l'isolation est
//! structurelle par préfixe de clé, ADR-027 §2 — il n'y a pas de registre
//! d'agents à parcourir). Un passage "tous agents" nécessiterait une
//! primitive d'énumération inter-agent nouvelle, hors du périmètre de ce
//! portage ; voir `docs/adr/ADR-038-native-expired-memory-gc.md` §Conséquences.

use std::sync::Arc;

use basemyai_core::{MaintenanceTask, Result as CoreResult};

use crate::storage::MemoryStore;
use crate::{AgentId, Memory, Result, now_unix};

/// Page par défaut d'une passe de GC (`basemyai gc`, [`ExpiredMemoryGcTask`])
/// quand l'appelant n'en spécifie pas — assez grand pour amortir l'aller-retour
/// par page sur un usage courant, assez petit pour ne jamais matérialiser une
/// population non bornée en un seul scan (ADR-038).
pub const DEFAULT_GC_PAGE_SIZE: usize = 1_000;

/// Rapport d'une passe de GC temporel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExpiredGcReport {
    /// Nombre de souvenirs expirés examinés (toutes pages confondues).
    pub examined: usize,
    /// Nombre de souvenirs physiquement supprimés.
    pub deleted: usize,
    /// Nombre de pages parcourues (0 si rien n'était expiré).
    pub pages: usize,
}

/// Passe de GC temporel **sans `Memory`** : opère directement sur
/// [`MemoryStore`], sans charger l'embedder Candle — le chemin CLI
/// (`basemyai gc`), miroir de [`crate::maintenance::adaptive_forgetting::run`].
///
/// `dry_run = true` parcourt et compte les souvenirs expirés sans en
/// supprimer aucun — le curseur de pagination avance par id, pas par nombre
/// de lignes supprimées, donc la pagination reste correcte même quand rien
/// n'est effacé (contrairement à une pagination qui supposerait que chaque
/// page réduit la population restante).
///
/// N'émet aucun [`crate::MemoryEvent`] (contrairement à
/// [`crate::Memory::expired_gc`]) : un processus CLI one-shot n'a pas
/// d'abonné à qui les envoyer.
///
/// # Errors
/// [`crate::MemoryError::InvalidGcPageSize`] si `page_size == 0`. Propage
/// aussi les erreurs de stockage (scan ou suppression).
pub async fn run(
    store: &Arc<dyn MemoryStore>,
    agent: &AgentId,
    page_size: usize,
    dry_run: bool,
) -> Result<ExpiredGcReport> {
    if page_size == 0 {
        return Err(crate::MemoryError::InvalidGcPageSize);
    }
    let now = now_unix();
    let mut cursor: Option<String> = None;
    let mut examined = 0usize;
    let mut deleted = 0usize;
    let mut pages = 0usize;
    loop {
        let page = store.scan_expired(agent, now, cursor.as_deref(), page_size).await?;
        if page.is_empty() {
            break;
        }
        pages += 1;
        examined += page.len();
        if !dry_run {
            for candidate in &page {
                store.forget(agent, &candidate.id).await?;
                deleted += 1;
            }
        }
        let last_full_page = page.len() == page_size;
        cursor = page.last().map(|c| c.id.clone());
        if !last_full_page {
            break;
        }
    }
    Ok(ExpiredGcReport {
        examined,
        deleted,
        pages,
    })
}

/// Tâche de fond de GC temporel, injectable dans le `MaintenanceWorker`
/// agnostique du core. Auto-suffisante : possède sa propre [`Memory`] et sa
/// taille de page (même pattern qu'[`crate::maintenance::AdaptiveForgettingTask`]).
pub struct ExpiredMemoryGcTask {
    memory: Arc<Memory>,
    page_size: usize,
}

impl ExpiredMemoryGcTask {
    /// Construit la tâche à partir d'une mémoire partagée et d'une taille de
    /// page. `page_size == 0` est rejeté par [`Memory::expired_gc`] à
    /// l'exécution (une page vide ne progresserait jamais) — voir ses tests.
    #[must_use]
    pub fn new(memory: Arc<Memory>, page_size: usize) -> Self {
        Self { memory, page_size }
    }
}

#[async_trait::async_trait]
impl MaintenanceTask for ExpiredMemoryGcTask {
    fn name(&self) -> &str {
        "expired-memory-gc"
    }

    /// Lance une passe de GC temporel. Mappe [`crate::MemoryError`] vers
    /// [`basemyai_core::CoreError::Storage`] pour satisfaire l'interface du
    /// core.
    async fn run(&self) -> CoreResult<()> {
        self.memory
            .expired_gc(self.page_size)
            .await
            .map(|_| ())
            .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))
    }
}
