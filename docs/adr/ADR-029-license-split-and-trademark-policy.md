# ADR-029 — Découpage de licence (open-core) et politique de marque

**Statut** : ⛔ Superseded by [ADR-031](ADR-031-unified-busl-license.md)
**Date** : 2026-07-06
**Relation aux ADR existants** : ne modifie aucun ADR technique ; ADR-024 à
ADR-028 restent la référence pour ce qu'*est* le moteur natif. Cet ADR décide
seulement sous quelles conditions ce code peut être utilisé par des tiers.

## Contexte

Tout le workspace est aujourd'hui MIT, y compris `crates/basemyai-engine` — le
moteur de stockage natif construit depuis N1 (ADR-025) jusqu'à N5.2
(ADR-028) : LSM-tree maison, index vectoriel LM-DiskANN, graphe natif,
full-text BM25 natif. C'est le principal différenciateur technique du projet
(zéro dépendance externe, parité de comportement prouvée contre libSQL,
performance mesurée dans `docs/benchmarks/`).

Deux risques identifiés sans rapport avec la qualité du code lui-même :

1. **Marque** : rien n'empêche aujourd'hui un tiers de forker le dépôt et de
   republier sous le nom "BaseMyAI" ou "ForgeMyAI" un produit dérivé sans
   rapport avec le projet d'origine — MIT ne couvre que le droit d'auteur sur
   le code, jamais la marque.
2. **Moteur natif** : sous MIT pur, un acteur peut (a) embarquer
   `basemyai-engine` tel quel dans un produit concurrent fermé, ou (b) le
   proposer en service hébergé (SaaS) sans jamais reverser de contribution —
   la faille classique des licences permissives déjà rencontrée par
   MongoDB (SSPL), Elastic (ELv2), MariaDB (origine de la BUSL), CockroachDB,
   Sentry.

`basemyai-core` et `basemyai` (la façade sémantique mémoire) ne sont **pas**
concernés par le second risque : leur valeur est dans l'adoption la plus
large possible (écosystème, bindings, ForgeMyAI qui consomme `basemyai-core`
en dépendance Rust native) ; les restreindre casserait cet objectif sans
bénéfice, et casserait aussi l'attente créée par la publication crates.io/PyPI
`0.1.0` déjà faite sous MIT le 2026-06-22 (qui ne peut de toute façon pas être
rétroactivement relicenciée — n'importe qui garde le droit d'utiliser cette
version MIT pour toujours).

## Décision

**Open-core, deux licences dans le même workspace, plus une politique de
marque séparée des deux :**

### 1. `basemyai-core`, `basemyai`, `basemyai-cli`, `basemyai-mcp`,
   `basemyai-rest`, les bindings — passent de MIT seul à **double licence
   MIT OR Apache-2.0** (convention de l'écosystème Rust : Cargo, tokio,
   serde, rand).

