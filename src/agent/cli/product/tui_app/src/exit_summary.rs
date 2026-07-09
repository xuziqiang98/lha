use crate::product::agent::protocol::FinalOutput;
use crate::product::agent::util::resume_command;
use crate::product::tui_app::app::AppExitInfo;
use crate::product::tui_app::app::InputSlimmingExitSummary;
use crate::product::tui_app::status::format_tokens_compact;
use crate::product::tui_app::status::format_usd_micros;
use owo_colors::OwoColorize;

pub fn format_exit_messages(exit_info: &AppExitInfo, color_enabled: bool) -> Vec<String> {
    if exit_info.token_usage.is_zero() {
        return Vec::new();
    }

    let mut lines = vec![format!(
        "{}",
        FinalOutput::from(exit_info.token_usage.clone())
    )];

    if let Some(input_slimming) = exit_info
        .input_slimming
        .as_ref()
        .filter(|summary| summary.tokens_saved > 0)
    {
        lines.push(format_input_slimming_savings(input_slimming));
    }

    if let Some(resume_cmd) = resume_command(exit_info.thread_name.as_deref(), exit_info.thread_id)
    {
        let command = if color_enabled {
            resume_cmd.cyan().to_string()
        } else {
            resume_cmd
        };
        lines.push(format!("To continue this session, run {command}"));
    }

    lines
}

fn format_input_slimming_savings(summary: &InputSlimmingExitSummary) -> String {
    let mut line = format!(
        "Input slimming saved: {} tokens",
        format_tokens_compact(summary.tokens_saved)
    );
    if let Some(saved_usd_micros) = summary.saved_usd_micros {
        line.push_str(" / ");
        line.push_str(&format_usd_micros(saved_usd_micros));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::agent::protocol::TokenUsage;
    use crate::product::protocol::ThreadId;
    use crate::product::tui_app::app::ExitReason;
    use pretty_assertions::assert_eq;

    fn sample_exit_info(conversation_id: Option<&str>, thread_name: Option<&str>) -> AppExitInfo {
        let token_usage = TokenUsage {
            output_tokens: 2,
            total_tokens: 2,
            ..Default::default()
        };
        AppExitInfo {
            token_usage,
            input_slimming: None,
            thread_id: conversation_id
                .map(ThreadId::from_string)
                .map(Result::unwrap),
            thread_name: thread_name.map(str::to_string),
            update_action: None,
            exit_reason: ExitReason::UserRequested,
        }
    }

    #[test]
    fn format_exit_messages_skips_zero_usage() {
        let exit_info = AppExitInfo {
            token_usage: TokenUsage::default(),
            input_slimming: Some(InputSlimmingExitSummary {
                tokens_saved: 18_700,
                saved_usd_micros: Some(4_675),
            }),
            thread_id: None,
            thread_name: None,
            update_action: None,
            exit_reason: ExitReason::UserRequested,
        };
        let lines = format_exit_messages(&exit_info, false);
        assert!(lines.is_empty());
    }

    #[test]
    fn format_exit_messages_includes_resume_hint_without_color() {
        let exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
        let lines = format_exit_messages(&exit_info, false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "To continue this session, run lha resume 123e4567-e89b-12d3-a456-426614174000"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn format_exit_messages_includes_input_slimming_tokens() {
        let mut exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
        exit_info.input_slimming = Some(InputSlimmingExitSummary {
            tokens_saved: 18_700,
            saved_usd_micros: None,
        });

        let lines = format_exit_messages(&exit_info, false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "Input slimming saved: 18.7K tokens".to_string(),
                "To continue this session, run lha resume 123e4567-e89b-12d3-a456-426614174000"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn format_exit_messages_includes_input_slimming_tokens_and_usd() {
        let mut exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
        exit_info.input_slimming = Some(InputSlimmingExitSummary {
            tokens_saved: 18_700,
            saved_usd_micros: Some(4_675),
        });

        let lines = format_exit_messages(&exit_info, false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "Input slimming saved: 18.7K tokens / $0.0047".to_string(),
                "To continue this session, run lha resume 123e4567-e89b-12d3-a456-426614174000"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn format_exit_messages_skips_zero_input_slimming_savings() {
        let mut exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
        exit_info.input_slimming = Some(InputSlimmingExitSummary {
            tokens_saved: 0,
            saved_usd_micros: Some(4_675),
        });

        let lines = format_exit_messages(&exit_info, false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "To continue this session, run lha resume 123e4567-e89b-12d3-a456-426614174000"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn format_exit_messages_applies_color_only_to_resume_hint() {
        let mut exit_info = sample_exit_info(Some("123e4567-e89b-12d3-a456-426614174000"), None);
        exit_info.input_slimming = Some(InputSlimmingExitSummary {
            tokens_saved: 18_700,
            saved_usd_micros: Some(4_675),
        });

        let lines = format_exit_messages(&exit_info, true);
        assert_eq!(lines.len(), 3);
        assert!(!lines[1].contains("\u{1b}["));
        assert!(lines[2].contains("\u{1b}[36m"));
    }

    #[test]
    fn format_exit_messages_prefers_thread_name() {
        let exit_info = sample_exit_info(
            Some("123e4567-e89b-12d3-a456-426614174000"),
            Some("my-thread"),
        );
        let lines = format_exit_messages(&exit_info, false);
        assert_eq!(
            lines,
            vec![
                "Token usage: total=2 input=0 output=2".to_string(),
                "To continue this session, run lha resume my-thread".to_string(),
            ]
        );
    }
}
