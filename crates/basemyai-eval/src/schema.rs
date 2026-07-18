use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{EvalError, Result};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalCase {
    pub schema_version: u32,
    pub id: String,
    pub suite: String,
    pub description: String,
    pub seed: u64,
    pub query: String,
    pub k: usize,
    pub token_budget: usize,
    #[serde(default = "default_candidate_limit")]
    pub candidate_limit: usize,
    #[serde(default)]
    pub options: CaseOptions,
    pub memories: Vec<MemoryFixture>,
    #[serde(default)]
    pub must_include: Vec<String>,
    #[serde(default)]
    pub must_exclude: Vec<String>,
    #[serde(default)]
    pub expected_provenance: BTreeMap<String, Provenance>,
    #[serde(default)]
    pub retrieval: BTreeMap<RetrievalMode, RetrievalExpectation>,
    #[serde(default)]
    pub graph: GraphFixture,
    #[serde(default)]
    pub assert_deterministic: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CaseOptions {
    #[serde(default)]
    pub include_procedural: bool,
    #[serde(default)]
    pub source_policy: SourcePolicy,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourcePolicy {
    AllowAll,
    #[default]
    ExcludeImported,
    UserAndConsolidationOnly,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryFixture {
    pub id: String,
    pub text: String,
    pub layer: Layer,
    #[serde(default)]
    pub source: Provenance,
    #[serde(default = "default_valid_from_offset")]
    pub valid_from_offset_secs: i64,
    #[serde(default)]
    pub valid_until_offset_secs: Option<i64>,
    #[serde(default)]
    pub relevance: u8,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub procedure_required: bool,
    #[serde(default)]
    pub conflict_group: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    ShortTerm,
    Episodic,
    Procedural,
    Semantic,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    #[default]
    User,
    Consolidation,
    Import,
    Unknown,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalMode {
    Vector,
    Hybrid,
    Graph,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RetrievalExpectation {
    #[serde(default)]
    pub must_include: Vec<String>,
    #[serde(default)]
    pub must_exclude: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct GraphFixture {
    #[serde(default)]
    pub entities: Vec<EntityFixture>,
    #[serde(default)]
    pub edges: Vec<EdgeFixture>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityFixture {
    pub id: String,
    pub kind: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeFixture {
    pub src: String,
    pub relation: String,
    pub dst: String,
    #[serde(default = "default_edge_weight")]
    pub weight: f64,
}

pub fn load_dataset(path: &Path) -> Result<Vec<EvalCase>> {
    let content = std::fs::read_to_string(path).map_err(|source| EvalError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_dataset(&content)
}

pub(crate) fn parse_dataset(content: &str) -> Result<Vec<EvalCase>> {
    let mut cases = Vec::new();
    for (index, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let case: EvalCase = serde_json::from_str(line).map_err(|source| EvalError::JsonLine {
            line: index + 1,
            source,
        })?;
        validate_case(&case)?;
        cases.push(case);
    }
    if cases.is_empty() {
        return Err(EvalError::Schema {
            case_id: "<dataset>".to_string(),
            message: "dataset must contain at least one JSON object".to_string(),
        });
    }
    let mut ids = BTreeSet::new();
    for case in &cases {
        if !ids.insert(case.id.as_str()) {
            return Err(EvalError::Schema {
                case_id: case.id.clone(),
                message: "case id is duplicated".to_string(),
            });
        }
    }
    Ok(cases)
}

fn validate_case(case: &EvalCase) -> Result<()> {
    let fail = |message: &str| {
        Err(EvalError::Schema {
            case_id: case.id.clone(),
            message: message.to_string(),
        })
    };
    if case.schema_version != SCHEMA_VERSION {
        return fail("unsupported schema_version");
    }
    if case.id.trim().is_empty() || case.suite.trim().is_empty() {
        return fail("id and suite must be non-empty");
    }
    if case.query.trim().is_empty() {
        return fail("query must be non-empty");
    }
    if case.k == 0 || case.token_budget == 0 || case.candidate_limit == 0 || case.candidate_limit > 256 {
        return fail("k, token_budget and candidate_limit must be positive; candidate_limit must be <= 256");
    }
    if case.memories.is_empty() {
        return fail("memories must be non-empty");
    }

    let mut memory_ids = BTreeSet::new();
    for memory in &case.memories {
        if memory.id.trim().is_empty() || memory.text.trim().is_empty() {
            return fail("memory id and text must be non-empty");
        }
        if memory.relevance > 3 {
            return fail("relevance must be in 0..=3");
        }
        if memory
            .valid_until_offset_secs
            .is_some_and(|until| until <= memory.valid_from_offset_secs)
        {
            return fail("valid_until_offset_secs must be greater than valid_from_offset_secs");
        }
        if !memory_ids.insert(memory.id.as_str()) {
            return fail("memory ids must be unique within a case");
        }
    }

    for reference in case
        .must_include
        .iter()
        .chain(&case.must_exclude)
        .chain(case.expected_provenance.keys())
        .chain(
            case.retrieval
                .values()
                .flat_map(|expectation| expectation.must_include.iter().chain(&expectation.must_exclude)),
        )
    {
        if !memory_ids.contains(reference.as_str()) {
            return fail("every expectation must reference a declared memory id");
        }
    }

    if case.retrieval.contains_key(&RetrievalMode::Graph) && case.graph.entities.is_empty() {
        return fail("graph retrieval requires at least one graph entity");
    }
    let mut entity_ids = BTreeSet::new();
    for entity in &case.graph.entities {
        if entity.id.trim().is_empty() || entity.kind.trim().is_empty() || entity.label.trim().is_empty() {
            return fail("graph entity id, kind and label must be non-empty");
        }
        if !entity_ids.insert(entity.id.as_str()) {
            return fail("graph entity ids must be unique");
        }
    }
    for edge in &case.graph.edges {
        if !edge.weight.is_finite() {
            return fail("graph edge weight must be finite");
        }
        if !entity_ids.contains(edge.src.as_str()) || !entity_ids.contains(edge.dst.as_str()) {
            return fail("graph edges must reference declared entity ids");
        }
    }
    Ok(())
}

const fn default_candidate_limit() -> usize {
    64
}

const fn default_valid_from_offset() -> i64 {
    -3_600
}

const fn default_edge_weight() -> f64 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_rejects_unknown_schema_versions() {
        let input = r#"{"schema_version":2,"id":"bad","suite":"schema","description":"bad","seed":1,"query":"q","k":1,"token_budget":10,"memories":[{"id":"m","text":"t","layer":"semantic"}]}"#;
        let error = parse_dataset(input).expect_err("schema version must be rejected");
        assert!(error.to_string().contains("unsupported schema_version"));
    }

    #[test]
    fn parser_rejects_dangling_expectations() {
        let input = r#"{"schema_version":1,"id":"bad","suite":"schema","description":"bad","seed":1,"query":"q","k":1,"token_budget":10,"memories":[{"id":"m","text":"t","layer":"semantic"}],"must_include":["missing"]}"#;
        let error = parse_dataset(input).expect_err("dangling id must be rejected");
        assert!(error.to_string().contains("declared memory id"));
    }
}
