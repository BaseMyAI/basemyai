# BaseMyAI — guide agent

Le guide agent canonique de ce repo est **[`CLAUDE.md`](CLAUDE.md)** : commandes
(`cargo xtask` reproduit la matrice CI), invariants à ne jamais violer, style
Rust, layout et statut. Lis-le en entier avant de travailler ici.

Note 2026-07-08 : le workspace est désormais **natif-only** (ADR-033) :
libSQL/V1/`crypto`/double-backend sont retirés du code actif.

Ce fichier n'est qu'un pointeur — ne pas dupliquer le contenu ici (les deux
copies avaient déjà divergé ; la double maintenance est volontairement abandonnée,
audit 2026-07-02).
