use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use basemyai::storage::{DEFAULT_IMPORTANCE, MemoryStore, NativeMemoryStore};
use basemyai::{
    AgentId, ContextRequest, ContextSectionKind, ContextSourcePolicy, ContextTemporalStatus, ExclusionReason,
    HashEmbedder, Memory, MemoryLayer, RecallOptions, Record, TrustLevel, Validity,
};
use basemyai_core::Embedder;

use crate::metrics::{bundle_metrics, retrieval_metrics};
use crate::report::{
    AssertionReport, BundleItemReport, BundleReport, CaseReport, ExcludedReport, MergedReport, RetrievalReport,
};
use crate::schema::{EvalCase, Layer, Provenance, RetrievalMode, SourcePolicy};
use crate::{EvalError, Result};

pub(crate) async fn execute_case(case: &EvalCase, record_timings: bool) -> Result<CaseReport> {
    let now = now_unix();
    let memory = open_seeded_memory(case, now).await?;
    let runtime_to_fixture = BTreeMap::<String, String>::new();
    seed_graph(&memory, case).await?;

    let recall_options = RecallOptions {
        include_procedural: case.options.include_procedural,
        exclude_imported: false,
    };
    let mut retrieval = BTreeMap::new();
    let mut observed_provenance = BTreeMap::new();
    for mode in case.retrieval.keys().copied() {
        let started = Instant::now();
        let records = match mode {
            RetrievalMode::Vector => memory.recall_with_options(&case.query, case.k, recall_options).await,
            RetrievalMode::Hybrid => {
                memory
                    .recall_hybrid_with_options(&case.query, case.k, recall_options)
                    .await
            }
            RetrievalMode::Graph => memory.search_graph(&case.query, case.k).await,
        }
        .map_err(|source| core_error(case, source))?;
        let elapsed = record_timings.then(|| micros(started.elapsed()));
        let ids = map_records(&records, &runtime_to_fixture, &mut observed_provenance);
        retrieval.insert(
            mode,
            RetrievalReport {
                metrics: retrieval_metrics(case, &ids),
                ids,
                latency_micros: elapsed,
            },
        );
    }

    let mut request = ContextRequest::new(&case.query, case.token_budget)
        .candidate_limit(case.candidate_limit)
        .source_policy(source_policy(case.options.source_policy))
        .explain();
    if case.options.include_procedural {
        request = request.include_procedural();
    }
    let bundle_started = Instant::now();
    let bundle = memory
        .compile_context(request)
        .await
        .map_err(|source| core_error(case, source))?;
    let bundle_latency = record_timings.then(|| micros(bundle_started.elapsed()));

    let mut items = Vec::new();
    let mut included_ids = Vec::new();
    let mut item_texts = Vec::new();
    for section in bundle.sections {
        for item in section.items {
            let mut source_ids: Vec<String> = item
                .source_memory_ids
                .iter()
                .map(|id| fixture_id(id, &runtime_to_fixture))
                .collect();
            source_ids.sort();
            source_ids.dedup();
            for id in &source_ids {
                observed_provenance.insert(id.clone(), provenance(item.trust));
            }
            included_ids.extend(source_ids.iter().cloned());
            item_texts.push(item.text.clone());
            items.push(BundleItemReport {
                section: section_name(section.kind).to_string(),
                text: item.text,
                source_ids,
                layer: layer_name(item.layer).to_string(),
                provenance: trust_name(item.trust).to_string(),
                estimated_tokens: item.estimated_tokens,
            });
        }
    }
    included_ids.sort();
    included_ids.dedup();

    let mut excluded: Vec<ExcludedReport> = bundle
        .excluded
        .into_iter()
        .map(|entry| ExcludedReport {
            id: fixture_id(&entry.memory_id, &runtime_to_fixture),
            reason: exclusion_name(entry.reason).to_string(),
            temporal_status: temporal_name(entry.temporal_status).to_string(),
        })
        .collect();
    excluded.sort_by(|left, right| {
        (&left.id, &left.reason, &left.temporal_status).cmp(&(&right.id, &right.reason, &right.temporal_status))
    });
    let mut merged: Vec<MergedReport> = bundle
        .merged
        .into_iter()
        .map(|entry| MergedReport {
            id: fixture_id(&entry.memory_id, &runtime_to_fixture),
            representative_id: fixture_id(&entry.representative_memory_id, &runtime_to_fixture),
        })
        .collect();
    merged.sort_by(|left, right| (&left.id, &left.representative_id).cmp(&(&right.id, &right.representative_id)));

    let metrics = bundle_metrics(
        case,
        &included_ids,
        &item_texts,
        &observed_provenance,
        bundle.estimated_tokens,
    );
    let mut assertions = assertions(case, &retrieval, &included_ids, &observed_provenance, &metrics);
    assertions.sort_by(|left, right| left.name.cmp(&right.name));
    let passed = assertions.iter().all(|assertion| assertion.passed);

    Ok(CaseReport {
        id: case.id.clone(),
        suite: case.suite.clone(),
        description: case.description.clone(),
        seed: case.seed,
        query: case.query.clone(),
        retrieval,
        bundle: BundleReport {
            items,
            included_ids,
            excluded,
            merged,
            estimated_tokens: bundle.estimated_tokens,
            token_budget: case.token_budget,
            metrics,
            latency_micros: bundle_latency,
        },
        assertions,
        passed,
    })
}

