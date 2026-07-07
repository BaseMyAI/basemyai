# ADR-031 — Licence BUSL-1.1 unifiée sur tout le workspace

**Statut** : ✅ Accepted
**Date** : 2026-07-06
**Relation aux ADR existants** : **remplace ADR-029** (découpage open-core :
MIT/Apache-2.0 sur `basemyai-core`/`basemyai`/CLI/MCP/REST/bindings, BUSL-1.1
seulement sur `basemyai-engine`). Ne modifie aucun ADR technique — ADR-024 à
ADR-030 restent la référence pour ce qu'*est* le moteur natif et le
chiffrement. `TRADEMARK_POLICY.md` (introduit par ADR-029) reste en vigueur,
inchangé dans son principe.

## Contexte

ADR-029 posait un découpage open-core : `basemyai-core`/`basemyai` (et
surfaces CLI/MCP/REST/bindings) en MIT puis MIT OR Apache-2.0, réservant la
licence restrictive (BUSL-1.1) au seul `basemyai-engine`. Le raisonnement
tenait sur un postulat implicite : le risque à couvrir était limité au moteur
de stockage natif (le différenciateur technique), et l'objectif prioritaire
pour le reste était l'adoption la plus large possible.

Ce postulat a été explicitement contredit lors de la revue de cet ADR : le
risque réel identifié n'est pas "on peut prendre `basemyai-engine`", c'est
**"on peut prendre `basemyai-core`/`basemyai` — le produit lui-même — les
rebrander sous n'importe quel nom, et les revendre comme moteur de mémoire
concurrent"**. Sous MIT/Apache, c'est exactement permis : copier, modifier,
redistribuer, vendre, sans reverser quoi que ce soit, y compris en changeant
le nom pour échapper à `TRADEMARK_POLICY.md` (qui ne couvre que le nom, pas
le code).

Deux usages bien distincts, mélangés dans ADR-029 :

1. **Dépendance** — un tiers embarque `basemyai`/`basemyai-core` dans SON
   produit (agent IA, outil interne) sans jamais le republier en tant que
   tel. C'est l'usage visé par la publication d'une lib, à préserver à tout
   prix.
2. **Fork-produit-concurrent** — un tiers prend le code source, le
   renomme, et le republie comme SON PROPRE moteur de mémoire pour agents.
   C'est ce qui doit être bloqué, et MIT/Apache ne le bloque pas.

Le mécanisme qui distingue déjà ces deux cas existe : c'est le test
fonctionnel de l'Additional Use Grant BUSL déjà rédigé pour
`basemyai-engine` dans ADR-029 (« fonction primaire du produit en aval »,
pas une clause anti-concurrence subjective). Il n'y a aucune raison
technique de le réserver au seul moteur — le même test protège aussi bien
`basemyai-core`/`basemyai` sans bloquer le cas 1.

## Décision

**Une seule licence, BUSL-1.1, sur tout le workspace** (`basemyai-core`,
`basemyai`, `basemyai-cli`, `basemyai-mcp`, `basemyai-rest`,
`basemyai-engine`, bindings Python/Node) — plus l'Additional Use Grant déjà
affiné pour couvrir explicitement les deux usages :

- **Autorisé sans permission** : dépendance/embarquement dans un produit
  tiers (même commercial, même fermé), usage interne, recherche, évaluation,
  et l'usage intra-écosystème (ForgeMyAI consommant `basemyai-core`).
- **Interdit sans licence commerciale** : (a) offrir le Licensed Work en
  service hébergé à des tiers ; (b) publier/vendre le Licensed Work — ou un
  fork substantiellement copié, sous n'importe quel nom — comme
  bibliothèque/SDK/produit dont la fonction primaire est de fournir du
  stockage mémoire d'agent, de la recherche vectorielle/graphe, ou de
  l'indexation de code à des tiers (i.e. devenir un substitut de
  BaseMyAI/ForgeMyAI, peu importe le nom utilisé).
- Conversion automatique en Apache-2.0 quatre ans après la publication de
  chaque version (inchangé par rapport à ADR-029) — le mécanisme de confiance
  qui évite l'accusation de verrouillage permanent reste entier.

Concrètement :

- `Cargo.toml` racine : `license = "BUSL-1.1"` (`workspace.package`),
  propagé à tous les crates via `license.workspace = true`
  — y compris `basemyai-engine`, qui n'a donc plus besoin d'un
  `license-file` séparé : `crates/basemyai-engine/LICENSE` est supprimé,
  consolidé dans le seul `LICENSE` racine.
- `LICENSE` racine réécrit : un seul texte BUSL-1.1, `Licensed Work` défini
  comme "tout le workspace", un seul Additional Use Grant pour tous les
  crates.
