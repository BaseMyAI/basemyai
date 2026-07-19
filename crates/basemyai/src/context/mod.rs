// SPDX-License-Identifier: BUSL-1.1
//! Compilation deterministe d'un recall en contexte borne et tracable.

mod compile;
mod render;
mod selection;
mod temporal;
mod token;
mod types;

pub use token::{ApproximateTokenEstimator, TokenEstimator};
pub use types::{
    ContextBundle, ContextCitation, ContextItem, ContextProfile, ContextRenderFormat, ContextRequest, ContextRole,
    ContextSection, ContextSectionKind, ContextSourcePolicy, ContextTemporalStatus, ContextTrace, ContextTraceEvent,
    ContextTraceLevel, ContextTraceSummary, ContextWarning, DedupCluster, ExcludedMemory, ExclusionReason,
    InclusionReason, MAX_CONTEXT_CANDIDATES, MAX_CONTEXT_TRACE_EVENTS, MergedMemory, RetrievalContribution,
};

use crate::{Memory, MemoryError, RecallOptions, Result};

impl Memory {
    /// Compile un recall hybride dans le format demande, sous budget estime.
    ///
    /// # Errors
    /// Retourne [`MemoryError::InvalidContextTokenBudget`] pour un budget nul,
    /// [`MemoryError::InvalidContextCandidateLimit`] pour un pool hors borne,
    /// ou propage les erreurs du recall hybride.
    pub async fn compile_context(&self, request: ContextRequest<'_>) -> Result<ContextBundle> {
        self.compile_context_with_estimator(request, &ApproximateTokenEstimator)
            .await
    }

    /// Variante utilisant un estimateur fourni par le consommateur.
    ///
    /// # Errors
    /// Memes erreurs que [`Self::compile_context`].
    pub async fn compile_context_with_estimator(
        &self,
        request: ContextRequest<'_>,
        estimator: &dyn TokenEstimator,
    ) -> Result<ContextBundle> {
        validate_request(&request)?;
        let records = self
            .recall_hybrid_with_options(
                request.query,
                request.candidate_limit,
                RecallOptions {
                    include_procedural: request.include_procedural,
                    exclude_imported: false,
                },
            )
            .await?;
        Ok(compile::compile_records(
            records,
            &request,
            estimator,
            crate::now_unix(),
        ))
    }
}

fn validate_request(request: &ContextRequest<'_>) -> Result<()> {
    if request.token_budget == 0 {
        return Err(MemoryError::InvalidContextTokenBudget);
    }
    if request.candidate_limit == 0 || request.candidate_limit > MAX_CONTEXT_CANDIDATES {
        return Err(MemoryError::InvalidContextCandidateLimit {
            value: request.candidate_limit,
            max: MAX_CONTEXT_CANDIDATES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_validation_rejects_invalid_bounds() {
        let zero_budget = ContextRequest::new("query", 0);
        assert!(matches!(
            validate_request(&zero_budget),
            Err(MemoryError::InvalidContextTokenBudget)
        ));

        let too_many = ContextRequest::new("query", 100).candidate_limit(MAX_CONTEXT_CANDIDATES + 1);
        assert!(matches!(
            validate_request(&too_many),
            Err(MemoryError::InvalidContextCandidateLimit { .. })
        ));
    }
}
