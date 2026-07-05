//! Point d'entrée du runner de tests **déclaratifs multi-backend** (N2,
//! `docs/TODO-NATIVE-ENGINE.md`). Rejoue `memory_tests::scenarios::all()`
//! contre chaque backend enregistré ci-dessous via `backend_suite!`.
//!
//! Aujourd'hui : un seul backend réel, `Libsql`. Brancher `Native` (dépendance
//! N3/N4, non commencés — voir `docs/TODO-NATIVE-ENGINE.md`) est mécanique :
//! implémenter `MemoryStore` pour lui, écrire une factory async équivalente à
//! `make_libsql_store`, puis décommenter/ajouter une ligne `backend_suite!`.
//! Aucune autre modification de ce fichier ni de `memory_tests/mod.rs` n'est
//! nécessaire — c'est précisément ce que la borne générique
//! `run_scenario<S: MemoryStore>` rend possible.

#[path = "memory_tests/mod.rs"]
mod memory_tests;

use basemyai::storage::LibsqlMemoryStore;
use basemyai_core::Store;
use memory_tests::run_scenario;

/// Backend `Libsql` frais (in-memory, migré) — une instance par scénario,
/// isolation totale même si deux scénarios partageaient un `agent` id.
async fn make_libsql_store() -> LibsqlMemoryStore {
    let store = Store::open_in_memory().await.expect("store in-memory ouvre");
    store.migrate(&basemyai::schema()).await.expect("migration");
    LibsqlMemoryStore::new(store)
}

/// Enregistre un backend : génère un `#[tokio::test]` qui rejoue **tous** les
/// scénarios de `memory_tests::scenarios::all()` contre une instance fraîche
/// du backend nommé `$backend`, construite par `$make`.
macro_rules! backend_suite {
    ($backend:ident, $make:expr) => {
        #[tokio::test]
        async fn $backend() {
            for scenario in memory_tests::scenarios::all() {
                let store = $make().await;
                run_scenario(&store, &scenario).await;
            }
        }
    };
}

backend_suite!(libsql, make_libsql_store);

/// Backend `Native` frais (répertoire temporaire jetable, supprimé au drop) —
/// une instance par scénario, comme `Libsql`. C'est ici que le diff
/// multi-backend promis au N2 se prouve : mêmes scénarios, même runner,
/// deux moteurs (ADR-027/N5.1).
#[cfg(feature = "engine-native")]
async fn make_native_store() -> basemyai::storage::NativeMemoryStore {
    basemyai::storage::NativeMemoryStore::open_ephemeral().expect("store natif éphémère ouvre")
}

#[cfg(feature = "engine-native")]
backend_suite!(native, make_native_store);