- `LICENSE-MIT`/`LICENSE-APACHE` (ajoutés par ADR-029) supprimés : on ne peut
  **pas** garder MIT/Apache comme option alternative en plus de BUSL — un
  double-licenciement "BUSL OR MIT" laisserait n'importe qui choisir les
  termes MIT et ignorer entièrement la restriction, ce qui viderait la BUSL
  de son sens. Le double-licenciement n'a de sens que quand toutes les
  options accordées sont voulues comme suffisantes ; ici une seule doit
  s'appliquer.
- En-têtes SPDX (introduits par ADR-029) : les 83 fichiers précédemment tagués
  `MIT OR Apache-2.0` repassent à `BUSL-1.1`, uniforme avec les 42 fichiers de
  `basemyai-engine` déjà tagués ainsi — 125 fichiers au total, un seul
  identifiant partout.
- `README.md`, `CONTRIBUTING.md`, `TRADEMARK_POLICY.md` mis à jour en miroir.

## Conséquences

- **Perte du statut "open source" au sens OSI** pour tout le workspace, pas
  seulement l'engine. BUSL-1.1 n'est pas approuvée OSI ; certaines
  entreprises ont une politique de conformité qui interdit toute dépendance
  non-OSI, indépendamment de la générosité de l'Additional Use Grant — coût
  accepté en connaissance de cause, priorité donnée à la protection contre le
  fork-produit-concurrent plutôt qu'à ce segment d'adoption entreprise.
- **Le `0.1.0` déjà publié sur crates.io/PyPI le 2026-06-22 reste MIT pour
  toujours** pour quiconque l'a déjà récupéré — cette décision protège les
  *futures* versions, elle ne peut pas et ne prétend pas retirer
  rétroactivement les droits déjà accordés sur cette version précise. C'est
  une limite structurelle du droit d'auteur (ADR-029 le notait déjà pour
  l'engine ; s'applique désormais aussi au reste du workspace).
- ForgeMyAI reste libre d'utiliser `basemyai-core` sans limite : couvert
  explicitement par l'Additional Use Grant (« usage intra-écosystème »), pas
  besoin d'exception séparée à négocier.
- `cargo xtask ci` non affecté : `license.workspace = true` partout, aucun
  changement de comportement de compilation/clippy. `basemyai-engine` reste
  `publish = false` ; les autres crates publiés changent de licence pour
  leurs *futures* versions seulement, aucun impact sur les tarballs déjà
  publiées.
- `TRADEMARK_POLICY.md` reste inchangé dans son principe (la marque est
  gouvernée indépendamment du code) mais ses références croisées vers
  `crates/basemyai-engine/LICENSE` deviennent des références vers le
  `LICENSE` racine unique.
- **Non couvert par cet ADR, décision humaine séparée** (inchangé
  d'ADR-029) : dépôt formel de la marque (USPTO/INPI), et provisionnement
  réel de `licensing@basemyai.com` (actuellement `security@basemyai.com` en
  contact temporaire dans `LICENSE` et `TRADEMARK_POLICY.md`).

## Alternatives rejetées

**Garder le découpage open-core d'ADR-029** — rejeté : ne protège pas contre
le risque réellement prioritaire (fork-produit-concurrent de
`basemyai-core`/`basemyai` eux-mêmes, pas seulement de l'engine). Le
découpage avait du sens *si* l'objectif était uniquement de protéger
l'engine ; ce n'est plus l'objectif énoncé.

**Double-licenciement BUSL OR MIT/Apache-2.0 sur le tout** — rejeté,
juridiquement incohérent avec l'objectif : "OR" laisse le choix à
l'utilisateur, qui choisirait alors systématiquement les termes les plus
permissifs (MIT) et ignorerait la restriction BUSL — cela reviendrait à ne
rien restreindre du tout, pire que ne rien faire puisque ça donnerait une
fausse impression de protection.

**AGPL-3.0 (copyleft fort) au lieu de BUSL** — rejeté : l'AGPL forcerait
même l'usage 1 (dépendance dans un produit tiers) à republier le code source
de ce produit tiers s'il est exposé en réseau — bien plus large que
nécessaire, casserait l'adoption comme dépendance embarquée que l'on veut
justement préserver. Le test fonctionnel de la BUSL cible précisément le cas
2 sans toucher au cas 1 ; l'AGPL ne fait pas cette distinction.

**Garder MIT/Apache-2.0 sur une version "lite" du core et BUSL sur le reste**
— envisagé puis écarté : maintenir deux bases de code (une lite ouverte, une
complète restreinte) est un coût d'ingénierie et de maintenance récurrent
disproportionné par rapport au problème (une seule licence avec un
Additional Use Grant bien rédigé distingue déjà dépendance vs. fork-concurrent
sans dupliquer aucun code).
