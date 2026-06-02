use regex::Regex;
use std::sync::LazyLock;

const REDACTED_SECRET: &str = "[REDACTED_SECRET]";

static OPENAI_KEY_REGEX: LazyLock<Regex> = LazyLock::new(|| compile_regex(r"sk-[A-Za-z0-9]{20,}"));
static AWS_ACCESS_KEY_ID_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile_regex(r"\bAKIA[0-9A-Z]{16}\b"));
static BEARER_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| compile_regex(r"(?i)\bBearer\s+[A-Za-z0-9._\-]{16,}\b"));
static SECRET_ASSIGNMENT_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    compile_regex(r#"(?i)\b(api[_-]?key|token|secret|password)\b(\s*[:=]\s*)(["']?)[^\s"']{8,}"#)
});

/// Best-effort redaction for common key and token patterns.
pub fn redact_secrets(input: String) -> String {
    let redacted = OPENAI_KEY_REGEX.replace_all(&input, REDACTED_SECRET);
    let redacted = AWS_ACCESS_KEY_ID_REGEX.replace_all(&redacted, REDACTED_SECRET);
    let redacted = BEARER_TOKEN_REGEX.replace_all(&redacted, "Bearer [REDACTED_SECRET]");
    let redacted = SECRET_ASSIGNMENT_REGEX.replace_all(&redacted, "$1$2$3[REDACTED_SECRET]");

    redacted.to_string()
}

fn compile_regex(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|err| panic!("invalid regex pattern `{pattern}`: {err}"))
}

#[cfg(test)]
mod tests {
    use super::redact_secrets;
    use pretty_assertions::assert_eq;

    #[test]
    fn redacts_openai_keys() {
        assert_eq!(
            redact_secrets("key sk-abcdefghijklmnopqrstuvwx".to_string()),
            "key [REDACTED_SECRET]"
        );
    }

    #[test]
    fn redacts_aws_keys() {
        assert_eq!(
            redact_secrets("AKIAABCDEFGHIJKLMNOP".to_string()),
            "[REDACTED_SECRET]"
        );
    }

    #[test]
    fn redacts_bearer_tokens() {
        assert_eq!(
            redact_secrets("Authorization: Bearer abcdefghijklmnop".to_string()),
            "Authorization: Bearer [REDACTED_SECRET]"
        );
    }

    #[test]
    fn redacts_assignment_style_secrets() {
        assert_eq!(
            redact_secrets("api_key = \"abcdefghijkl\"".to_string()),
            "api_key = \"[REDACTED_SECRET]\""
        );
    }
}
