//! Tests d'intégration : scénarios métier multi-étapes (remember → recall,
//! invalidation, temporalité, isolation, batch, SSE).
#![cfg(feature = "test-util")]

#[path = "support/mod.rs"]
mod support;

#[path = "integration/batch_atomicity.rs"]
mod batch_atomicity;
#[path = "integration/events.rs"]
mod events;
#[path = "integration/isolation.rs"]
mod isolation;
#[path = "integration/lifecycle.rs"]
mod lifecycle;
#[path = "integration/temporal.rs"]
mod temporal;
