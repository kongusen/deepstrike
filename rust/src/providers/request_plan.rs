use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use deepstrike_core::context::measurement::{MeasurementConfidence, MeasurementSource};

/// Provider-visible endpoint identity. Credentials and retry transport state are intentionally
/// excluded from this host-side request contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderRequestEndpoint {
    pub id: String,
    pub protocol: String,
    #[serde(rename = "baseURL")]
    pub base_url: String,
}

impl ProviderRequestEndpoint {
    pub fn new(
        id: impl Into<String>,
        protocol: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            protocol: protocol.into(),
            base_url: base_url.into(),
        }
    }
}

/// One material provider request. The fingerprint binds a preflight measurement to exactly the
/// model, endpoint, context, tools, and material request options that will be executed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderRequestPlan {
    pub version: u8,
    pub provider_id: String,
    pub model_id: String,
    pub endpoint: ProviderRequestEndpoint,
    pub context: Value,
    pub tools: Vec<Value>,
    pub options: Value,
    pub fingerprint: String,
}

impl ProviderRequestPlan {
    pub fn new(
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
        endpoint: ProviderRequestEndpoint,
        context: Value,
        tools: Vec<Value>,
        options: Value,
    ) -> Result<Self, RequestPlanError> {
        let provider_id = provider_id.into();
        let model_id = model_id.into();
        let options = material_options(options)?;
        let hashed = serde_json::json!({
            "version": 1,
            "providerId": provider_id,
            "modelId": model_id,
            "endpoint": {
                "id": endpoint.id,
                "protocol": endpoint.protocol,
                "baseURL": endpoint.base_url,
            },
            "context": context,
            "tools": tools,
            "options": options,
        });
        let fingerprint = format!(
            "sha256:{:x}",
            Sha256::digest(canonical_json(&hashed).as_bytes())
        );
        Ok(Self {
            version: 1,
            provider_id,
            model_id,
            endpoint,
            context: hashed["context"].clone(),
            tools: hashed["tools"].as_array().cloned().unwrap_or_default(),
            options: hashed["options"].clone(),
            fingerprint,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordedPromptMeasurement {
    pub version: u8,
    pub request_fingerprint: String,
    pub input_tokens: u64,
    pub source: MeasurementSource,
    pub confidence: MeasurementConfidence,
}

pub type PromptMeasurementSource = MeasurementSource;

pub fn record_prompt_measurement(
    plan: &ProviderRequestPlan,
    input_tokens: u64,
    source: MeasurementSource,
    confidence: MeasurementConfidence,
) -> RecordedPromptMeasurement {
    RecordedPromptMeasurement {
        version: 1,
        request_fingerprint: plan.fingerprint.clone(),
        input_tokens,
        source,
        confidence,
    }
}

pub fn measurement_for_plan(
    plan: &ProviderRequestPlan,
    measurement: &RecordedPromptMeasurement,
) -> Option<RecordedPromptMeasurement> {
    (measurement.version == 1 && measurement.request_fingerprint == plan.fingerprint)
        .then(|| measurement.clone())
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub reasoning_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedProviderUsage {
    pub input_tokens: u64,
    pub uncached_input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub reasoning_tokens: Option<u64>,
}

pub fn normalize_provider_usage(
    usage: &ProviderUsage,
) -> Result<NormalizedProviderUsage, RequestPlanError> {
    let cached = usage
        .cache_read_input_tokens
        .checked_add(usage.cache_creation_input_tokens)
        .ok_or(RequestPlanError::InvalidUsage)?;
    if cached > usage.input_tokens
        || usage
            .reasoning_tokens
            .is_some_and(|tokens| tokens > usage.output_tokens)
    {
        return Err(RequestPlanError::InvalidUsage);
    }
    Ok(NormalizedProviderUsage {
        input_tokens: usage.input_tokens,
        uncached_input_tokens: usage.input_tokens - cached,
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        reasoning_tokens: usage.reasoning_tokens,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct PricingRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_creation: Option<f64>,
    pub reasoning: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PricingSnapshot {
    pub version: String,
    pub currency: String,
    pub region: String,
    pub effective_from: String,
    pub expires_at: Option<String>,
    pub rates_per_million: PricingRates,
}

impl PricingSnapshot {
    pub fn new(
        version: impl Into<String>,
        currency: impl Into<String>,
        region: impl Into<String>,
        effective_from: impl Into<String>,
        expires_at: Option<String>,
        rates_per_million: PricingRates,
    ) -> Self {
        Self {
            version: version.into(),
            currency: currency.into(),
            region: region.into(),
            effective_from: effective_from.into(),
            expires_at,
            rates_per_million,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CostObservation {
    Snapshot {
        currency: String,
        amount: f64,
        pricing_version: String,
    },
    Unpriced {
        reason: UnpricedReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnpricedReason {
    SnapshotNotEffective,
    SnapshotExpired,
    InvalidPricingSnapshot,
}

pub fn price_provider_usage(
    usage: &NormalizedProviderUsage,
    snapshot: &PricingSnapshot,
    observed_at: &str,
) -> CostObservation {
    let valid_rates = [
        Some(snapshot.rates_per_million.input),
        Some(snapshot.rates_per_million.output),
        snapshot.rates_per_million.cache_read,
        snapshot.rates_per_million.cache_creation,
        snapshot.rates_per_million.reasoning,
    ]
    .into_iter()
    .flatten()
    .all(|rate| rate.is_finite() && rate >= 0.0);
    let observed_at = parse_rfc3339(observed_at);
    let effective_from = parse_rfc3339(&snapshot.effective_from);
    let expires_at = snapshot
        .expires_at
        .as_deref()
        .map(parse_rfc3339)
        .transpose();
    if snapshot.version.is_empty()
        || snapshot.currency.is_empty()
        || !valid_rates
        || observed_at.is_err()
        || effective_from.is_err()
        || expires_at.is_err()
    {
        return CostObservation::Unpriced {
            reason: UnpricedReason::InvalidPricingSnapshot,
        };
    }
    let observed_at = observed_at.expect("checked above");
    let effective_from = effective_from.expect("checked above");
    if observed_at < effective_from {
        return CostObservation::Unpriced {
            reason: UnpricedReason::SnapshotNotEffective,
        };
    }
    if expires_at
        .expect("checked above")
        .is_some_and(|expires| observed_at >= expires)
    {
        return CostObservation::Unpriced {
            reason: UnpricedReason::SnapshotExpired,
        };
    }
    let rates = &snapshot.rates_per_million;
    let amount = (usage.uncached_input_tokens as f64 * rates.input
        + usage.output_tokens as f64 * rates.output
        + usage.cache_read_input_tokens as f64 * rates.cache_read.unwrap_or(rates.input)
        + usage.cache_creation_input_tokens as f64 * rates.cache_creation.unwrap_or(rates.input)
        + usage.reasoning_tokens.unwrap_or_default() as f64 * rates.reasoning.unwrap_or(0.0))
        / 1_000_000.0;
    CostObservation::Snapshot {
        currency: snapshot.currency.clone(),
        amount,
        pricing_version: snapshot.version.clone(),
    }
}

fn parse_rfc3339(value: &str) -> Result<OffsetDateTime, time::error::Parse> {
    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RequestPlanError {
    #[error("request plan options must be a JSON object")]
    OptionsMustBeObject,
    #[error("provider usage has inconsistent token subsets")]
    InvalidUsage,
}

fn material_options(options: Value) -> Result<Value, RequestPlanError> {
    let Value::Object(object) = options else {
        return Err(RequestPlanError::OptionsMustBeObject);
    };
    Ok(sanitize_value(Value::Object(object)).unwrap_or(Value::Object(Default::default())))
}

fn sanitize_value(value: Value) -> Option<Value> {
    match value {
        Value::Array(values) => Some(Value::Array(
            values.into_iter().filter_map(sanitize_value).collect(),
        )),
        Value::Object(values) => Some(Value::Object(
            values
                .into_iter()
                .filter_map(|(key, value)| {
                    (!transport_only_key(&key))
                        .then(|| sanitize_value(value).map(|value| (key, value)))
                        .flatten()
                })
                .collect(),
        )),
        value => Some(value),
    }
}

fn transport_only_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(|character| character.to_lowercase())
        .collect();
    matches!(normalized.as_str(), "retry" | "maxretries" | "basedelay" | "timeout" | "signal")
        || normalized.contains("authorization")
        || normalized.contains("credential")
        || normalized.contains("accesstoken")
        || normalized.contains("refreshtoken")
        || normalized.contains("apikey")
        || matches!(normalized.as_str(), "bearer" | "token" | "secret" | "xapikey")
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).expect("strings serialize"),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!(
                    "{}:{}",
                    serde_json::to_string(key).expect("keys serialize"),
                    canonical_json(value)
                ))
                .collect::<Vec<_>>()
                .join(","),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_shared_cross_sdk_sha256_fixture() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/provider-request-plan/v1.json"
        ))
        .expect("fixture is valid JSON");
        let input = fixture.get("input").expect("fixture input");
        let endpoint = input.get("endpoint").expect("fixture endpoint");
        let plan = ProviderRequestPlan::new(
            input["providerId"].as_str().unwrap(),
            input["modelId"].as_str().unwrap(),
            ProviderRequestEndpoint::new(
                endpoint["id"].as_str().unwrap(),
                endpoint["protocol"].as_str().unwrap(),
                endpoint["baseURL"].as_str().unwrap(),
            ),
            input["context"].clone(),
            input["tools"].as_array().unwrap().clone(),
            input["options"].clone(),
        )
        .expect("valid request plan");

        assert_eq!(plan.fingerprint, fixture["fingerprint"].as_str().unwrap());
        assert_eq!(
            plan.options,
            serde_json::json!({"auth": {"mode": "request"}, "temperature": 0.2, "transport": {}})
        );
        assert!(
            !serde_json::to_string(&plan)
                .unwrap()
                .contains("must-not-hash")
        );
    }

    #[test]
    fn records_measurements_and_prices_only_valid_snapshots() {
        let plan = ProviderRequestPlan::new(
            "openai",
            "gpt-4o",
            ProviderRequestEndpoint::new("openai.chat", "openai-chat", "https://api.openai.com/v1"),
            serde_json::json!({"systemText": "s", "turns": []}),
            vec![],
            serde_json::json!({}),
        )
        .unwrap();
        let measurement = record_prompt_measurement(
            &plan,
            42,
            PromptMeasurementSource::Native {
                provider: "openai".into(),
            },
            MeasurementConfidence::Exact,
        );
        assert_eq!(measurement_for_plan(&plan, &measurement), Some(measurement));

        let usage = normalize_provider_usage(&ProviderUsage {
            input_tokens: 120,
            output_tokens: 30,
            cache_read_input_tokens: 20,
            cache_creation_input_tokens: 10,
            reasoning_tokens: Some(6),
        })
        .unwrap();
        assert_eq!(usage.uncached_input_tokens, 90);
        assert_eq!(
            price_provider_usage(
                &usage,
                &PricingSnapshot::new(
                    "v1",
                    "USD",
                    "global",
                    "2026-08-01T00:00:00Z",
                    Some("2026-09-01T00:00:00Z".into()),
                    PricingRates {
                        input: 2.0,
                        output: 8.0,
                        cache_read: Some(0.2),
                        cache_creation: Some(2.5),
                        reasoning: None,
                    },
                ),
                "2026-08-13T00:00:00Z",
            ),
            CostObservation::Snapshot {
                currency: "USD".into(),
                amount: 0.000449,
                pricing_version: "v1".into()
            },
        );
    }

    #[test]
    fn redacts_nested_case_and_separator_variant_credentials() {
        let plan = ProviderRequestPlan::new(
            "openai",
            "gpt-4o",
            ProviderRequestEndpoint::new("openai.chat", "openai-chat", "https://api.openai.com/v1"),
            serde_json::json!({"turns": []}),
            vec![],
            serde_json::json!({"headers": {"Authorization": "secret", "x-api-key": "secret"}, "access_token": "secret", "temperature": 0.2}),
        )
        .unwrap();
        assert_eq!(plan.options, serde_json::json!({"temperature": 0.2, "headers": {}}));
        assert!(!serde_json::to_string(&plan).unwrap().contains("secret"));
    }
}
