use std::collections::HashMap;

use crate::Personality;
use crate::Verbosity;
use crate::env::LHA_MODEL_ENV_VAR;
use crate::env::read_required_env_with_lookup;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde::de;
use strum::IntoEnumIterator;
use strum_macros::Display;
use strum_macros::EnumIter;
use tracing::warn;
use ts_rs::TS;

const PERSONALITY_PLACEHOLDER: &str = "{{ personality }}";

#[derive(
    Debug,
    Serialize,
    Deserialize,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Display,
    JsonSchema,
    TS,
    EnumIter,
    Hash,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    XHigh,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema, PartialEq, Eq)]
pub struct ReasoningEffortPreset {
    pub effort: ReasoningEffort,
    pub description: String,
}

#[derive(
    Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS, JsonSchema, EnumIter, Display,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ModelVisibility {
    List,
    Hide,
    None,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TruncationMode {
    Bytes,
    Tokens,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS, JsonSchema)]
pub struct TruncationPolicyConfig {
    pub mode: TruncationMode,
    pub limit: i64,
}

impl TruncationPolicyConfig {
    pub const fn bytes(limit: i64) -> Self {
        Self {
            mode: TruncationMode::Bytes,
            limit,
        }
    }

    pub const fn tokens(limit: i64) -> Self {
        Self {
            mode: TruncationMode::Tokens,
            limit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
#[ts(type = "number")]
pub struct UsdPerMillionTokensMicros(i64);

impl UsdPerMillionTokensMicros {
    const MICROS_PER_USD: i64 = 1_000_000;

    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    pub const fn as_micros(self) -> i64 {
        self.0
    }

    pub fn as_usd(self) -> f64 {
        self.0 as f64 / Self::MICROS_PER_USD as f64
    }
}

impl Serialize for UsdPerMillionTokensMicros {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(self.as_usd())
    }
}

impl JsonSchema for UsdPerMillionTokensMicros {
    fn schema_name() -> String {
        "UsdPerMillionTokensMicros".to_string()
    }

    fn is_referenceable() -> bool {
        false
    }

    fn json_schema(schema_gen: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        f64::json_schema(schema_gen)
    }
}

impl<'de> Deserialize<'de> for UsdPerMillionTokensMicros {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UsdPerMillionTokensMicrosVisitor)
    }
}

struct UsdPerMillionTokensMicrosVisitor;

impl de::Visitor<'_> for UsdPerMillionTokensMicrosVisitor {
    type Value = UsdPerMillionTokensMicros;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a non-negative USD price as a number or decimal string")
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value < 0 {
            return Err(E::custom("USD price must be non-negative"));
        }
        value
            .checked_mul(UsdPerMillionTokensMicros::MICROS_PER_USD)
            .map(UsdPerMillionTokensMicros)
            .ok_or_else(|| E::custom("USD price is too large"))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        let value = i64::try_from(value).map_err(|_| E::custom("USD price is too large"))?;
        self.visit_i64(value)
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if !value.is_finite() || value < 0.0 {
            return Err(E::custom("USD price must be a finite non-negative number"));
        }
        let micros = (value * UsdPerMillionTokensMicros::MICROS_PER_USD as f64).round();
        if !micros.is_finite() || micros > i64::MAX as f64 {
            return Err(E::custom("USD price is too large"));
        }
        Ok(UsdPerMillionTokensMicros(micros as i64))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        parse_usd_price_micros(value).map_err(E::custom)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_str(&value)
    }
}

