# Skill: security — BaseMyAI

## Surface d'attaque principale

BaseMyAI est un moteur de mémoire local. Les vecteurs d'attaque réalistes sont :

1. **Injection SQL** via `agent_id` ou contenu fourni par un agent externe
2. **Exfiltration de clé de chiffrement** (libSQL crypto)
3. **Contournement d'isolation** entre agents (mauvais scope `agent_id`)
4. **Timing attacks** sur la comparaison de tokens (MCP auth)
5. **Auto-download silencieux** de modèles (réseau non consenti)
6. **Logging de contenu** dans les traces d'audit

---

## Anti-SQL injection : `Filter` paramétré (ADR-006)

**Règle absolue** : tout input externe (agent_id, texte utilisateur, query) va dans `params`, jamais interpolé.

```rust
// BON — paramètres liés
let filter = Filter {
    sql: "agent_id = ?1 AND valid_until IS NULL".into(),
    params: vec![Value::Text(agent_id.as_str().to_string())],
};

// MAUVAIS — injection possible
let filter = Filter {
    sql: format!("agent_id = '{}'", agent_id.as_str()), // DANGEREUX
    params: vec![],
};
```

`AgentId` est un **newtype** `(String)` non-`Clone` public. Son constructeur valide le format (non-vide, pas d'espace). Cela empêche d'utiliser une string brute là où un `AgentId` est attendu.

```rust
// basemyai/src/memory/isolation.rs
pub struct AgentId(String);

impl AgentId {
    pub fn new(id: &str) -> Result<Self, MemoryError> {
        if id.trim().is_empty() {
            return Err(MemoryError::InvalidAgentId);
        }
        Ok(Self(id.to_string()))
    }
    pub fn as_str(&self) -> &str { &self.0 }
}
```

---

## Anti-timing attack : comparaison constante (MCP HTTP auth)

```rust
// basemyai-mcp/src/auth.rs
use subtle::ConstantTimeEq;

impl BearerAuthService {
    fn verify(&self, token: &str) -> bool {
        let expected = self.api_key.as_bytes();
        let provided = token.as_bytes();
        // Longueur différente → false en temps constant
        expected.ct_eq(provided).into()
    }
}
```

**Ne jamais** comparer des tokens avec `==` ou `starts_with` — observable par timing.

---

## Chiffrement libSQL (ADR-007)

```rust
// basemyai EXIGE une clé — ne peut pas ouvrir sans
pub struct Memory {
    store: Arc<Store>,
    agent: AgentId,
    enc_key: EncryptionKey,  // obligatoire
}

impl Memory {
    pub async fn open(
        path: &Path,
        agent_id: AgentId,
        key: EncryptionKey,  // pas Option<>
    ) -> Result<Self, MemoryError> { ... }

    // Test-only (feature "test-util") — sans chiffrement
    #[cfg(feature = "test-util")]
    pub async fn open_in_memory(agent_id: &str) -> Result<Self, MemoryError> { ... }
}
```

**`basemyai-core`** : chiffrement optionnel (`Option<EncryptionKey>`).
**`basemyai`** : chiffrement **obligatoire** — refus explicite si pas de clé.

---

## Audit MCP — ne jamais logger le contenu

```rust
// basemyai-mcp/src/audit.rs
pub fn emit_audit(tool: &str, agent_id: &str, outcome: Outcome, time_ms: u64) {
    tracing::info!(
        tool = tool,
        agent_id = agent_id,
        outcome = %outcome,
        time_ms = time_ms,
        "mcp_audit"
    );
    // INTERDIT dans cet appel :
    // - le texte mémorisé
    // - les vecteurs
    // - les résultats de recall
    // - toute PII
}
```

---

## Isolation multi-agent

Chaque `Memory` est construite avec un `AgentId` — toutes les requêtes SQL incluent `WHERE agent_id = ?`. Un agent ne peut pas accéder aux mémoires d'un autre.

```sql
-- Toutes les requêtes de recall incluent ce filtre
SELECT id, content, vec_distance_cosine(embedding, ?) AS score
FROM memories
WHERE agent_id = ?1
  AND (valid_until IS NULL OR valid_until > unixepoch())
ORDER BY score
LIMIT ?
```

---

## Zéro réseau dans la lib (ADR-010)

```rust
// INTERDIT dans basemyai-core et basemyai
impl CandleEmbedder {
    // NON — téléchargement silencieux
    pub fn new_auto() -> Result<Self> {
        download_model_if_needed()?; // INTERDIT
        ...
    }
    
    // OUI — chemin fourni par l'appelant (setup::provision)
    pub fn from_path(model_path: &Path, device: Device) -> Result<Self> { ... }
}
```

Le réseau est **uniquement** dans `basemyai/provision/embedder.rs` et `provision/llm.rs`, derrière un consentement explicite (`consent: bool`).

---

## Checklist de review sécurité

| Point | Check |
|-------|-------|
| Input externe dans SQL | Passe par `Filter.params`, jamais interpolé |
| Comparaison de token | `subtle::ConstantTimeEq`, jamais `==` |
| Chiffrement basemyai | `EncryptionKey` obligatoire (pas `Option`) |
| AgentId | Construit via `AgentId::new()` validé |
| Audit log | Contient outil + outcome + durée, JAMAIS le contenu |
| Réseau en lib | Aucun `reqwest`/`ureq`/`hyper` dans basemyai-core ou basemyai |
| `static mut` | Interdit — utiliser `OnceLock`/`RwLock` |
| Secrets dans les erreurs | Erreurs ne contiennent pas de clés ou de vecteurs |

---

## Vecteurs d'attaque spécifiques MCP

Le serveur MCP écoute des connexions de clients (agents LLM externes). Risques :

- **Prompt injection via le contenu mémorisé** : le contenu rappelé est retourné tel quel à l'agent appelant. Ne peut pas être filtré (c'est la donnée). Mitigation : isolation stricte par `agent_id`.
- **DDoS mémoire** : recall de très grands volumes. Mitigation : `max_result_bytes` dans `Config` (défaut 256 KiB), troncation avec `TruncationMarker`.
- **Brute-force Bearer token** : Mitigation : `BearerAuthLayer` avec `ConstantTimeEq` + pas de différence de timing entre "token trop court" et "token invalide".
