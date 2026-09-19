//! 0.2.70 Evolution Runtime contracts.
//!
//! This module is storage-neutral.  Artifact bytes and ledger persistence remain host-owned; the
//! core owns canonical bytes, content-addressed identities, and fail-closed validation of the
//! proposal → evaluation → promotion → activation graph.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest as ShaDigest, Sha256};
use thiserror::Error;

pub const EVOLUTION_SCHEMA: &str = "evolution/v1";
pub const EVOLUTION_REPORT_SCHEMA: &str = "evolution-report/v1";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentDigest(String);

impl ContentDigest {
    pub fn parse(value: impl Into<String>) -> Result<Self, EvolutionError> {
        let value = value.into();
        let valid = value.len() == 71
            && value.starts_with("sha256:")
            && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit());
        if valid {
            Ok(Self(value.to_ascii_lowercase()))
        } else {
            Err(EvolutionError::InvalidDigest(value))
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Self(format!("sha256:{:x}", hasher.finalize()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ContentDigest {
    type Error = EvolutionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<ContentDigest> for String {
    fn from(value: ContentDigest) -> Self {
        value.0
    }
}

/// The artifact identity a kernel operation is bound to at genesis.
///
/// Artifact bytes and promotion records stay host-owned. The kernel receives only their
/// content-addressed identities, which makes the operation's execution set explicit and
/// replayable without turning the kernel into an artifact store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSetBinding {
    pub artifact_set_digest: ContentDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion_decision_digest: Option<ContentDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage_root_digest: Option<ContentDigest>,
}

impl ArtifactSetBinding {
    pub fn new(artifact_set_digest: ContentDigest) -> Self {
        Self {
            artifact_set_digest,
            promotion_decision_digest: None,
            lineage_root_digest: None,
        }
    }
}

impl std::fmt::Display for ContentDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EvolutionError {
    #[error("invalid content digest: {0}")]
    InvalidDigest(String),
    #[error("canonical evolution object could not be serialized: {0}")]
    Serialization(String),
    #[error("duplicate artifact reference: {0}")]
    DuplicateArtifact(ContentDigest),
    #[error("artifact set must contain at least one artifact")]
    EmptyArtifactSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Runtime,
    Skill,
    Prompt,
    Policy,
    Toolset,
    Bundle,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub kind: ArtifactKind,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactManifest {
    pub kind: ArtifactKind,
    pub payload_digest: ContentDigest,
    pub parents: Vec<ContentDigest>,
    pub toolchain_digest: ContentDigest,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVersion {
    pub digest: ContentDigest,
    pub manifest: ArtifactManifest,
}

impl ArtifactVersion {
    pub fn from_manifest(manifest: ArtifactManifest) -> Result<Self, EvolutionError> {
        let digest = canonical_digest(&manifest)?;
        Ok(Self { digest, manifest })
    }

    pub fn reference(&self) -> ArtifactRef {
        ArtifactRef {
            kind: self.manifest.kind,
            digest: self.digest.clone(),
        }
    }

    pub fn verify_digest(&self) -> Result<(), EvolutionError> {
        let expected = canonical_digest(&self.manifest)?;
        if expected != self.digest {
            return Err(EvolutionError::InvalidDigest(self.digest.to_string()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSet {
    pub digest: ContentDigest,
    pub artifacts: Vec<ArtifactRef>,
}

impl ArtifactSet {
    pub fn new(mut artifacts: Vec<ArtifactRef>) -> Result<Self, EvolutionError> {
        if artifacts.is_empty() {
            return Err(EvolutionError::EmptyArtifactSet);
        }
        artifacts.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.digest.cmp(&right.digest))
        });
        for pair in artifacts.windows(2) {
            if pair[0].digest == pair[1].digest {
                return Err(EvolutionError::DuplicateArtifact(pair[0].digest.clone()));
            }
        }
        let digest = canonical_digest(&artifacts)?;
        Ok(Self { digest, artifacts })
    }

    pub fn verify_digest(&self) -> Result<(), EvolutionError> {
        let expected = canonical_digest(&self.artifacts)?;
        if expected != self.digest {
            return Err(EvolutionError::InvalidDigest(self.digest.to_string()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvolutionProposal {
    pub digest: ContentDigest,
    pub base_artifact_set: ContentDigest,
    pub candidate_artifact_set: ContentDigest,
    pub objective: String,
    pub change_manifest: ContentDigest,
    pub proposer: String,
    pub constraints: Vec<String>,
}

impl EvolutionProposal {
    pub fn new(
        base_artifact_set: ContentDigest,
        candidate_artifact_set: ContentDigest,
        objective: impl Into<String>,
        change_manifest: ContentDigest,
        proposer: impl Into<String>,
        constraints: Vec<String>,
    ) -> Result<Self, EvolutionError> {
        let unsigned = Self {
            digest: ContentDigest::from_bytes(b"placeholder"),
            base_artifact_set,
            candidate_artifact_set,
            objective: objective.into(),
            change_manifest,
            proposer: proposer.into(),
            constraints,
        };
        let digest = canonical_digest(&ProposalBody::from(&unsigned))?;
        Ok(Self { digest, ..unsigned })
    }

    pub fn verify_digest(&self) -> Result<(), EvolutionError> {
        let expected = canonical_digest(&ProposalBody::from(self))?;
        if expected != self.digest {
            return Err(EvolutionError::InvalidDigest(self.digest.to_string()));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct ProposalBody<'a> {
    base_artifact_set: &'a ContentDigest,
    candidate_artifact_set: &'a ContentDigest,
    objective: &'a str,
    change_manifest: &'a ContentDigest,
    proposer: &'a str,
    constraints: &'a [String],
}

impl<'a> From<&'a EvolutionProposal> for ProposalBody<'a> {
    fn from(value: &'a EvolutionProposal) -> Self {
        Self {
            base_artifact_set: &value.base_artifact_set,
            candidate_artifact_set: &value.candidate_artifact_set,
            objective: &value.objective,
            change_manifest: &value.change_manifest,
            proposer: &value.proposer,
            constraints: &value.constraints,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationRun {
    pub digest: ContentDigest,
    pub proposal: ContentDigest,
    pub baseline_artifact_set: ContentDigest,
    pub candidate_artifact_set: ContentDigest,
    pub evaluator: ContentDigest,
    pub dataset: ContentDigest,
    pub operation_ids: Vec<String>,
    pub evidence_refs: Vec<ContentDigest>,
}

impl EvaluationRun {
    pub fn verify_digest(&self) -> Result<(), EvolutionError> {
        let expected = canonical_digest(&EvaluationRunBody::from(self))?;
        if expected != self.digest {
            return Err(EvolutionError::InvalidDigest(self.digest.to_string()));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct EvaluationRunBody<'a> {
    proposal: &'a ContentDigest,
    baseline_artifact_set: &'a ContentDigest,
    candidate_artifact_set: &'a ContentDigest,
    evaluator: &'a ContentDigest,
    dataset: &'a ContentDigest,
    operation_ids: &'a [String],
    evidence_refs: &'a [ContentDigest],
}

impl<'a> From<&'a EvaluationRun> for EvaluationRunBody<'a> {
    fn from(value: &'a EvaluationRun) -> Self {
        Self {
            proposal: &value.proposal,
            baseline_artifact_set: &value.baseline_artifact_set,
            candidate_artifact_set: &value.candidate_artifact_set,
            evaluator: &value.evaluator,
            dataset: &value.dataset,
            operation_ids: &value.operation_ids,
            evidence_refs: &value.evidence_refs,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationMetric {
    pub name: String,
    pub baseline: String,
    pub candidate: String,
    pub improved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationGate {
    pub name: String,
    pub required: bool,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationFact {
    pub digest: ContentDigest,
    pub evaluation: ContentDigest,
    pub metrics: Vec<EvaluationMetric>,
    pub gates: Vec<EvaluationGate>,
    pub replay_passed: bool,
}

impl EvaluationFact {
    pub fn verify_digest(&self) -> Result<(), EvolutionError> {
        let expected = canonical_digest(&EvaluationFactBody::from(self))?;
        if expected != self.digest {
            return Err(EvolutionError::InvalidDigest(self.digest.to_string()));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct EvaluationFactBody<'a> {
    evaluation: &'a ContentDigest,
    metrics: &'a [EvaluationMetric],
    gates: &'a [EvaluationGate],
    replay_passed: bool,
}

impl<'a> From<&'a EvaluationFact> for EvaluationFactBody<'a> {
    fn from(value: &'a EvaluationFact) -> Self {
        Self {
            evaluation: &value.evaluation,
            metrics: &value.metrics,
            gates: &value.gates,
            replay_passed: value.replay_passed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionOutcome {
    Promote,
    Reject,
    Hold,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionDecision {
    pub digest: ContentDigest,
    pub proposal: ContentDigest,
    pub evaluation_facts: Vec<ContentDigest>,
    pub policy: ContentDigest,
    pub outcome: PromotionOutcome,
    pub selected_artifact_set: ContentDigest,
}

impl PromotionDecision {
    pub fn verify_digest(&self) -> Result<(), EvolutionError> {
        let expected = canonical_digest(&PromotionDecisionBody::from(self))?;
        if expected != self.digest {
            return Err(EvolutionError::InvalidDigest(self.digest.to_string()));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct PromotionDecisionBody<'a> {
    proposal: &'a ContentDigest,
    evaluation_facts: &'a [ContentDigest],
    policy: &'a ContentDigest,
    outcome: PromotionOutcome,
    selected_artifact_set: &'a ContentDigest,
}

impl<'a> From<&'a PromotionDecision> for PromotionDecisionBody<'a> {
    fn from(value: &'a PromotionDecision) -> Self {
        Self {
            proposal: &value.proposal,
            evaluation_facts: &value.evaluation_facts,
            policy: &value.policy,
            outcome: value.outcome,
            selected_artifact_set: &value.selected_artifact_set,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationBinding {
    pub digest: ContentDigest,
    pub operation_id: String,
    pub artifact_set: ContentDigest,
    pub promotion_decision: ContentDigest,
}

impl ActivationBinding {
    pub fn verify_digest(&self) -> Result<(), EvolutionError> {
        let expected = canonical_digest(&(
            &self.operation_id,
            &self.artifact_set,
            &self.promotion_decision,
        ))?;
        if expected != self.digest {
            return Err(EvolutionError::InvalidDigest(self.digest.to_string()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvolutionBundle {
    pub artifacts: Vec<ArtifactVersion>,
    pub artifact_sets: Vec<ArtifactSet>,
    pub proposals: Vec<EvolutionProposal>,
    pub evaluations: Vec<EvaluationRun>,
    pub facts: Vec<EvaluationFact>,
    pub decisions: Vec<PromotionDecision>,
    pub activations: Vec<ActivationBinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvolutionVerdict {
    Pass,
    Fail,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvolutionViolation {
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvolutionReport {
    pub schema: String,
    pub verdict: EvolutionVerdict,
    pub violations: Vec<EvolutionViolation>,
}

pub fn validate_evolution(bundle: &EvolutionBundle) -> EvolutionReport {
    let mut violations = Vec::new();
    let artifacts: HashMap<_, _> = bundle
        .artifacts
        .iter()
        .map(|artifact| (artifact.digest.clone(), artifact))
        .collect();
    let sets: HashMap<_, _> = bundle
        .artifact_sets
        .iter()
        .map(|set| (set.digest.clone(), set))
        .collect();
    let proposals: HashMap<_, _> = bundle
        .proposals
        .iter()
        .map(|proposal| (proposal.digest.clone(), proposal))
        .collect();
    let evaluations: HashMap<_, _> = bundle
        .evaluations
        .iter()
        .map(|evaluation| (evaluation.digest.clone(), evaluation))
        .collect();
    let facts: HashMap<_, _> = bundle
        .facts
        .iter()
        .map(|fact| (fact.digest.clone(), fact))
        .collect();
    let decisions: HashMap<_, _> = bundle
        .decisions
        .iter()
        .map(|decision| (decision.digest.clone(), decision))
        .collect();

    for artifact in &bundle.artifacts {
        if artifact.verify_digest().is_err() {
            violation(
                &mut violations,
                "E1",
                format!("artifact {} digest mismatch", artifact.digest),
            );
        }
        for parent in &artifact.manifest.parents {
            if !artifacts.contains_key(parent) {
                violation(
                    &mut violations,
                    "E2",
                    format!("artifact {} has missing parent {parent}", artifact.digest),
                );
            }
        }
    }
    for artifact in &bundle.artifacts {
        if has_artifact_cycle(
            artifact.digest.clone(),
            &artifacts,
            &mut HashSet::new(),
            &mut HashSet::new(),
        ) {
            violation(
                &mut violations,
                "E2",
                format!("artifact lineage contains a cycle at {}", artifact.digest),
            );
        }
    }
    for set in &bundle.artifact_sets {
        if set.verify_digest().is_err() {
            violation(
                &mut violations,
                "E1",
                format!("artifact set {} digest mismatch", set.digest),
            );
        }
        for reference in &set.artifacts {
            if !artifacts.contains_key(&reference.digest) {
                violation(
                    &mut violations,
                    "E2",
                    format!(
                        "artifact set {} references missing {}",
                        set.digest, reference.digest
                    ),
                );
            }
        }
    }
    for proposal in &bundle.proposals {
        if proposal.verify_digest().is_err() {
            violation(
                &mut violations,
                "E1",
                format!("proposal {} digest mismatch", proposal.digest),
            );
        }
        if !sets.contains_key(&proposal.base_artifact_set)
            || !sets.contains_key(&proposal.candidate_artifact_set)
        {
            violation(
                &mut violations,
                "E3",
                format!(
                    "proposal {} references an unknown artifact set",
                    proposal.digest
                ),
            );
        }
    }
    for evaluation in &bundle.evaluations {
        if evaluation.verify_digest().is_err() {
            violation(
                &mut violations,
                "E1",
                format!("evaluation {} digest mismatch", evaluation.digest),
            );
        }
        match proposals.get(&evaluation.proposal) {
            Some(proposal)
                if proposal.base_artifact_set == evaluation.baseline_artifact_set
                    && proposal.candidate_artifact_set == evaluation.candidate_artifact_set => {}
            Some(_) => violation(
                &mut violations,
                "E4",
                format!(
                    "evaluation {} does not match its proposal",
                    evaluation.digest
                ),
            ),
            None => violation(
                &mut violations,
                "E4",
                format!(
                    "evaluation {} references an unknown proposal",
                    evaluation.digest
                ),
            ),
        }
        if evaluation.operation_ids.is_empty() || evaluation.evidence_refs.is_empty() {
            violation(
                &mut violations,
                "E5",
                format!("evaluation {} has incomplete evidence", evaluation.digest),
            );
        }
    }
    for fact in &bundle.facts {
        if fact.verify_digest().is_err() {
            violation(
                &mut violations,
                "E1",
                format!("fact {} digest mismatch", fact.digest),
            );
        }
        if !evaluations.contains_key(&fact.evaluation) {
            violation(
                &mut violations,
                "E5",
                format!("fact {} references an unknown evaluation", fact.digest),
            );
        }
        if fact
            .metrics
            .iter()
            .any(|metric| metric.baseline.is_empty() || metric.candidate.is_empty())
        {
            violation(
                &mut violations,
                "E6",
                format!("fact {} contains an incomplete metric", fact.digest),
            );
        }
    }
    for decision in &bundle.decisions {
        if decision.verify_digest().is_err() {
            violation(
                &mut violations,
                "E1",
                format!("decision {} digest mismatch", decision.digest),
            );
        }
        let Some(proposal) = proposals.get(&decision.proposal) else {
            violation(
                &mut violations,
                "E7",
                format!(
                    "decision {} references an unknown proposal",
                    decision.digest
                ),
            );
            continue;
        };
        if decision.evaluation_facts.is_empty() {
            violation(
                &mut violations,
                "E7",
                format!("decision {} has no evaluation facts", decision.digest),
            );
        }
        let referenced_facts: Vec<_> = decision
            .evaluation_facts
            .iter()
            .filter_map(|digest| facts.get(digest))
            .collect();
        if referenced_facts.len() != decision.evaluation_facts.len() {
            violation(
                &mut violations,
                "E7",
                format!("decision {} references missing facts", decision.digest),
            );
        }
        if decision.outcome == PromotionOutcome::Promote
            && (referenced_facts.is_empty()
                || referenced_facts.iter().any(|fact| {
                    !fact.replay_passed
                        || fact.gates.iter().any(|gate| gate.required && !gate.passed)
                }))
        {
            violation(
                &mut violations,
                "E7",
                format!(
                    "decision {} promotes without passing required gates",
                    decision.digest
                ),
            );
        }
        if decision.selected_artifact_set != proposal.candidate_artifact_set {
            violation(
                &mut violations,
                "E7",
                format!(
                    "decision {} selects a non-candidate artifact set",
                    decision.digest
                ),
            );
        }
    }
    for activation in &bundle.activations {
        if activation.verify_digest().is_err() {
            violation(
                &mut violations,
                "E1",
                format!("activation {} digest mismatch", activation.digest),
            );
        }
        match decisions.get(&activation.promotion_decision) {
            Some(decision)
                if decision.outcome == PromotionOutcome::Promote
                    && decision.selected_artifact_set == activation.artifact_set => {}
            Some(_) => violation(
                &mut violations,
                "E8",
                format!(
                    "activation {} is not backed by a promotion decision",
                    activation.digest
                ),
            ),
            None => violation(
                &mut violations,
                "E8",
                format!(
                    "activation {} references an unknown decision",
                    activation.digest
                ),
            ),
        }
    }

    EvolutionReport {
        schema: EVOLUTION_REPORT_SCHEMA.to_string(),
        verdict: if violations.is_empty() {
            EvolutionVerdict::Pass
        } else {
            EvolutionVerdict::Fail
        },
        violations,
    }
}

fn canonical_digest<T: Serialize>(value: &T) -> Result<ContentDigest, EvolutionError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| EvolutionError::Serialization(error.to_string()))?;
    Ok(ContentDigest::from_bytes(&bytes))
}

fn violation(violations: &mut Vec<EvolutionViolation>, code: &str, detail: String) {
    violations.push(EvolutionViolation {
        code: code.to_string(),
        detail,
    });
}

fn has_artifact_cycle(
    current: ContentDigest,
    artifacts: &HashMap<ContentDigest, &ArtifactVersion>,
    visiting: &mut HashSet<ContentDigest>,
    visited: &mut HashSet<ContentDigest>,
) -> bool {
    if visited.contains(&current) {
        return false;
    }
    if !visiting.insert(current.clone()) {
        return true;
    }
    let cycle = artifacts.get(&current).is_some_and(|artifact| {
        artifact
            .manifest
            .parents
            .iter()
            .any(|parent| has_artifact_cycle(parent.clone(), artifacts, visiting, visited))
    });
    visiting.remove(&current);
    visited.insert(current);
    cycle
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(label: &str) -> ContentDigest {
        ContentDigest::from_bytes(label.as_bytes())
    }

    fn artifact(kind: ArtifactKind, label: &str, parents: Vec<ContentDigest>) -> ArtifactVersion {
        ArtifactVersion::from_manifest(ArtifactManifest {
            kind,
            payload_digest: digest(&format!("payload:{label}")),
            parents,
            toolchain_digest: digest("toolchain:v4"),
            scope: "runtime".to_string(),
        })
        .unwrap()
    }

    #[test]
    fn artifact_and_set_digests_are_canonical() {
        let runtime = artifact(ArtifactKind::Runtime, "r1", Vec::new());
        let skill = artifact(ArtifactKind::Skill, "s1", Vec::new());
        let set = ArtifactSet::new(vec![skill.reference(), runtime.reference()]).unwrap();

        assert_eq!(set.artifacts[0].kind, ArtifactKind::Runtime);
        assert!(runtime.verify_digest().is_ok());
        assert!(set.verify_digest().is_ok());
        assert_eq!(
            ContentDigest::parse(set.digest.to_string()).unwrap(),
            set.digest
        );
    }

    #[test]
    fn content_digest_json_decode_rejects_non_sha256_values() {
        let invalid = serde_json::from_value::<ContentDigest>(serde_json::json!("legacy-id"));
        assert!(invalid.is_err());
    }

    #[test]
    fn tampering_is_fail_closed() {
        let runtime = artifact(ArtifactKind::Runtime, "r1", Vec::new());
        let mut tampered = runtime.clone();
        tampered.manifest.scope = "changed".to_string();
        let report = validate_evolution(&EvolutionBundle {
            artifacts: vec![tampered],
            ..Default::default()
        });

        assert_eq!(report.verdict, EvolutionVerdict::Fail);
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.code == "E1")
        );
    }

    #[test]
    fn proposal_evaluation_promotion_and_activation_form_one_valid_graph() {
        let base = artifact(ArtifactKind::Runtime, "base", Vec::new());
        let candidate = artifact(
            ArtifactKind::Runtime,
            "candidate",
            vec![base.digest.clone()],
        );
        let base_set = ArtifactSet::new(vec![base.reference()]).unwrap();
        let candidate_set = ArtifactSet::new(vec![candidate.reference()]).unwrap();
        let proposal = EvolutionProposal::new(
            base_set.digest.clone(),
            candidate_set.digest.clone(),
            "improve recovery",
            digest("change-manifest"),
            "agent:root",
            vec!["replay must pass".to_string()],
        )
        .unwrap();
        let evaluator = digest("evaluator");
        let dataset = digest("dataset");
        let evidence = digest("evidence");
        let operation_ids = vec!["eval-op".to_string()];
        let evidence_refs = vec![evidence.clone()];
        let run_body = EvaluationRunBody {
            proposal: &proposal.digest,
            baseline_artifact_set: &base_set.digest,
            candidate_artifact_set: &candidate_set.digest,
            evaluator: &evaluator,
            dataset: &dataset,
            operation_ids: &operation_ids,
            evidence_refs: &evidence_refs,
        };
        let evaluation = EvaluationRun {
            digest: canonical_digest(&run_body).unwrap(),
            proposal: proposal.digest.clone(),
            baseline_artifact_set: base_set.digest.clone(),
            candidate_artifact_set: candidate_set.digest.clone(),
            evaluator,
            dataset,
            operation_ids,
            evidence_refs,
        };
        let metrics = vec![EvaluationMetric {
            name: "quality".to_string(),
            baseline: "0.8".to_string(),
            candidate: "0.9".to_string(),
            improved: true,
        }];
        let gates = vec![EvaluationGate {
            name: "replay".to_string(),
            required: true,
            passed: true,
        }];
        let fact_body = EvaluationFactBody {
            evaluation: &evaluation.digest,
            metrics: &metrics,
            gates: &gates,
            replay_passed: true,
        };
        let fact = EvaluationFact {
            digest: canonical_digest(&fact_body).unwrap(),
            evaluation: evaluation.digest.clone(),
            metrics,
            gates,
            replay_passed: true,
        };
        let policy = digest("policy");
        let evaluation_facts = vec![fact.digest.clone()];
        let decision_body = PromotionDecisionBody {
            proposal: &proposal.digest,
            evaluation_facts: &evaluation_facts,
            policy: &policy,
            outcome: PromotionOutcome::Promote,
            selected_artifact_set: &candidate_set.digest,
        };
        let decision = PromotionDecision {
            digest: canonical_digest(&decision_body).unwrap(),
            proposal: proposal.digest.clone(),
            evaluation_facts,
            policy,
            outcome: PromotionOutcome::Promote,
            selected_artifact_set: candidate_set.digest.clone(),
        };
        let activation_body = (
            &"next-op".to_string(),
            &candidate_set.digest,
            &decision.digest,
        );
        let activation = ActivationBinding {
            digest: canonical_digest(&activation_body).unwrap(),
            operation_id: "next-op".to_string(),
            artifact_set: candidate_set.digest.clone(),
            promotion_decision: decision.digest.clone(),
        };
        let report = validate_evolution(&EvolutionBundle {
            artifacts: vec![base, candidate],
            artifact_sets: vec![base_set, candidate_set],
            proposals: vec![proposal],
            evaluations: vec![evaluation],
            facts: vec![fact],
            decisions: vec![decision],
            activations: vec![activation],
        });

        assert_eq!(report.verdict, EvolutionVerdict::Pass);
        assert!(report.violations.is_empty());
    }

    #[test]
    fn promotion_rejects_missing_required_gate() {
        let fact = EvaluationFact {
            digest: digest("fact"),
            evaluation: digest("evaluation"),
            metrics: Vec::new(),
            gates: vec![EvaluationGate {
                name: "safety".to_string(),
                required: true,
                passed: false,
            }],
            replay_passed: true,
        };
        let decision = PromotionDecision {
            digest: digest("decision"),
            proposal: digest("proposal"),
            evaluation_facts: vec![fact.digest.clone()],
            policy: digest("policy"),
            outcome: PromotionOutcome::Promote,
            selected_artifact_set: digest("candidate"),
        };
        let report = validate_evolution(&EvolutionBundle {
            facts: vec![fact],
            decisions: vec![decision],
            ..Default::default()
        });

        assert_eq!(report.verdict, EvolutionVerdict::Fail);
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.code == "E7")
        );
    }
}