fn parse_usd_price_micros(value: &str) -> Result<UsdPerMillionTokensMicros, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("USD price must not be empty".to_string());
    }
    if value.starts_with('-') {
        return Err("USD price must be non-negative".to_string());
    }

    let mut parts = value.split('.');
    let dollars = parts.next().unwrap_or_default();
    let fractional = parts.next();
    if parts.next().is_some() {
        return Err("USD price must be a decimal number".to_string());
    }
    if dollars.is_empty() && fractional.is_none() {
        return Err("USD price must be a decimal number".to_string());
    }
    if !dollars.chars().all(|ch| ch.is_ascii_digit()) {
        return Err("USD price must be a decimal number".to_string());
    }

    let dollars = if dollars.is_empty() {
        0
    } else {
        dollars
            .parse::<i64>()
            .map_err(|_| "USD price is too large".to_string())?
    };
    let dollars_micros = dollars
        .checked_mul(UsdPerMillionTokensMicros::MICROS_PER_USD)
        .ok_or_else(|| "USD price is too large".to_string())?;

    let fractional_micros = if let Some(fractional) = fractional {
        if fractional.len() > 6 {
            return Err("USD price supports at most 6 decimal places".to_string());
        }
        if !fractional.chars().all(|ch| ch.is_ascii_digit()) {
            return Err("USD price must be a decimal number".to_string());
        }
        let padded = format!("{fractional:0<6}");
        padded
            .parse::<i64>()
            .map_err(|_| "USD price is too large".to_string())?
    } else {
        0
    };

    dollars_micros
        .checked_add(fractional_micros)
        .map(UsdPerMillionTokensMicros)
        .ok_or_else(|| "USD price is too large".to_string())
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS, JsonSchema)]
pub enum ModelPricingCurrency {
    #[serde(rename = "USD")]
    Usd,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelPricingUnit {
    UsdPerMillionTokens,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelPricingBilling {
    Standard,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema)]
pub struct ModelPricing {
    pub currency: ModelPricingCurrency,
    pub unit: ModelPricingUnit,
    pub billing: ModelPricingBilling,
    pub context_bands: Vec<ModelPricingBand>,
}

impl ModelPricing {
    pub fn band_for_input_tokens(&self, input_tokens: i64) -> Option<&ModelPricingBand> {
        if input_tokens < 0 {
            return None;
        }
        self.context_bands
            .iter()
            .find(|band| band.matches_input_tokens(input_tokens))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema)]
pub struct ModelPricingBand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<i64>,
    pub input: UsdPerMillionTokensMicros,
    pub cached_input: UsdPerMillionTokensMicros,
    pub output: UsdPerMillionTokensMicros,
}

impl ModelPricingBand {
    pub fn matches_input_tokens(&self, input_tokens: i64) -> bool {
        if input_tokens < 0 {
            return false;
        }
        if let Some(min_input_tokens) = self.min_input_tokens
            && input_tokens < min_input_tokens
        {
            return false;
        }
        if let Some(max_input_tokens) = self.max_input_tokens
            && input_tokens > max_input_tokens
        {
            return false;
        }
        true
    }
}

const fn default_effective_context_window_percent() -> i64 {
    95
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema)]
pub struct ModelInfo {
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_level: Option<ReasoningEffort>,
    pub supported_reasoning_levels: Vec<ReasoningEffortPreset>,
    pub visibility: ModelVisibility,
    pub supported_in_api: bool,
    pub priority: i32,
    pub upgrade: Option<ModelInfoUpgrade>,
    pub base_instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_messages: Option<ModelMessages>,
    pub supports_reasoning_summaries: bool,
    pub support_verbosity: bool,
    pub default_verbosity: Option<Verbosity>,
    pub truncation_policy: TruncationPolicyConfig,
    pub supports_parallel_tool_calls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_token_limit: Option<i64>,
    #[serde(default = "default_effective_context_window_percent")]
    pub effective_context_window_percent: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
}

impl ModelInfo {
    pub fn minimal(model: impl Into<String>) -> Self {
        let model = model.into();
        Self {
            slug: model.clone(),
            display_name: model,
            description: None,
            default_reasoning_level: None,
            supported_reasoning_levels: Vec::new(),
            visibility: ModelVisibility::List,
            supported_in_api: true,
            priority: 0,
            upgrade: None,
            base_instructions: "You are a helpful assistant.".to_string(),
            model_messages: None,
            supports_reasoning_summaries: false,
            support_verbosity: false,
            default_verbosity: None,
            truncation_policy: TruncationPolicyConfig::bytes(10_000),
            supports_parallel_tool_calls: false,
            context_window: None,
            auto_compact_token_limit: None,
            effective_context_window_percent: default_effective_context_window_percent(),
            pricing: None,
        }
    }

