//! Compiled policies and their evaluation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use operation_governance_sdk::{
    Applicability, CheckMode, CheckResponse, FactSet, GovernanceCheckRequest, GovernanceOutcome,
    OgError, PolicyFinding, operation_family,
};
use serde_json::{Value, json};
use time::OffsetDateTime;
use toolkit_policy_evaluation::{
    CompiledDocument, CostBound, EvaluationContext, EvaluationError, RegoBackend, compile_screened,
};
use toolkit_security::SecurityContext;

use crate::config::{MAX_APPLICABILITY_LEASE_MS, OgRegoConfigPluginConfig, PolicyKind};

/// Prefix of a Fact reference in the evaluation input (`input.facts.<id>`).
const FACTS_PREFIX: &str = "facts";

struct CompiledPolicy {
    name: String,
    kind: PolicyKind,
    document: Arc<dyn CompiledDocument>,
}

struct OperationPlan {
    policies: Vec<CompiledPolicy>,
    /// Union of the Facts every policy of the operation reads.
    reads: FactSet,
}

/// The plugin's policies, compiled once at startup, keyed by operation family.
pub struct Service {
    operations: HashMap<String, OperationPlan>,
    bound: CostBound,
    lease_ms: u64,
}

impl Service {
    /// Compile every configured policy.
    ///
    /// # Errors
    /// When a policy does not compile, defines the wrong entrypoint, uses a
    /// denylisted builtin, reads Facts through a reference that cannot be
    /// resolved statically, or an operation is listed twice.
    pub fn from_config(cfg: &OgRegoConfigPluginConfig) -> anyhow::Result<Self> {
        if cfg.eval_timeout_ms == 0 {
            bail!("og-rego-config-plugin: eval_timeout_ms must be greater than zero");
        }
        if cfg.applicability_lease_ms > MAX_APPLICABILITY_LEASE_MS {
            bail!(
                "og-rego-config-plugin: applicability_lease_ms must be at most \
                 {MAX_APPLICABILITY_LEASE_MS} (got {})",
                cfg.applicability_lease_ms
            );
        }
        let backend = RegoBackend::new();
        let mut operations = HashMap::new();
        for op in &cfg.operations {
            let family = operation_family(&op.operation);
            let mut plan = OperationPlan {
                policies: Vec::new(),
                reads: FactSet::new(),
            };
            for policy in &op.policies {
                let document = compile_screened(
                    &backend,
                    &format!("{}.rego", policy.name),
                    &policy.source,
                    policy.kind.entrypoint(),
                )
                .with_context(|| format!("policy `{}` of `{family}`", policy.name))?;
                plan.reads.extend(
                    fact_reads(document.as_ref())
                        .with_context(|| format!("policy `{}` of `{family}`", policy.name))?,
                );
                plan.policies.push(CompiledPolicy {
                    name: policy.name.clone(),
                    kind: policy.kind,
                    document,
                });
            }
            if operations.insert(family.clone(), plan).is_some() {
                bail!("og-rego-config-plugin: operation `{family}` is listed twice");
            }
        }
        Ok(Self {
            operations,
            bound: CostBound::new(Duration::from_millis(cfg.eval_timeout_ms)),
            lease_ms: cfg.applicability_lease_ms,
        })
    }

    /// Number of operations with policies.
    #[must_use]
    pub fn operation_count(&self) -> usize {
        self.operations.len()
    }

    pub(crate) fn applicability(&self, operation: &str) -> Applicability {
        // "Not governed" is leased too: it is what lets ungoverned operations
        // skip the backend entirely between refreshes.
        let mut answer = self
            .operations
            .get(&operation_family(operation))
            .map_or_else(Applicability::default, |plan| Applicability {
                governed: !plan.policies.is_empty(),
                facts_read: plan.reads.clone(),
                lease_ms: 0,
            });
        answer.lease_ms = self.lease_ms;
        answer
    }

