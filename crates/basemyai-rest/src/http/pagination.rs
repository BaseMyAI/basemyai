// SPDX-License-Identifier: BUSL-1.1
//! Troncature best-effort des réponses de liste (`recall`, `recall_hybrid`,
//! `graph traverse`/`search`). Pas de curseur/pagination réelle aujourd'hui —
//! aucun endpoint actuel n'en a besoin (les listes sont déjà bornées par `k`/
//! `max_depth`) ; ce module ne borne que la taille sérialisée.

use serde::Serialize;

/// Tronque une liste sérialisable pour tenir sous `max_bytes` (best-effort) :
/// retire les derniers éléments, déjà les moins pertinents puisque triés par
/// score/profondeur. Renvoie `(éléments conservés, tronqué)`.
pub fn truncate_to_fit<T: Serialize>(mut items: Vec<T>, max_bytes: usize) -> (Vec<T>, bool) {
    let mut truncated = false;
    while !items.is_empty() {
        match serde_json::to_vec(&items) {
            Ok(bytes) if bytes.len() <= max_bytes => break,
            _ => {
                items.pop();
                truncated = true;
            }
        }
    }
    (items, truncated)
}

#[cfg(test)]
mod tests {
    use super::truncate_to_fit;

    #[test]
    fn keeps_everything_under_the_limit() {
        let items = vec!["a".to_string(), "b".to_string()];
        let (kept, truncated) = truncate_to_fit(items, 1024);
        assert_eq!(kept.len(), 2);
        assert!(!truncated);
    }

    #[test]
    fn drops_tail_items_over_the_limit() {
        let items: Vec<String> = (0..1000).map(|i| format!("item-{i}")).collect();
        let (kept, truncated) = truncate_to_fit(items, 64);
        assert!(kept.len() < 1000);
        assert!(truncated);
    }
}