async fn open_seeded_memory(case: &EvalCase, now: i64) -> Result<Memory> {
    let store = Arc::new(NativeMemoryStore::open_ephemeral().map_err(|source| core_error(case, source))?);
    let agent = AgentId::new(format!("eval-{}-{}", case.seed, case.id)).ok_or_else(|| EvalError::Schema {
        case_id: case.id.clone(),
        message: "generated agent id is empty".to_string(),
    })?;
    let embedder = HashEmbedder::new();
    let mut fixtures: Vec<_> = case.memories.iter().collect();
    fixtures.sort_by_key(|fixture| (seeded_key(case.seed, &fixture.id), fixture.id.as_str()));
    for fixture in fixtures {
        let vector = embedder
            .embed(&fixture.text)
            .map_err(basemyai::MemoryError::Core)
            .map_err(|source| core_error(case, source))?;
        store
            .put_memory(
                &fixture.id,
                &agent,
                layer(fixture.layer),
                &fixture.text,
                validity(fixture, now),
                &vector,
                provenance_name(fixture.source),
                DEFAULT_IMPORTANCE,
            )
            .await
            .map_err(|source| core_error(case, source))?;
    }
    Memory::from_native_store(store, Box::new(HashEmbedder::new()), agent)
        .await
        .map_err(|source| core_error(case, source))
}

