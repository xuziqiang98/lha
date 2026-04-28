use std::path::Path;
use std::path::PathBuf;

use adam_protocol::ThreadId;
use tracing::error;

use crate::parse_command::shlex_join;

/// Emit structured feedback metadata as key/value pairs.
///
/// This logs a tracing event with `target: "feedback_tags"`. If
/// `adam_feedback::CodexFeedback::metadata_layer()` is installed, these fields are captured and
/// later attached as tags when feedback is uploaded.
///
/// Values are wrapped with [`tracing::field::DebugValue`], so the expression only needs to
/// implement [`std::fmt::Debug`].
///
/// Example:
///
/// ```rust
/// adam_agent::feedback_tags!(model = "gpt-5", cached = true);
/// adam_agent::feedback_tags!(provider = provider_id, request_id = request_id);
/// ```
#[macro_export]
macro_rules! feedback_tags {
    ($( $key:ident = $value:expr ),+ $(,)?) => {
        ::tracing::info!(
            target: "feedback_tags",
            $( $key = ::tracing::field::debug(&$value) ),+
        );
    };
}

pub(crate) fn error_or_panic(message: impl std::string::ToString) {
    if cfg!(debug_assertions) {
        panic!("{}", message.to_string());
    } else {
        error!("{}", message.to_string());
    }
}

pub fn resolve_path(base: &Path, path: &PathBuf) -> PathBuf {
    if path.is_absolute() {
        path.clone()
    } else {
        base.join(path)
    }
}

/// Trim a thread name and return `None` if it is empty after trimming.
pub fn normalize_thread_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn resume_command(thread_name: Option<&str>, thread_id: Option<ThreadId>) -> Option<String> {
    let resume_target = thread_name
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| thread_id.map(|thread_id| thread_id.to_string()));
    resume_target.map(|target| {
        let needs_double_dash = target.starts_with('-');
        let escaped = shlex_join(&[target]);
        if needs_double_dash {
            format!("adam resume -- {escaped}")
        } else {
            format!("adam resume {escaped}")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_tags_macro_compiles() {
        #[derive(Debug)]
        struct OnlyDebug;

        feedback_tags!(model = "gpt-5", cached = true, debug_only = OnlyDebug);
    }

    #[test]
    fn normalize_thread_name_trims_and_rejects_empty() {
        assert_eq!(normalize_thread_name("   "), None);
        assert_eq!(
            normalize_thread_name("  my thread  "),
            Some("my thread".to_string())
        );
    }

    #[test]
    fn resume_command_prefers_name_over_id() {
        let thread_id = ThreadId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();
        let command = resume_command(Some("my-thread"), Some(thread_id));
        assert_eq!(command, Some("adam resume my-thread".to_string()));
    }

    #[test]
    fn resume_command_with_only_id() {
        let thread_id = ThreadId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();
        let command = resume_command(None, Some(thread_id));
        assert_eq!(
            command,
            Some("adam resume 123e4567-e89b-12d3-a456-426614174000".to_string())
        );
    }

    #[test]
    fn resume_command_with_no_name_or_id() {
        let command = resume_command(None, None);
        assert_eq!(command, None);
    }

    #[test]
    fn resume_command_quotes_thread_name_when_needed() {
        let command = resume_command(Some("-starts-with-dash"), None);
        assert_eq!(
            command,
            Some("adam resume -- -starts-with-dash".to_string())
        );

        let command = resume_command(Some("two words"), None);
        assert_eq!(command, Some("adam resume 'two words'".to_string()));

        let command = resume_command(Some("quote'case"), None);
        assert_eq!(command, Some("adam resume \"quote'case\"".to_string()));
    }
}
