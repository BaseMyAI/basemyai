# Table de mapping de types cross-language — BaseMyAI

**Règles :**
- Enums Rust → string literals (jamais des int)
- `Option<T>` → `T | None` en Python, `T | null` en TypeScript
- Timestamps → `int` (epoch UTC secondes) en JSON, `datetime` en Python, `Date` en TypeScript
- `Vec<f32>` embeddings → non exposés par défaut en V1

---

## `MemoryLayer`

| Rust | JSON wire | Python (SDK) | TypeScript (SDK) |
|---|---|---|---|
| `MemoryLayer::ShortTerm` | `"short_term"` | `Literal["short_term"]` | `"short_term"` |
| `MemoryLayer::Episodic` | `"episodic"` | `Literal["episodic"]` | `"episodic"` |
| `MemoryLayer::Procedural` | `"procedural"` | `Literal["procedural"]` | `"procedural"` |
| `MemoryLayer::Semantic` | `"semantic"` | `Literal["semantic"]` | `"semantic"` |
| Type agrégé | `string` (enum) | `MemoryLayer = Literal["short_term", "episodic", "procedural", "semantic"]` | `type MemoryLayer = "short_term" \| "episodic" \| "procedural" \| "semantic"` |

---

## `AgentId` (newtype `String`)

| Rust | JSON wire | Python | TypeScript |
|---|---|---|---|
| `AgentId` | `string` | `str` — alias `AgentId = str` recommandé | `type AgentId = string` (branded optionnel) |
| Contraintes | minLength 1, maxLength 128 | validé côté client | validé côté client |

---

## `Record`

| Champ Rust | Type Rust | JSON wire | Python | TypeScript |
|---|---|---|---|---|
| `id` | `String` | `string` (UUID v4) | `str` | `string` |
| `text` | `String` | `string` | `str` | `string` |
| `layer` | `MemoryLayer` | `string` enum | `MemoryLayer` | `MemoryLayer` |
| `score` | `f32` | `number` [0,1] | `float` | `number` |

```python
# Python
from typing import Literal
MemoryLayer = Literal["short_term", "episodic", "procedural", "semantic"]

from dataclasses import dataclass

@dataclass
class Record:
    id: str
    text: str
    layer: MemoryLayer
    score: float
```

```typescript
// TypeScript
type MemoryLayer = "short_term" | "episodic" | "procedural" | "semantic";

interface Record {
  id: string;
  text: string;
  layer: MemoryLayer;
  score: number;
}
```

---

## `AgentStats`

| Champ Rust | Type Rust | JSON wire | Python | TypeScript |
|---|---|---|---|---|
| `short_term` | `usize` | `number` (integer ≥ 0) | `int` | `number` |
| `episodic` | `usize` | `number` | `int` | `number` |
| `procedural` | `usize` | `number` | `int` | `number` |
| `semantic` | `usize` | `number` | `int` | `number` |
| `total()` | calculé | `number` | `int` | `number` |

---

## `Fused` (résultat RRF — interne, V2 si endpoint dédié)

| Champ Rust | Type Rust | JSON wire | Python | TypeScript |
|---|---|---|---|---|
| `id` | `String` | `string` (UUID v4) | `str` | `string` |
| `score` | `f64` | `number` | `float` | `number` |
| `contributions` | `Vec<String>` | `string[]` | `list[str]` | `string[]` |

---

## `Entity` (traversée graphe)

| Champ Rust | Type Rust | JSON wire | Python | TypeScript |
|---|---|---|---|---|
| `id` | `String` | `string` | `str` | `string` |
| `kind` | `String` | `string` | `str` | `string` |
| `label` | `String` | `string` | `str` | `string` |
| `depth` | `u32` | `number` (integer ≥ 0) | `int` | `number` |

---

## `Edge` (graphe — V2)

| Champ SQL | Type Rust (conceptuel) | JSON wire | Python | TypeScript |
|---|---|---|---|---|
| `src` | `String` | `string` | `str` | `string` |
| `dst` | `String` | `string` | `str` | `string` |
| `relation` | `String` | `string` | `str` | `string` |
| `weight` | `f64` | `number` | `float` | `number` |
| `valid_until` | `Option<i64>` | `integer \| null` | `datetime \| None` | `Date \| null` |

---

## `valid_until: Option<i64>`

| Contexte | Rust | JSON wire | Python SDK | TypeScript SDK |
|---|---|---|---|---|
| Présent | `Some(1_780_000_000_i64)` | `1780000000` | `datetime(2026, 5, 5, tzinfo=timezone.utc)` | `new Date(1780000000 * 1000)` |
| Absent | `None` | `null` (ou champ omis) | `None` | `null` (champ optionnel `?`) |
| Conversion | — | entier signé 64 bits | `datetime.fromtimestamp(v, tz=timezone.utc)` | `new Date(v * 1000)` |

---

## `CoreError` / `MemoryError` → `ErrorResponse`

| Variante Rust | `code` JSON | HTTP status | Python exception | TypeScript class |
|---|---|---|---|---|
| `CoreError::Storage(_)` | `"INTERNAL_ERROR"` | 500 | `BasemyaiStorageError` | `class StorageError extends BasemyaiError` |
| `CoreError::Embed(_)` | `"INTERNAL_ERROR"` | 500 | `BasemyaiEmbedError` | `class EmbedError extends BasemyaiError` |
| `CoreError::Encryption` | `"ENCRYPTION_REQUIRED"` | 500 | `BasemyaiEncryptionError` | `class EncryptionError extends BasemyaiError` |
| `CoreError::ModelNotProvisioned(_)` | `"INTERNAL_ERROR"` | 500 | `BasemyaiSetupError` | `class SetupError extends BasemyaiError` |
| `MemoryError::MissingAgent` | `"MISSING_AGENT"` | 400 | `BasemyaiValidationError(ValueError)` | `class ValidationError extends BasemyaiError` |
| `MemoryError::UnknownLayer(_)` | `"UNKNOWN_LAYER"` | 400 | `BasemyaiValidationError` | `ValidationError` |
| `MemoryError::Inference(_)` | `"INTERNAL_ERROR"` | 500 | `BasemyaiInferenceError` | `class InferenceError extends BasemyaiError` |
| Bearer manquant/invalide | `"UNAUTHORIZED"` | 401 | `BasemyaiAuthError` | `class AuthError extends BasemyaiError` |
| Body > limite | `"PAYLOAD_TOO_LARGE"` | 413 | `BasemyaiPayloadError` | `class PayloadError extends BasemyaiError` |
| Timeout | `"TIMEOUT"` | 504 | `BasemyaiTimeoutError(TimeoutError)` | `class TimeoutError extends BasemyaiError` |

---

## `Vec<f32>` (embeddings)

| Contexte | Valeur |
|---|---|
| Exposé par le sidecar V1 | **Non** — vecteur opaque (`F32_BLOB` libSQL) |
| Exposé par les bindings natifs V2 | `list[float]` Python, `Float32Array` TypeScript |
| Format JSON si exposé | Tableau de 384 `number` (1.5 KiB par résultat) |