    pub fn minimal_from_lha_env() -> crate::Result<Self> {
        Self::minimal_from_lha_env_with_lookup(|var| std::env::var(var).ok())
    }

    pub(crate) fn minimal_from_lha_env_with_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> crate::Result<Self> {
        read_required_env_with_lookup(LHA_MODEL_ENV_VAR, lookup).map(Self::minimal)
    }

    pub fn auto_compact_token_limit(&self) -> Option<i64> {
        self.auto_compact_token_limit.or_else(|| {
            self.context_window
                .map(|context_window| (context_window * 9) / 10)
        })
    }

    pub fn supports_personality(&self) -> bool {
        self.model_messages
            .as_ref()
            .is_some_and(ModelMessages::supports_personality)
    }

    pub fn get_model_instructions(&self, personality: Option<Personality>) -> String {
        if let Some(model_messages) = &self.model_messages
            && let Some(template) = &model_messages.instructions_template
        {
            let personality_message = model_messages
                .get_personality_message(personality)
                .unwrap_or_default();
            template.replace(PERSONALITY_PLACEHOLDER, personality_message.as_str())
        } else if let Some(personality) = personality {
            warn!(
                model = %self.slug,
                %personality,
                "Model personality requested but model_messages is missing, falling back to base instructions."
            );
            self.base_instructions.clone()
        } else {
            self.base_instructions.clone()
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema)]
pub struct ModelMessages {
    pub instructions_template: Option<String>,
    pub instructions_variables: Option<ModelInstructionsVariables>,
}

impl ModelMessages {
    fn has_personality_placeholder(&self) -> bool {
        self.instructions_template
            .as_ref()
            .map(|spec| spec.contains(PERSONALITY_PLACEHOLDER))
            .unwrap_or(false)
    }

    fn supports_personality(&self) -> bool {
        self.has_personality_placeholder()
            && self
                .instructions_variables
                .as_ref()
                .is_some_and(ModelInstructionsVariables::is_complete)
    }

    pub fn get_personality_message(&self, personality: Option<Personality>) -> Option<String> {
        self.instructions_variables
            .as_ref()
            .and_then(|variables| variables.get_personality_message(personality))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema)]
pub struct ModelInstructionsVariables {
    pub personality_default: Option<String>,
    pub personality_friendly: Option<String>,
    pub personality_pragmatic: Option<String>,
}

impl ModelInstructionsVariables {
    pub fn is_complete(&self) -> bool {
        self.personality_default.is_some()
            && self.personality_friendly.is_some()
            && self.personality_pragmatic.is_some()
    }