fn seeded_key(seed: u64, id: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64 ^ seed;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

async fn seed_graph(memory: &Memory, case: &EvalCase) -> Result<()> {
    let graph = memory.graph();
    for entity in &case.graph.entities {
        graph
            .add_entity(&entity.id, &entity.kind, &entity.label)
            .await
            .map_err(|source| core_error(case, source))?;
    }
    for edge in &case.graph.edges {
        graph
            .add_edge(&edge.src, &edge.relation, &edge.dst, edge.weight)
            .await
            .map_err(|source| core_error(case, source))?;
    }
    Ok(())
}

fn map_records(
    records: &[Record],
    runtime_to_fixture: &BTreeMap<String, String>,
    observed: &mut BTreeMap<String, Provenance>,
) -> Vec<String> {
    records
        .iter()
        .map(|record| {
            let id = fixture_id(&record.id, runtime_to_fixture);
            observed.insert(id.clone(), provenance(record.trust()));
            id
        })
        .collect()
}

fn assertions(
    case: &EvalCase,
    retrieval: &BTreeMap<RetrievalMode, RetrievalReport>,
    included_ids: &[String],
    observed_provenance: &BTreeMap<String, Provenance>,
    metrics: &crate::report::BundleMetrics,
) -> Vec<AssertionReport> {
    let included: BTreeSet<&str> = included_ids.iter().map(String::as_str).collect();
    let missing: Vec<_> = case
        .must_include
        .iter()
        .filter(|id| !included.contains(id.as_str()))
        .cloned()
        .collect();
    let forbidden: Vec<_> = case
        .must_exclude
        .iter()
        .filter(|id| included.contains(id.as_str()))
        .cloned()
        .collect();
    let provenance_mismatches: Vec<_> = case
        .expected_provenance
        .iter()
        .filter(|(id, expected)| observed_provenance.get(id.as_str()) != Some(expected))
        .map(|(id, expected)| format!("{id}: expected {expected:?}, got {:?}", observed_provenance.get(id)))
        .collect();

    let mut output = vec![
        assertion(
            "bundle.must_include",
            missing.is_empty(),
            format!("missing IDs: {missing:?}"),
        ),
        assertion(
            "bundle.must_exclude",
            forbidden.is_empty(),
            format!("forbidden IDs included: {forbidden:?}"),
        ),
        assertion(
            "bundle.budget",
            metrics.budget_compliant,
            format!("budget compliant: {}", metrics.budget_compliant),
        ),
        assertion(
            "provenance",
            provenance_mismatches.is_empty(),
            format!("mismatches: {provenance_mismatches:?}"),
        ),
    ];
    for (mode, expectation) in &case.retrieval {
        let ids = retrieval.get(mode).map_or(&[][..], |report| report.ids.as_slice());
        let missing: Vec<_> = expectation
            .must_include
            .iter()
            .filter(|id| !ids.contains(id))
            .cloned()
            .collect();
        let forbidden: Vec<_> = expectation
            .must_exclude
            .iter()
            .filter(|id| ids.contains(id))
            .cloned()
            .collect();
        output.push(assertion(
            &format!("retrieval.{mode:?}.must_include").to_lowercase(),
            missing.is_empty(),
            format!("missing IDs: {missing:?}"),
        ));
        output.push(assertion(
            &format!("retrieval.{mode:?}.must_exclude").to_lowercase(),
            forbidden.is_empty(),
            format!("forbidden IDs returned: {forbidden:?}"),
        ));
    }
    output
}

fn assertion(name: &str, passed: bool, details: String) -> AssertionReport {
    AssertionReport {
        name: name.to_string(),
        passed,
        details,
    }
}

fn validity(fixture: &crate::schema::MemoryFixture, now: i64) -> Validity {
    Validity {
        valid_from: now.saturating_add(fixture.valid_from_offset_secs),
        valid_until: fixture.valid_until_offset_secs.map(|offset| now.saturating_add(offset)),
    }
}

const fn layer(value: Layer) -> MemoryLayer {
    match value {
        Layer::ShortTerm => MemoryLayer::ShortTerm,
        Layer::Episodic => MemoryLayer::Episodic,
        Layer::Procedural => MemoryLayer::Procedural,
        Layer::Semantic => MemoryLayer::Semantic,
    }
}

const fn source_policy(value: SourcePolicy) -> ContextSourcePolicy {
    match value {
        SourcePolicy::AllowAll => ContextSourcePolicy::AllowAll,
        SourcePolicy::ExcludeImported => ContextSourcePolicy::ExcludeImported,
        SourcePolicy::UserAndConsolidationOnly => ContextSourcePolicy::UserAndConsolidationOnly,
    }
}

const fn provenance(value: TrustLevel) -> Provenance {
    match value {
        TrustLevel::Import => Provenance::Import,
        TrustLevel::User => Provenance::User,
        TrustLevel::Consolidation => Provenance::Consolidation,
        TrustLevel::Unknown => Provenance::Unknown,
        _ => Provenance::Unknown,
    }
}

const fn provenance_name(value: Provenance) -> &'static str {
    match value {
        Provenance::User => "user",
        Provenance::Consolidation => "consolidation",
        Provenance::Import => "import",
        Provenance::Unknown => "eval-unknown",
    }
}

const fn layer_name(value: MemoryLayer) -> &'static str {
    match value {
        MemoryLayer::ShortTerm => "short_term",
        MemoryLayer::Episodic => "episodic",
        MemoryLayer::Procedural => "procedural",
        MemoryLayer::Semantic => "semantic",
        _ => "unknown",
    }
}

const fn section_name(value: ContextSectionKind) -> &'static str {
    match value {
        ContextSectionKind::WorkingContext => "working_context",
        ContextSectionKind::CurrentFacts => "current_facts",
        ContextSectionKind::Procedures => "procedures",
        ContextSectionKind::RecentEvents => "recent_events",
        _ => "unknown",
    }
}

const fn trust_name(value: TrustLevel) -> &'static str {
    match value {
        TrustLevel::User => "user",
        TrustLevel::Consolidation => "consolidation",
        TrustLevel::Import => "import",
        TrustLevel::Unknown => "unknown",
        _ => "unknown",
    }
}

const fn exclusion_name(value: ExclusionReason) -> &'static str {
    match value {
        ExclusionReason::SourceFiltered => "source_filtered",
        ExclusionReason::NotCurrentlyValid => "not_currently_valid",
        ExclusionReason::TokenBudget => "token_budget",
        _ => "unknown",
    }
}

const fn temporal_name(value: ContextTemporalStatus) -> &'static str {
    match value {
        ContextTemporalStatus::Current => "current",
        ContextTemporalStatus::Scheduled => "scheduled",
        ContextTemporalStatus::Expired => "expired",
        _ => "unknown",
    }
}

fn fixture_id(runtime_id: &str, runtime_to_fixture: &BTreeMap<String, String>) -> String {
    runtime_to_fixture
        .get(runtime_id)
        .cloned()
        .unwrap_or_else(|| runtime_id.to_string())
}

fn core_error(case: &EvalCase, source: basemyai::MemoryError) -> EvalError {
    EvalError::BaseMyAi {
        case_id: case.id.clone(),
        source,
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

fn micros(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}
