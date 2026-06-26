use crate::Error;
use crate::Result;

pub const LHA_BASE_URL_ENV_VAR: &str = "LHA_BASE_URL";
pub const LHA_API_KEY_ENV_VAR: &str = "LHA_API_KEY";
pub const LHA_MODEL_ENV_VAR: &str = "LHA_MODEL";
pub const LHA_ENDPOINT_ENV_VAR: &str = "LHA_ENDPOINT";

pub(crate) fn read_required_env_with_lookup(
    var: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<String> {
    lookup(var)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::EnvVar {
            var: var.to_string(),
            instructions: None,
        })
}
