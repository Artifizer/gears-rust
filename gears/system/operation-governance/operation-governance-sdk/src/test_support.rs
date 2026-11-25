//! Test helpers for gears that use Operation Governance.
//!
//! - [`ScriptedPlugin`] — a governance backend with scripted answers that
//!   records every request, to test a gear's governance step without policies.
//! - [`check_provider_covers`] — the conformance check every gear runs for each
//!   of its [`FactProvider`]s: the provider can fill every declared Fact.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use parking_lot::Mutex;
use toolkit_security::SecurityContext;

use crate::error::OgError;
use crate::facts::{FactProvider, FactSchema, FactWriter};
use crate::models::{Applicability, CheckResponse, FactSet, GovernanceCheckRequest};
use crate::plugin_api::OperationGovernancePluginClientV1;

/// A governance backend whose answers are scripted by the test.
///
/// `check` answers the queued responses in order, then Allowed.
#[derive(Default)]
pub struct ScriptedPlugin {
    applicability: Mutex<Applicability>,
    responses: Mutex<VecDeque<Result<CheckResponse, OgError>>>,
    requests: Mutex<Vec<GovernanceCheckRequest>>,
    applicability_calls: AtomicUsize,
}

impl ScriptedPlugin {
    /// No policy governs anything: every check takes the fast path.
    #[must_use]
    pub fn not_governed() -> Self {
        let plugin = Self::default();
        plugin.applicability.lock().lease_ms = 30_000;
        plugin
    }

    /// Grant this applicability lease, in milliseconds (`0`: do not cache).
    #[must_use]
    pub fn with_lease_ms(self, lease_ms: u64) -> Self {
        self.applicability.lock().lease_ms = lease_ms;
        self
    }

    /// Every operation is governed, and its policies read `facts_read`.
    #[must_use]
    pub fn governed<'a>(facts_read: impl IntoIterator<Item = &'a str>) -> Self {
        let plugin = Self::default();
        *plugin.applicability.lock() = Applicability {
            governed: true,
            facts_read: facts_read.into_iter().map(str::to_owned).collect(),
            lease_ms: 30_000,
        };
        plugin
    }

    /// Queue the next answer of `check`.
    #[must_use]
    pub fn then(self, response: Result<CheckResponse, OgError>) -> Self {
        self.responses.lock().push_back(response);
        self
    }

    /// Every check request received, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<GovernanceCheckRequest> {
        self.requests.lock().clone()
    }

    /// How many times applicability was asked.
    #[must_use]
    pub fn applicability_calls(&self) -> usize {
        self.applicability_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl OperationGovernancePluginClientV1 for ScriptedPlugin {
    async fn applicability(
        &self,
        _ctx: &SecurityContext,
        _operation: &str,
    ) -> Result<Applicability, OgError> {
        self.applicability_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.applicability.lock().clone())
    }

    async fn check(
        &self,
        _ctx: &SecurityContext,
        request: &GovernanceCheckRequest,
    ) -> Result<CheckResponse, OgError> {
        self.requests.lock().push(request.clone());
        self.responses
            .lock()
            .pop_front()
            .unwrap_or(Ok(CheckResponse::Decided(
                crate::models::GovernanceOutcome::Allowed,
            )))
    }
}

/// Check that `provider` fills every Fact its Governed Operation declares,
/// with values that match their schemas.
///
/// # Errors
/// A description of the first Fact that is undeclared, malformed, or missing.
pub async fn check_provider_covers<P: FactProvider>(provider: &P) -> Result<(), String> {
    let schema = FactSchema::of::<P::Facts>().map_err(|e| e.to_string())?;
    let declared: FactSet = schema.declared();
    let mut writer = FactWriter::new(&schema);
    provider
        .provide(&declared, &mut writer)
        .await
        .map_err(|e| e.to_string())?;
    match declared.iter().find(|f| !writer.has(f)) {
        Some(missing) => Err(format!(
            "`{}`: the provider did not write fact `{missing}`",
            schema.type_id()
        )),
        None => Ok(()),
    }
}
