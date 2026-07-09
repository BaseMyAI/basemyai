# Modèle de menace — BaseMyAI

BaseMyAI stocke la mémoire d'agents IA en local. Un même conteneur `.bmai` peut
héberger plusieurs agents (`agent_id`). Les menaces ci-dessous sont **in scope**
pour la V1 native (ADR-033).

## Actifs

| Actif | Sensibilité |
|-------|-------------|
| Contenu mémoire (texte, graphe) | Critique |
| Vecteurs d'embedding | Élevée (dérivés du texte) |
| Passphrase utilisateur (KEK) | Critique — jamais persistée par le produit |
| Modèle d'embedding local | Intégrité supply-chain |
| Clé API REST/MCP | Moyenne (accès local au sidecar) |

## Adversaires

1. **Agent malveillant** dans un tenant partagé — tente de lire/écrire la mémoire
   d'un autre `agent_id`.
2. **Contenu hostile** injecté en mémoire — tente d'empoisonner les recalls futurs
   (memory poisoning).
3. **Opérateur négligent** — bind REST public, passphrase faible, backup absent.
4. **Fichier store corrompu** — WAL/SST malformé fourni à l'ouverture.

## Défenses (résumé)

| Menace | Défense | Référence |
|--------|---------|-----------|
| Fuite cross-agent | Préfixes de clés + post-filtre vecteur | [agent-isolation.md](agent-isolation.md), ADR-006 |
| Clé récupérée | Enveloppe DEK/KEK, jamais loguée | [encryption-model.md](encryption-model.md), ADR-030 |
| Poisoning procedural | Exclusion par défaut du recall général | [memory-poisoning.md](memory-poisoning.md), ADR-035 |
| Store corrompu | CRC + AEAD, erreurs typées à l'open | [native-engine-format-security.md](native-engine-format-security.md) |
| REST exposé | Loopback par défaut, Bearer, dev=loopback only | [rest-security.md](rest-security.md) |

## Hors scope V1

- Comportement mathématique du modèle d'embedding
- Sécurité du LLM consommateur
- Accès physique à une machine déverrouillée avec la clé en RAM

Voir aussi [`SECURITY.md`](../../SECURITY.md) (politique publique).
