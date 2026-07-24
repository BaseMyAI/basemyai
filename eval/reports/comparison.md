# Recall Quality Comparison

- Dataset: `recall-core.jsonl`
- Dataset fingerprint: `fnv1a64:bca133f8726b6383`
- Failed cases: 0 -> 0
- Regressions: 0

| Metric | Baseline | Current | Delta | Direction | Result |
|---|---:|---:|---:|---|---:|
| `retrieval.vector.hit_at_k` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `retrieval.vector.recall_at_k` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `retrieval.vector.precision_at_k` | 0.666667 | 0.666667 | +0.000000 | Higher | ok |
| `retrieval.vector.mrr` | 0.750000 | 0.750000 | +0.000000 | Higher | ok |
| `retrieval.vector.ndcg_at_k` | 0.625575 | 0.625575 | +0.000000 | Higher | ok |
| `retrieval.vector.exact_id_hit_rate` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `retrieval.hybrid.hit_at_k` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `retrieval.hybrid.recall_at_k` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `retrieval.hybrid.precision_at_k` | 0.448485 | 0.448485 | +0.000000 | Higher | ok |
| `retrieval.hybrid.mrr` | 0.954545 | 0.954545 | +0.000000 | Higher | ok |
| `retrieval.hybrid.ndcg_at_k` | 0.950185 | 0.950185 | +0.000000 | Higher | ok |
| `retrieval.hybrid.exact_id_hit_rate` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `retrieval.graph.hit_at_k` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `retrieval.graph.recall_at_k` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `retrieval.graph.precision_at_k` | 0.400000 | 0.400000 | +0.000000 | Higher | ok |
| `retrieval.graph.mrr` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `retrieval.graph.ndcg_at_k` | 0.833991 | 0.833991 | +0.000000 | Higher | ok |
| `retrieval.graph.exact_id_hit_rate` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `bundle.must_include_coverage` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `bundle.forbidden_inclusion_rate` | 0.000000 | 0.000000 | +0.000000 | Lower | ok |
| `bundle.budget_compliance_rate` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `bundle.duplicate_token_ratio` | 0.105763 | 0.105763 | +0.000000 | Lower | ok |
| `bundle.provenance_coverage` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `bundle.stale_fact_rate` | 0.000000 | 0.000000 | +0.000000 | Lower | ok |
| `bundle.source_filtered_leakage_rate` | 0.000000 | 0.000000 | +0.000000 | Lower | ok |
| `bundle.procedure_coverage` | 1.000000 | 1.000000 | +0.000000 | Higher | ok |
| `bundle.unreported_conflicts` | 0.000000 | 0.000000 | +0.000000 | Lower | ok |
