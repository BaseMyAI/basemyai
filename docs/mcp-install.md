# Brancher BaseMyAI sur votre agent (MCP)

`basemyai-mcp` donne à un agent IA une **mémoire persistante, locale et chiffrée**
via le **Model Context Protocol**. Il fonctionne avec tout hôte MCP : Claude Code,
Claude Desktop, Cursor, Windsurf, ChatGPT Desktop, Codex…

Une fois branché, l'agent dispose de 9 outils : `remember`, `recall`,
`recall_hybrid`, `recall_graph`, **`compile_context`**, `invalidate`, `stats`,
**`consolidate`** et **`consolidate_apply`**. `compile_context` compile un
recall hybride en contexte borné, tracé et prêt pour un prompt — déterministe,
sans LLM (Context Engine, R1.8). La consolidation (épisodes → faits + graphe)
suit une **politique à niveaux** (ADR-018) qui marche partout, **sans serveur
LLM ni clé** : voir §6.

---

## 1. Construire le binaire

```bash
cargo build --release -p basemyai-mcp --bin basemyai-mcp
# Binaire produit : target/release/basemyai-mcp(.exe)
```

> **Windows — build de la feature `crypto`.** libSQL chiffré (bug `libsql-ffi`
> 0.9.30) exige **CMake** et le **`cp` de Git** sur le PATH. Si le build échoue
> sur `copy_with_cp` (« Accès refusé ») :
>
> ```powershell
> $env:PATH = "C:\Program Files\Git\usr\bin;" + $env:PATH   # fournit `cp`
> cargo clean -p libsql-ffi                                  # purge un résidu bloquant
> cargo build --release -p basemyai-mcp --bin basemyai-mcp
> ```

---

## 2. Variables d'environnement

| Variable | Rôle | Requis |
| --- | --- | --- |
| Passphrase (ADR-034) | Voir [`docs/security/key-resolution.md`](security/key-resolution.md) : `BASEMYAI_DB_KEY`, `BASEMYAI_DB_KEY_FILE`, `/run/secrets/basemyai_db_key`, ou `~/.basemyai/key` | **Oui** |
| `BASEMYAI_FETCH` | `1` = consent au téléchargement du modèle d'embedding au 1ᵉʳ lancement. | 1ᵉʳ run seulement |
| `BASEMYAI_MCP_TRANSPORT` | `stdio` (défaut) ou `http`. | Non |
| `BASEMYAI_MCP_API_KEY` | Jeton Bearer — **requis** pour le transport HTTP. | HTTP seulement |
| `BASEMYAI_MCP_PORT` | Port HTTP (défaut `7744`, écoute `127.0.0.1`). | Non |

> **Modèle d'embedding** : au tout premier lancement, lancez une fois avec
> `BASEMYAI_FETCH=1` pour télécharger le modèle baseline (`all-MiniLM-L6-v2`,
> vérifié par SHA-256) dans `~/.basemyai/models`. Les lancements suivants n'ont
> plus besoin de cette variable — **zéro download silencieux** (ADR-010).

---

## 3. Claude Code (stdio)

```bash
# Générer une passphrase locale (recommandé dev) — jamais affichée
basemyai config key generate

claude mcp add basemyai \
  --env BASEMYAI_FETCH=1 \
  -- /chemin/absolu/target/release/basemyai-mcp
```

Si vous préférez une variable d'environnement explicite, utilisez
`BASEMYAI_DB_KEY` (voir [key-resolution.md](security/key-resolution.md)) —
**ne pas** utiliser de placeholder type `change-me` en production.

Exemple avec env var :

```bash
claude mcp add basemyai \
  --env BASEMYAI_DB_KEY="$BASEMYAI_DB_KEY" \
  --env BASEMYAI_FETCH=1 \
  -- /chemin/absolu/target/release/basemyai-mcp
```

Vérifiez : `claude mcp list` doit montrer `basemyai`. Dans une session, demandez
à Claude d'utiliser l'outil `remember`, puis `recall` dans une session ultérieure.

---

## 4. Claude Desktop / Cursor / Windsurf (config JSON)

Ajoutez à la config MCP de l'hôte (ex. `claude_desktop_config.json`) :

```json
{
  "mcpServers": {
    "basemyai": {
      "command": "C:\\chemin\\absolu\\target\\release\\basemyai-mcp.exe",
      "env": {
        "BASEMYAI_FETCH": "1"
      }
    }
  }
}
```

Après `basemyai config key generate`, la passphrase est lue depuis
`%USERPROFILE%\\.basemyai\\key`. Sinon, définissez `BASEMYAI_DB_KEY` dans
`env` (voir [key-resolution.md](security/key-resolution.md)). Ajoutez
`BASEMYAI_DB_KEY_MODE=passphrase` pour un store Argon2id ; laissez la variable
absente pour ouvrir un store historique `raw-key`.

Retirez `BASEMYAI_FETCH` après le premier lancement réussi.

---

## 5. Transport HTTP (optionnel)

Pour un hôte distant ou un déploiement séparé (écoute `127.0.0.1` uniquement) :

```bash
BASEMYAI_DB_KEY=... \
BASEMYAI_MCP_TRANSPORT=http \
BASEMYAI_MCP_API_KEY=un-jeton-bearer-secret \
basemyai-mcp
# MCP Streamable HTTP sur http://127.0.0.1:7744/mcp (auth Bearer obligatoire)
```

L'exposition au-delà de `localhost` est une décision explicite de l'opérateur
(reverse-proxy) — jamais un défaut.

---

## 6. La consolidation, sans serveur LLM (le différenciateur)

`consolidate` distille les épisodes en faits durables + graphe d'entités. Il choisit
sa source d'inférence dans cet ordre (ADR-018) :

1. **Sampling MCP** — si votre client l'annonce (rare ; déprécié dans le protocole,
   non supporté par Claude Code).
2. **LLM local** — si Ollama / LM Studio / AnythingLLM tourne (autonome : le serveur
   fait l'extraction, vous recevez juste le bilan).
3. **Piloté par l'agent** — sinon, `consolidate` renvoie `status:"extraction_required"`
   avec les épisodes et des instructions : **votre agent (Claude lui-même) fait
   l'extraction**, puis la persiste en appelant `consolidate_apply`.

Le niveau 3 est le **vrai plug-and-play dans Claude Code** : zéro LLM à installer,
c'est le modèle de l'agent qui travaille, et **rien ne sort de votre machine**.

> **Le plus simple** : tapez la commande de prompt **`/mcp__basemyai__consolidate_memory`**
> (argument `agent_id`). Elle pilote tout le flux : récupère les épisodes, demande à
> Claude d'extraire faits/entités/relations, et déclenche `consolidate_apply`.

Pour activer le **LLM local** (niveau 2) dans Claude Code, ajoutez ses variables à la
config MCP — ex. AnythingLLM :

```bash
claude mcp add basemyai -s user \
  -e BASEMYAI_DB_KEY=... \
  -e BASEMYAI_ANYTHINGLLM_KEY=... \
  -e BASEMYAI_ANYTHINGLLM_WORKSPACE=mon-espace-dédié \
  -- /chemin/target/release/basemyai-mcp
```

Pour les consommateurs **sans agent** (SDK Python/Node, REST), seuls les niveaux 2
(LLM local) et — déféré — cloud opt-in BYOK s'appliquent. Détails et implications de
confidentialité : **ADR-018** (supersède ADR-017).