    pub(crate) fn check(
        &self,
        ctx: &SecurityContext,
        request: &GovernanceCheckRequest,
    ) -> Result<CheckResponse, OgError> {
        let family = operation_family(&request.operation);
        let Some(plan) = self.operations.get(&family) else {
            return Ok(CheckResponse::Decided(GovernanceOutcome::Allowed));
        };

        if let Some((fact, reason)) = request
            .unavailable
            .iter()
            .find(|(fact, _)| plan.reads.contains(*fact))
        {
            return Ok(CheckResponse::Decided(GovernanceOutcome::Unavailable {
                message: format!("fact `{fact}` could not be loaded: {reason}"),
                retryable: true,
            }));
        }
        let missing: FactSet = plan
            .reads
            .iter()
            .filter(|f| !request.facts.contains_key(*f))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Ok(if request.round == 0 {
                CheckResponse::NeedFacts { missing }
            } else {
                CheckResponse::Decided(GovernanceOutcome::Unavailable {
                    message: format!("facts still missing: {missing:?}"),
                    retryable: false,
                })
            });
        }

        let input = EvaluationContext::new(input_document(ctx, request, &family), now())
            .map_err(|e| OgError::Invalid(e.to_string()))?;

        let blocked = self.findings(plan, PolicyKind::Guardrail, &input)?;
        if !blocked.is_empty() {
            return Ok(CheckResponse::Decided(GovernanceOutcome::Blocked {
                findings: blocked,
            }));
        }
        let approval = self.findings(plan, PolicyKind::Approval, &input)?;
        if !approval.is_empty() {
            return Ok(CheckResponse::Decided(
                GovernanceOutcome::ApprovalRequired { findings: approval },
            ));
        }
        Ok(CheckResponse::Decided(GovernanceOutcome::Allowed))
    }

    fn findings(
        &self,
        plan: &OperationPlan,
        kind: PolicyKind,
        input: &EvaluationContext,
    ) -> Result<Vec<PolicyFinding>, OgError> {
        let mut findings = Vec::new();
        for policy in plan.policies.iter().filter(|p| p.kind == kind) {
            let value = policy
                .document
                .evaluate(input, self.bound)
                .map_err(|e| match e {
                    EvaluationError::BoundExceeded { .. } => {
                        OgError::Unavailable(format!("policy `{}`: {e}", policy.name))
                    }
                    EvaluationError::Failed { .. } => {
                        OgError::Internal(format!("policy `{}`: {e}", policy.name))
                    }
                })?;
            findings.extend(messages(value).into_iter().map(|message| PolicyFinding {
                policy: policy.name.clone(),
                message,
            }));
        }
        Ok(findings)
    }
}

/// Facts a compiled document reads: the first segment after `facts.` of each
/// referenced input path.
fn fact_reads(document: &dyn CompiledDocument) -> anyhow::Result<FactSet> {
    let mut reads = FactSet::new();
    for path in document.referenced_input_paths() {
        if path == FACTS_PREFIX {
            bail!(
                "reads `input.facts` as a whole or by a dynamic key; name each fact as \
                 `input.facts.<id>`"
            );
        }
        if let Some(rest) = path
            .strip_prefix(FACTS_PREFIX)
            .and_then(|r| r.strip_prefix('.'))
            && let Some(fact) = rest.split('.').next()
        {
            reads.insert(fact.to_owned());
        }
    }
    Ok(reads)
}

fn input_document(ctx: &SecurityContext, request: &GovernanceCheckRequest, family: &str) -> Value {
    json!({
        "subject": {
            "id": ctx.subject_id(),
            "tenant_id": ctx.subject_tenant_id(),
        },
        "operation": {
            "type": request.operation,
            "family": family,
        },
        "resource": request.resource,
        "mode": match request.mode {
            CheckMode::Enforce => "enforce",
            CheckMode::Preview => "preview",
        },
        "facts": request.facts,
    })
}

/// Messages of a `deny` / `require_approval` result: a set of strings, a
/// single string, or `true`.
fn messages(value: Option<Value>) -> Vec<String> {
    match value {
        None | Some(Value::Null | Value::Bool(false)) => Vec::new(),
        Some(Value::Array(items)) => items
            .into_iter()
            .map(|v| match v {
                Value::String(s) => s,
                other => other.to_string(),
            })
            .collect(),
        Some(Value::String(s)) => vec![s],
        Some(Value::Bool(true)) => vec!["policy matched".to_owned()],
        Some(other) => vec![other.to_string()],
    }
}

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