Racine `LICENSE-MIT` (contenu identique à l'ancien `LICENSE`) +
`LICENSE-APACHE` ajoutés ; racine `LICENSE` devient un pointeur court vers les
deux plus la licence de l'engine et la politique de marque. Changement
**additif, sans risque** : "OR" ne retire aucun droit déjà accordé sous MIT
(y compris sur la version `0.1.0` déjà publiée) — il ajoute une option pour
les versions futures. Bénéfice concret : Apache-2.0 apporte une concession de
brevet explicite + une clause de rétorsion (quiconque attaque le projet en
justice sur un brevet couvrant ce code perd sa licence Apache), que MIT seul
n'a pas. `Cargo.toml` racine (`[workspace.package].license`) mis à jour ; tous
les crates qui héritent via `license.workspace = true` suivent
automatiquement.

### 2. `crates/basemyai-engine` passe sous **Business Source License 1.1**
   (BUSL-1.1), fichier `crates/basemyai-engine/LICENSE` propre à ce crate
   (`license-file`, plus `license.workspace = true` sur ce crate précis).

Choix de BUSL-1.1 plutôt que les alternatives considérées :

- **Additional Use Grant** rédigé pour couvrir exactement les deux menaces
  identifiées ci-dessus (fork produit concurrent + hébergement SaaS sans
  contribution), sans restreindre l'usage interne, l'usage commercial normal,
  ni l'usage par BaseMyAI/ForgeMyAI eux-mêmes. Le test est **fonctionnel, pas
  une clause anti-concurrence subjective** : il porte sur la fonction
  primaire du produit/service en aval (stockage mémoire d'agent, recherche
  vectorielle/graphe, indexation de code pour des tiers), pas sur un jugement
  de valeur du Licensor sur qui « compète ». C'est le reproche précis fait à
  des clauses comme celle de la SSPL — une clause ancrée sur un fait
  observable est plus défendable devant un juge et plus prévisible pour un
  utilisateur de bonne foi.
- **Change Date à 4 ans + Change License Apache-2.0** : la restriction n'est
  pas permanente — le code redevient pleinement open source par version,
  contrairement à une licence propriétaire classique. C'est un signal de
  confiance pour la communauté et évite l'accusation d' »open-washing«
  permanent.
- `basemyai-engine` a `publish = false` (jamais publié sur crates.io) et son
  seul contributeur de code à ce jour est l'auteur du projet (`git log --
  crates/basemyai-engine` : un seul auteur) — aucun conflit avec des droits
  de tiers, le changement est possible sans négociation de relicenciement.

### 3. Politique de marque indépendante des deux licences de code

`TRADEMARK_POLICY.md` (racine) : couvre les noms "BaseMyAI"/"ForgeMyAI" et
les assets sous `basemyai-branding/`. Explicite que ni MIT ni BUSL n'accordent
de droit sur la marque (vrai par défaut en droit, mais rendu explicite pour
éviter toute ambiguïté côté utilisateurs/contributeurs). Distingue usage libre
(mention factuelle, fork non rebrandé, redistribution à l'identique) de
l'usage soumis à permission (rebranding, produit concurrent, impression
d'affiliation officielle).

## Conséquences

- `crates/basemyai-engine/Cargo.toml` : `license.workspace = true` →
  `license-file = "LICENSE"` (le crate n'hérite plus la licence du
  workspace).
- Racine `TRADEMARK_POLICY.md` ajouté ; `README.md` §License mis à jour pour
  documenter le découpage par crate et pointer vers la politique de marque.
- `CONTRIBUTING.md` : ajout d'un DCO (Developer Certificate of Origin,
  sign-off `git commit -s`) — nécessaire pour garder une chaîne de titre
  claire sur les futures contributions à `basemyai-engine`, condition pour
  pouvoir continuer à faire évoluer ses termes (ou les assouplir) sans devoir
  retrouver chaque contributeur historique.
- `docs/status.md` : note de clôture de cet item.
- Aucun impact sur `cargo xtask ci` : `license-file` ne change ni le
  comportement de compilation ni celui de clippy ; `basemyai-engine` n'étant
  pas publié, aucune contrainte de validation SPDX crates.io ne s'applique.
- BaseMyAI/ForgeMyAI eux-mêmes restent libres d'utiliser `basemyai-engine`
  sans limite (l'Additional Use Grant ne restreint que les tiers).
- **Non couvert par cet ADR, décision humaine séparée** : dépôt effectif
  d'une marque (USPTO/INPI) — la politique déclare une intention
  d'application, l'enregistrement est ce qui lui donne une portée légale
  réelle contre un tiers, et reste hors scope ici (démarche administrative,
  pas une décision d'architecture).
- **En-têtes SPDX** (`SPDX-License-Identifier: MIT OR Apache-2.0` /
  `BUSL-1.1`) ajoutés en tête des fichiers source de chaque crate — nécessaire
  pour que les outils de scan de licence (FOSSA, scancode, etc.) détectent
  correctement le découpage par crate plutôt que de ne voir qu'un seul
  `LICENSE` racine et classer tout le repo sous une seule licence par erreur.
- Contact de licence commerciale/marque temporairement
  `security@basemyai.com` (objet explicite) — `licensing@basemyai.com`
  n'existe pas encore ; à créer, puis à remplacer dans
  `crates/basemyai-engine/LICENSE` et `TRADEMARK_POLICY.md`.

## Alternatives rejetées

**Tout le workspace en BUSL/SSPL/AGPL** — rejeté : casse l'objectif
d'adoption large de `basemyai-core`/`basemyai` (le point d'entrée de
l'écosystème, y compris pour ForgeMyAI) sans bénéfice supplémentaire, ces
crates n'étant pas la source du risque identifié.

**Relicencier rétroactivement tout ce qui est publié en MIT** — impossible
en droit : une version déjà distribuée sous MIT reste utilisable sous MIT à
perpétuité par quiconque l'a obtenue ; seules les versions futures d'un crate
donné peuvent changer de licence.

**Elastic License v2 (ELv2) au lieu de BUSL** — rejeté : ELv2 ne restreint
que l'offre en service hébergé, pas le fork-produit-concurrent (une des deux
menaces explicitement visées ici), et n'a pas de conversion automatique vers
une licence ouverte — moins aligné avec l'objectif de rester perçu comme un
projet qui redevient ouvert avec le temps.

**SSPL (MongoDB)** — rejeté : conçue pour forcer la publication de la
*stack de service complète* (jusqu'à l'infra) d'un hébergeur, jugée
disproportionnée et rarement reconnue comme "source-available" par les
distributions Linux et beaucoup d'entreprises (contrairement à BUSL, accepté
plus largement) ; le risque SaaS ici est couvert suffisamment par
l'Additional Use Grant de BUSL sans ce niveau d'agressivité.

**Ne rien faire côté marque (compter sur le droit commun seul)** — rejeté :
un document explicite coûte peu, clarifie les attentes pour la communauté
avant tout litige, et prépare le terrain pour un dépôt formel ultérieur.

**Garder `basemyai-core`/`basemyai` en MIT seul (pas de double licence)** —
rejeté : n'apporte aucun bénéfice par rapport à MIT OR Apache-2.0 (la
concession MIT reste intacte, le choix Apache-2.0 est juste une option en
plus pour l'utilisateur), s'écarte de la convention de facto de l'écosystème
Rust, et prive le projet de la clause de rétorsion brevet sans aucune
contrepartie.

**Abandonner complètement MIT pour `basemyai-core`/`basemyai` au profit d'une
licence plus restrictive** — rejeté : juridiquement possible pour les
futures versions (seul détenteur du copyright), mais casserait l'objectif
d'adoption qui est la raison d'être de garder ces crates permissifs (voir
Contexte) ; la double licence MIT/Apache-2.0 obtient le bénéfice recherché
(protection brevet) sans ce coût.
