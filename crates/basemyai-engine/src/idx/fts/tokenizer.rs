//! Native FTS tokenizer (N5.2, ADR-028 §2).
//!
//! Replicates, without a new dependency, the two things SQLite FTS5's
//! `porter unicode61 remove_diacritics 2` tokenizer (ADR-014) applies to
//! **both** indexed content and `MATCH` query tokens: Unicode-aware
//! lowercasing and diacritic folding. Splitting is identical to the
//! caller-side `fts_match_expr()` (`crates/basemyai/src/memory/mod.rs`):
//! break on any non-alphanumeric `char`.
//!
//! **Deliberately not implemented**: Porter (English) stemming. ADR-028 §2
//! documents this as an assumed, measured parity gap — not a silent one. A
//! query for `"chats"` will not match content containing only `"chat"` on
//! this backend, unlike libSQL's FTS5 path.
//!
//! [`fold`] is exposed separately from [`tokenize`] so the `match_expr`
//! parser ([`super::persistent`]) can fold query tokens that
//! `fts_match_expr()` already split and lowercased, without re-splitting
//! them.

/// Folds one diacritic character to its plain-ASCII base letter. A fixed
/// table over the Latin-1 Supplement + Latin Extended-A ranges most common
/// in French/European text — not full Unicode NFD decomposition (no
/// dependency provides that here, ADR-028 §2), and deliberately **not**
/// multi-char (`æ`, `œ`, `ß` fold to a single nearest letter, not `ae`/`oe`/`ss`)
/// to keep the mapping a total, allocation-free `char -> char` function.
/// Characters outside the table pass through unchanged.
#[must_use]
pub fn fold_char(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' => 'i',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ß' => 's',
        'æ' => 'a',
        'œ' => 'o',
        other => other,
    }
}

/// Folds every character of `token` through [`fold_char`]. Does **not**
/// lowercase or split — callers that already have a lowercase, split token
/// (e.g. a `match_expr` term extracted by [`super::persistent`]) use this
/// directly; [`tokenize`] composes it with splitting and lowercasing for raw
/// document content.
#[must_use]
pub fn fold(token: &str) -> String {
    token.chars().map(fold_char).collect()
}

/// Tokenizes free text exactly like `fts_match_expr()` splits (break on any
/// `!char::is_alphanumeric`, drop empty pieces), Unicode-lowercases each
/// piece, then diacritic-folds it. Order preserved, duplicates kept — term
/// frequency is the caller's job ([`super::docterms::term_frequencies`]).
#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|piece| !piece.is_empty())
        .map(|piece| piece.chars().flat_map(char::to_lowercase).map(fold_char).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_non_alphanumeric_like_fts_match_expr() {
        assert_eq!(tokenize("chat, chien! (souris)"), vec!["chat", "chien", "souris"]);
    }

    #[test]
    fn lowercases_unicode() {
        assert_eq!(tokenize("CHAT Chien ÉCOLE"), vec!["chat", "chien", "ecole"]);
    }

    #[test]
    fn folds_common_french_diacritics() {
        assert_eq!(
            tokenize("réunion à l'école déjà"),
            vec!["reunion", "a", "l", "ecole", "deja"]
        );
    }

    #[test]
    fn drops_empty_pieces_from_runs_of_separators() {
        assert_eq!(tokenize("  chat   chien  "), vec!["chat", "chien"]);
    }

    #[test]
    fn empty_text_tokenizes_to_nothing() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("   !!! ,,,").is_empty());
    }

    #[test]
    fn keeps_duplicates_and_order() {
        assert_eq!(tokenize("chat chat chien"), vec!["chat", "chat", "chien"]);
    }

    #[test]
    fn non_latin_alphanumeric_passes_through_lowercased() {
        // CJK ideographs are `is_alphanumeric` in Rust's Unicode tables and
        // have no case, so they survive tokenization untouched.
        assert_eq!(tokenize("京都 réunion"), vec!["京都", "reunion"]);
    }

    #[test]
    fn fold_does_not_split_or_lowercase() {
        // `fold` is the query-side primitive: input is already split and
        // lowercased by `fts_match_expr()`, so it must not re-split.
        assert_eq!(fold("déjà-vu"), "deja-vu");
        assert_eq!(fold("ÉCOLE"), "ÉCOLE"); // uppercase untouched: not this fn's job
    }

    #[test]
    fn fold_char_is_total_and_leaves_ascii_alone() {
        for c in 'a'..='z' {
            assert_eq!(fold_char(c), c);
        }
        assert_eq!(fold_char('5'), '5');
    }
}
