/// Review thread system prompt. Edit `agent/src/review_prompt.md` to customize.
pub const REVIEW_PROMPT: &str = include_str!("../review_prompt.md");

// Centralized templates for review-related user messages
pub const REVIEW_EXIT_SUCCESS_TMPL: &str = include_str!("../templates/review/exit_success.xml");
pub const REVIEW_EXIT_INTERRUPTED_TMPL: &str =
    include_str!("../templates/review/exit_interrupted.xml");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_prompt_is_available() {
        assert!(!REVIEW_PROMPT.is_empty());
        assert!(!REVIEW_EXIT_SUCCESS_TMPL.is_empty());
        assert!(!REVIEW_EXIT_INTERRUPTED_TMPL.is_empty());
    }
}