    pub fn get_personality_message(&self, personality: Option<Personality>) -> Option<String> {
        if let Some(personality) = personality {
            match personality {
                Personality::Friendly => self.personality_friendly.clone(),
                Personality::Pragmatic => self.personality_pragmatic.clone(),
            }
        } else {
            self.personality_default.clone()
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema)]
pub struct ModelInfoUpgrade {
    pub model: String,
    pub migration_markdown: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema, Default)]
pub struct ModelsResponse {
    pub models: Vec<ModelInfo>,
}

pub fn reasoning_effort_mapping_from_presets(
    presets: &[ReasoningEffortPreset],
) -> Option<HashMap<ReasoningEffort, ReasoningEffort>> {
    if presets.is_empty() {
        return None;
    }

    let supported: Vec<ReasoningEffort> = presets.iter().map(|p| p.effort).collect();
    let mut map = HashMap::new();
    for effort in ReasoningEffort::iter() {
        let nearest = nearest_effort(effort, &supported);
        map.insert(effort, nearest);
    }
    Some(map)
}

fn effort_rank(effort: ReasoningEffort) -> i32 {
    match effort {
        ReasoningEffort::None => 0,
        ReasoningEffort::Minimal => 1,
        ReasoningEffort::Low => 2,
        ReasoningEffort::Medium => 3,
        ReasoningEffort::High => 4,
        ReasoningEffort::XHigh => 5,
    }
}

fn nearest_effort(target: ReasoningEffort, supported: &[ReasoningEffort]) -> ReasoningEffort {
    let target_rank = effort_rank(target);
    supported
        .iter()
        .copied()
        .min_by_key(|candidate| (effort_rank(*candidate) - target_rank).abs())
        .unwrap_or(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn price_accepts_number_and_string_as_micro_usd_fixed_point() {
        let number: UsdPerMillionTokensMicros =
            serde_json::from_value(json!(2.50)).expect("number price parses");
        let string: UsdPerMillionTokensMicros =
            serde_json::from_value(json!("2.50")).expect("string price parses");

        assert_eq!(number, UsdPerMillionTokensMicros::from_micros(2_500_000));
        assert_eq!(string, number);
        assert_eq!(
            serde_json::to_value(number).expect("price serializes"),
            json!(2.5)
        );
    }

    #[test]
    fn price_schema_is_usd_number_not_internal_micros_integer() {
        let schema = schemars::schema_for!(UsdPerMillionTokensMicros);
        let schema_value = serde_json::to_value(&schema.schema).expect("schema serializes");

        assert_eq!(schema_value.get("type"), Some(&json!("number")));
        assert_ne!(schema_value.get("type"), Some(&json!("integer")));
        assert_ne!(schema_value.get("format"), Some(&json!("int64")));
    }

    #[test]
    fn pricing_band_match_is_inclusive_and_ordered() {
        let pricing = ModelPricing {
            currency: ModelPricingCurrency::Usd,
            unit: ModelPricingUnit::UsdPerMillionTokens,
            billing: ModelPricingBilling::Standard,
            context_bands: vec![
                ModelPricingBand {
                    min_input_tokens: None,
                    max_input_tokens: Some(272_000),
                    input: UsdPerMillionTokensMicros::from_micros(2_500_000),
                    cached_input: UsdPerMillionTokensMicros::from_micros(250_000),
                    output: UsdPerMillionTokensMicros::from_micros(15_000_000),
                },
                ModelPricingBand {
                    min_input_tokens: Some(272_001),
                    max_input_tokens: None,
                    input: UsdPerMillionTokensMicros::from_micros(5_000_000),
                    cached_input: UsdPerMillionTokensMicros::from_micros(500_000),
                    output: UsdPerMillionTokensMicros::from_micros(22_500_000),
                },
            ],
        };

        assert_eq!(
            pricing
                .band_for_input_tokens(272_000)
                .map(|band| band.input),
            Some(UsdPerMillionTokensMicros::from_micros(2_500_000))
        );
        assert_eq!(
            pricing
                .band_for_input_tokens(272_001)
                .map(|band| band.input),
            Some(UsdPerMillionTokensMicros::from_micros(5_000_000))
        );
    }

    #[test]
    fn model_info_minimal_uses_conservative_defaults() {
        let model_info = ModelInfo::minimal("test-model");

        assert_eq!(
            model_info,
            ModelInfo {
                slug: "test-model".to_string(),
                display_name: "test-model".to_string(),
                description: None,
                default_reasoning_level: None,
                supported_reasoning_levels: Vec::new(),
                visibility: ModelVisibility::List,
                supported_in_api: true,
                priority: 0,
                upgrade: None,
                base_instructions: "You are a helpful assistant.".to_string(),
                model_messages: None,
                supports_reasoning_summaries: false,
                support_verbosity: false,
                default_verbosity: None,
                truncation_policy: TruncationPolicyConfig::bytes(10_000),
                supports_parallel_tool_calls: false,
                context_window: None,
                auto_compact_token_limit: None,
                effective_context_window_percent: 95,
                pricing: None,
            }
        );
    }
}
