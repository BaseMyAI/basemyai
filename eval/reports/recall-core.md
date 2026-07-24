# Recall Quality Lab

- Dataset: `recall-core.jsonl`
- Dataset fingerprint: `fnv1a64:bca133f8726b6383`
- Result: 11/11 cases passed
- Bundle budget compliance: 1.000

| Case | Suite | Result | Must include | Forbidden | Budget | Provenance |
|---|---|---:|---:|---:|---:|---:|
| `direct-release-id` | direct_relevance | PASS | 1.000 | 0.000 | yes | 1.000 |
| `temporal-stale-replacement` | temporal_stale | PASS | 1.000 | 0.000 | yes | 1.000 |
| `poisoning-import-filter` | poisoning_trust | PASS | 1.000 | 0.000 | yes | 1.000 |
| `procedure-exact-id` | procedures_ids | PASS | 1.000 | 0.000 | yes | 1.000 |
| `exact-dedup-citations` | deduplication | PASS | 1.000 | 0.000 | yes | 1.000 |
| `budget-512` | budgets | PASS | 1.000 | 0.000 | yes | 1.000 |
| `budget-2000` | budgets | PASS | 1.000 | 0.000 | yes | 1.000 |
| `budget-8000` | budgets | PASS | 1.000 | 0.000 | yes | 1.000 |
| `budget-32000` | budgets | PASS | 1.000 | 0.000 | yes | 1.000 |
| `graph-two-hop` | graph | PASS | 1.000 | 0.000 | yes | 1.000 |
| `deterministic-repeat` | determinism | PASS | 1.000 | 0.000 | yes | 1.000 |

## Retrieval

| Mode | Cases | Hit@K | Recall@K | Precision@K | MRR | nDCG@K | Exact ID |
|---|---:|---:|---:|---:|---:|---:|---:|
| vector | 2 | 1.000 | 1.000 | 0.667 | 0.750 | 0.626 | 1.000 |
| hybrid | 11 | 1.000 | 1.000 | 0.448 | 0.955 | 0.950 | 1.000 |
| graph | 1 | 1.000 | 1.000 | 0.400 | 1.000 | 0.834 | 1.000 |
