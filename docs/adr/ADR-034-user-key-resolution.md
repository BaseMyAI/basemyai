# ADR-034 — Résolution centralisée de la passphrase utilisateur (User Key Resolution)

**Statut** : ✅ Accepted  
**Date** : 2026-07-08  
**Relation** : amends ADR-007 (chiffrement obligatoire) ; complète ADR-030 (enveloppe DEK/KEK inchangée).

## Contexte

BaseMyAI exige une passphrase utilisateur à l'ouverture d'un conteneur `.bmai`
chiffré (ADR-007/ADR-030). La passphrase dérive une KEK qui scelle la DEK dans
`crypto.meta` — **cette architecture crypto n'est pas modifiée**.

Avant ADR-034, la DX était fragmentée :

- le CLI lisait surtout `BASEMYAI_DB_KEY` ;
- les bindings exigeaient `encryption_key` à chaque `open` ;
- les docs recommandaient parfois des valeurs placeholder (`change-me`) ;
- `~/.basemyai/config.toml` pouvait charger `db_key` / `api_key` côté REST/MCP.

Les standards OWASP recommandent de ne pas stocker les secrets dans le code ni
dans un TOML versionnable ; pour le local, un fichier dédié `chmod 600` ou une
variable d'environnement injectée au runtime sont acceptables (voir
`docs/security/key-resolution.md`).

## Décision

1. **Résolution unique** dans `basemyai_core::EncryptionKey::resolve` (ordre
   strict) :
   1. argument explicite (API / SDK) ;
   2. `BASEMYAI_DB_KEY` ;
   3. `BASEMYAI_ENCRYPTION_KEY` (alias legacy, documenté — pas de warning runtime) ;
   4. `BASEMYAI_DB_KEY_FILE` ;
   5. `/run/secrets/basemyai_db_key` si présent ;
   6. `~/.basemyai/key` si présent ;
   7. erreur typée `KeyResolveError::Missing` → `KEY_REQUIRED` (CLI exit 3).

2. **Fichier par défaut** `~/.basemyai/key` :
   - créé par `basemyai config key generate` (jamais d'affichage de la valeur) ;
   - permissions Unix : répertoire `~/.basemyai` ≤ `0700`, fichier ≤ `0600` —
     sinon erreur `KEY_INSECURE` avec hint `chmod` ;
   - **jamais** stocké dans `config.toml`.

3. **CLI** : `require_key()` délègue à `EncryptionKey::resolve(None)` ;
   sous-commandes `config key generate|path|check`.

4. **REST / MCP** : même résolution ; `[rest].db_key` et `[mcp].api_key` /
   `[rest].api_key` dans le TOML sont **ignorés** (warning stderr, TODO P1
   suppression du schéma).

5. **Bindings** : `encryption_key` optionnel — fallback sur la résolution ADR-034.

6. **Hors scope V1** : intégration OS keyring (Keychain / DPAPI / libsecret) —
   section V2 dans `docs/security/key-resolution.md`.

## Conséquences

- Meilleure DX locale sans affaiblir le chiffrement obligatoire.
- Une seule source de vérité pour toutes les surfaces.
- Les déploiements Docker peuvent monter `/run/secrets/basemyai_db_key`.
- Perte de `~/.basemyai/key` = perte irréversible des `.bmai` chiffrés (documenté).
