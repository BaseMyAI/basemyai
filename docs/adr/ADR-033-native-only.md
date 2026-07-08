# ADR-033 — Migration 100 % moteur natif (libSQL/V1 retirés)

**Statut** : ✅ Accepted  
**Date** : 2026-07-08  
**Relation** : supersedes le mode compat libSQL décrit dans
`ADR-032-native-engine-default.md`, supersedes ADR-011/ADR-021, amends
ADR-019/ADR-020.

## Contexte

La bascule « moteur natif par défaut » est terminée et la phase de compatibilité
libSQL n'est plus maintenue dans le workspace actif :

- plus de backend libSQL côté runtime produit ;
- plus de `Store` / `LibsqlMemoryStore` / `basemyai_core::libsql` ;
- plus de feature `crypto` libSQL ni de job CI associé ;
- plus de feature de compatibilité `engine-native` : le natif est le chemin
  normal, compilé par défaut.

## Décision

1. **Backend unique** : BaseMyAI utilise uniquement le moteur natif
   `basemyai-engine` dans tout le workspace.
2. **Format actif** : `.bmai` est un conteneur natif (layout moteur natif) ;
   la compatibilité V1/libSQL est retirée de la base de code active.
3. **Chiffrement au repos** : assuré par l'enveloppe native ADR-030
   (XChaCha20-Poly1305, `crypto.meta`, WAL/SST chiffrés) ; aucun prérequis
   CMake.
4. **Contrats** : `MemoryStore` reste le contrat sémantique, avec
   `NativeMemoryStore` comme unique implémentation.

## Conséquences

- Simplification de la matrice CI/xtask (suppression `test-crypto` et des
  variantes `engine-native`).
- Suppression des reliquats SQL-leaky de la surface produit.
- Tests et examples exécutés sur le backend natif réel, y compris la variante
  chiffrée.
