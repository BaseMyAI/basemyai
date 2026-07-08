// SPDX-License-Identifier: BUSL-1.1
//! Abonnements mémoire en temps réel (ADR — *live subscriptions*).
//!
//! Mécanisme de domaine **propre à `basemyai`** (jamais dans `basemyai-core`,
//! qui ignore `agent_id` et les couches). Une écriture mémoire qui **commit**
//! émet un [`MemoryEvent`] sur un canal `tokio::sync::broadcast`. Les
//! consommateurs s'abonnent via [`Memory::watch`](crate::Memory::watch) et
//! reçoivent **uniquement** les événements de leur agent (et couche, si
//! filtrée) : l'isolation est appliquée **côté serveur**, dans
//! [`MemorySubscription::recv`], jamais déléguée à l'appelant.
//!
//! L'émission est *best-effort* : un `send` sans récepteur vivant renvoie
//! `Err` — c'est attendu (personne n'écoute), jamais propagé comme erreur.

use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;

use crate::MemoryLayer;

/// Capacité du canal de diffusion d'événements (par `Memory`). Au-delà, les
/// récepteurs lents perdent les plus anciens événements (`Lagged`) — toléré par
/// [`MemorySubscription::recv`], qui poursuit sans paniquer.
pub(crate) const DEFAULT_EVENT_CAPACITY: usize = 1024;

/// Nature d'une mutation mémoire diffusée aux abonnés.
///
/// `#[non_exhaustive]` : de nouveaux genres peuvent apparaître en minor ; les
/// `match` externes doivent inclure un bras `_ =>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MemoryEventKind {
    /// Un souvenir vient d'être mémorisé (`remember`/`remember_batch`).
    Remembered,
    /// Un souvenir vient d'être invalidé (`valid_until = now`).
    Invalidated,
    /// Un souvenir vient d'être physiquement effacé (`forget`).
    Forgotten,
    /// Un fait sémantique vient d'être promu par consolidation.
    Consolidated,
}

/// Événement émis **après** qu'une écriture mémoire a commité.
///
/// Porte l'`agent_id` propriétaire (clé d'isolation), le genre de mutation, la
/// couche concernée et l'`id` du souvenir/fait affecté.
#[derive(Debug, Clone)]
pub struct MemoryEvent {
    /// Agent propriétaire — la clé d'isolation. Seul cet agent reçoit l'événement.
    pub agent_id: String,
    /// Nature de la mutation.
    pub kind: MemoryEventKind,
    /// Couche concernée.
    pub layer: MemoryLayer,
    /// Identifiant du souvenir/fait affecté.
    pub id: String,
}

/// Abonnement isolé à un flux d'événements mémoire.
///
/// Obtenu via [`Memory::watch`](crate::Memory::watch). N'expose **jamais** le
/// `Receiver` brut : l'isolation par agent (et le filtre de couche optionnel)
/// est appliquée à l'intérieur de [`MemorySubscription::recv`], pas par
/// l'appelant. Un événement d'un autre agent (ou d'une autre couche, si
/// filtrée) n'est jamais livré.
#[derive(Debug)]
pub struct MemorySubscription {
    rx: Receiver<MemoryEvent>,
    agent_id: String,
    layer: Option<MemoryLayer>,
}

impl MemorySubscription {
    /// Construit l'abonnement (interne — passe par [`Memory::watch`]).
    pub(crate) fn new(rx: Receiver<MemoryEvent>, agent_id: String, layer: Option<MemoryLayer>) -> Self {
        Self { rx, agent_id, layer }
    }

    /// Prochain événement destiné à **cet** agent (et à la couche filtrée, le
    /// cas échéant). Les événements d'autres agents/couches sont écartés et
    /// **jamais** livrés (isolation). Renvoie `None` quand le canal est fermé
    /// (le `Memory` source — et tout `Sender` — a été détruit).
    ///
    /// Tolère le retard (`Lagged`) : si l'abonné consomme trop lentement et que
    /// des événements ont été perdus, `recv` reprend au plus récent disponible
    /// au lieu d'échouer.
    pub async fn recv(&mut self) -> Option<MemoryEvent> {
        loop {
            match self.rx.recv().await {
                Ok(ev) if self.delivers(&ev) => return Some(ev),
                // Autre agent ou autre couche → écarté (isolation).
                Ok(_) => continue,
                // Récepteur en retard : des événements ont été perdus. On
                // poursuit sur les suivants plutôt que d'échouer.
                Err(RecvError::Lagged(_)) => continue,
                // Plus aucun `Sender` : le flux est terminé.
                Err(RecvError::Closed) => return None,
            }
        }
    }

    /// `true` si l'événement appartient à cet agent et passe le filtre de couche.
    fn delivers(&self, ev: &MemoryEvent) -> bool {
        ev.agent_id == self.agent_id && self.layer.is_none_or(|l| l == ev.layer)
    }
}
