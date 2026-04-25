use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::fmt;

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub struct ModelRef {
    pub provider_id: String,
    pub endpoint_id: String,
    pub model_id: String,
}

impl ModelRef {
    pub fn new(
        provider_id: impl Into<String>,
        endpoint_id: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            endpoint_id: endpoint_id.into(),
            model_id: model_id.into(),
        }
    }

    pub fn parse(value: &str) -> Result<Self, ModelRefParseError> {
        let value = value.trim();
        let (endpoint_ref, model_id) = value
            .split_once(':')
            .ok_or(ModelRefParseError::MissingSeparator)?;
        let (provider_id, endpoint_id) = endpoint_ref
            .split_once('.')
            .ok_or(ModelRefParseError::MissingEndpoint)?;
        Self::validate_part(provider_id, ModelRefParseError::EmptyProvider)?;
        Self::validate_part(endpoint_id, ModelRefParseError::EmptyEndpoint)?;
        if model_id.trim().is_empty() {
            return Err(ModelRefParseError::EmptyModel);
        }
        Ok(Self::new(
            provider_id.trim(),
            endpoint_id.trim(),
            model_id.trim(),
        ))
    }

    pub fn parse_or_openai_main(value: &str) -> Result<Self, ModelRefParseError> {
        if value.contains(':') {
            Self::parse(value)
        } else if value.trim().is_empty() {
            Err(ModelRefParseError::EmptyModel)
        } else {
            Ok(Self::new("openai", "main", value.trim()))
        }
    }

    pub fn endpoint_ref(&self) -> String {
        format!("{}.{}", self.provider_id, self.endpoint_id)
    }

    fn validate_part(
        value: &str,
        empty_error: ModelRefParseError,
    ) -> Result<(), ModelRefParseError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(empty_error);
        }
        if value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            Ok(())
        } else {
            Err(ModelRefParseError::InvalidIdentifier(value.to_string()))
        }
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}:{}",
            self.provider_id, self.endpoint_id, self.model_id
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRefParseError {
    MissingSeparator,
    MissingEndpoint,
    EmptyProvider,
    EmptyEndpoint,
    EmptyModel,
    InvalidIdentifier(String),
}

impl fmt::Display for ModelRefParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSeparator => write!(f, "model ref must be provider.endpoint:model"),
            Self::MissingEndpoint => {
                write!(f, "model ref must include provider.endpoint before ':'")
            }
            Self::EmptyProvider => write!(f, "model ref provider is empty"),
            Self::EmptyEndpoint => write!(f, "model ref endpoint is empty"),
            Self::EmptyModel => write!(f, "model ref model is empty"),
            Self::InvalidIdentifier(value) => write!(
                f,
                "model ref identifier `{value}` contains invalid characters"
            ),
        }
    }
}

impl std::error::Error for ModelRefParseError {}

#[cfg(test)]
mod tests {
    use super::ModelRef;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_canonical_ref() {
        let model_ref = ModelRef::parse("openrouter.main:anthropic/claude-sonnet-4").unwrap();
        assert_eq!(model_ref.provider_id, "openrouter");
        assert_eq!(model_ref.endpoint_id, "main");
        assert_eq!(model_ref.model_id, "anthropic/claude-sonnet-4");
        assert_eq!(
            model_ref.to_string(),
            "openrouter.main:anthropic/claude-sonnet-4"
        );
    }

    #[test]
    fn bare_model_defaults_to_openai_main() {
        let model_ref = ModelRef::parse_or_openai_main("gpt-5.2").unwrap();
        assert_eq!(model_ref.to_string(), "openai.main:gpt-5.2");
    }

    #[test]
    fn rejects_invalid_provider_identifier() {
        let err = ModelRef::parse("openrouter#main.main:claude").unwrap_err();
        assert_eq!(
            err.to_string(),
            "model ref identifier `openrouter#main` contains invalid characters"
        );
    }
}
