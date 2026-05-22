use strum::IntoEnumIterator;
use strum_macros::AsRefStr;
use strum_macros::EnumIter;
use strum_macros::EnumString;
use strum_macros::IntoStaticStr;

/// Commands that can be invoked by starting a message with a leading slash.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, EnumString, EnumIter, AsRefStr, IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum SlashCommand {
    // DO NOT ALPHA-SORT! Enum order is presentation order in the popup, so
    // more frequently used commands should be listed first.
    Model,
    Providers,
    Approvals,
    Permissions,
    #[strum(serialize = "setup-elevated-sandbox")]
    ElevateSandbox,
    Experimental,
    Buddy,
    Skills,
    Review,
    Rename,
    New,
    Resume,
    Fork,
    Init,
    Compact,
    Identity,
    // Undo,
    Changelog,
    Diff,
    Mention,
    Status,
    Plan,
    Bottom,
    Mcp,
    Logout,
    Quit,
    Exit,
    Feedback,
    Rollout,
    Ps,
    #[strum(to_string = "stop", serialize = "clean")]
    Stop,
    Personality,
    TestApproval,
}

impl SlashCommand {
    /// User-visible description shown in the popup.
    pub fn description(self) -> &'static str {
        match self {
            SlashCommand::Feedback => "save feedback locally",
            SlashCommand::New => "start a new chat during a conversation",
            SlashCommand::Init => "create an AGENTS.md file with instructions for Adam",
            SlashCommand::Compact => "summarize conversation to prevent hitting the context limit",
            SlashCommand::Review => "review my current changes and find issues",
            SlashCommand::Rename => "rename the current thread",
            SlashCommand::Resume => "resume a saved chat",
            SlashCommand::Fork => "fork the current chat",
            // SlashCommand::Undo => "ask Adam to undo a turn",
            SlashCommand::Quit | SlashCommand::Exit => "exit Adam",
            SlashCommand::Diff => "show git diff (including untracked files)",
            SlashCommand::Mention => "mention a file",
            SlashCommand::Skills => "use skills to improve how Adam performs specific tasks",
            SlashCommand::Status => "show current session configuration and token usage",
            SlashCommand::Plan => "jump to the latest proposed plan",
            SlashCommand::Bottom => "scroll transcript to the bottom",
            SlashCommand::Ps => "list background terminals",
            SlashCommand::Stop => "stop all background terminals",
            SlashCommand::Model => "choose what model and reasoning effort to use",
            SlashCommand::Providers => "add a custom provider and save models for it",
            SlashCommand::Personality => "choose a communication style for Adam",
            SlashCommand::Identity => "choose Adam identity",
            SlashCommand::Changelog => "show added, modified, and deleted files",
            SlashCommand::Approvals => "choose what Adam can do without approval",
            SlashCommand::Permissions => "choose what Adam is allowed to do",
            SlashCommand::ElevateSandbox => "set up elevated agent sandbox",
            SlashCommand::Experimental => "toggle experimental features",
            SlashCommand::Buddy => "manage your TUI buddy companion",
            SlashCommand::Mcp => "list configured MCP tools",
            SlashCommand::Logout => "log out of Adam",
            SlashCommand::Rollout => "print the rollout file path",
            SlashCommand::TestApproval => "test approval request",
        }
    }

    /// Command string without the leading '/'. Provided for compatibility with
    /// existing code that expects a method named `command()`.
    pub fn command(self) -> &'static str {
        self.into()
    }

    /// Whether this command can be run while a task is in progress.
    pub fn available_during_task(self) -> bool {
        match self {
            SlashCommand::New
            | SlashCommand::Resume
            | SlashCommand::Fork
            | SlashCommand::Init
            | SlashCommand::Compact
            // | SlashCommand::Undo
            | SlashCommand::Model
            | SlashCommand::Providers
            | SlashCommand::Personality
            | SlashCommand::Approvals
            | SlashCommand::Permissions
            | SlashCommand::ElevateSandbox
            | SlashCommand::Experimental
            | SlashCommand::Review
            | SlashCommand::Logout => false,
            SlashCommand::Diff
            | SlashCommand::Changelog
            | SlashCommand::Rename
            | SlashCommand::Buddy
            | SlashCommand::Mention
            | SlashCommand::Skills
            | SlashCommand::Status
            | SlashCommand::Plan
            | SlashCommand::Bottom
            | SlashCommand::Ps
            | SlashCommand::Stop
            | SlashCommand::Mcp
            | SlashCommand::Feedback
            | SlashCommand::Quit
            | SlashCommand::Exit => true,
            SlashCommand::Rollout => true,
            SlashCommand::TestApproval => true,
            SlashCommand::Identity => false,
        }
    }

    fn is_visible(self) -> bool {
        match self {
            SlashCommand::Rollout | SlashCommand::TestApproval => cfg!(debug_assertions),
            _ => true,
        }
    }
}

/// Return all built-in commands in a Vec paired with their command string.
pub fn built_in_slash_commands() -> Vec<(&'static str, SlashCommand)> {
    SlashCommand::iter()
        .filter(|command| command.is_visible())
        .map(|c| (c.command(), c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::str::FromStr;

    #[test]
    fn stop_command_is_canonical_name() {
        assert_eq!(SlashCommand::Stop.command(), "stop");
    }

    #[test]
    fn clean_alias_parses_to_stop_command() {
        assert_eq!(SlashCommand::from_str("clean"), Ok(SlashCommand::Stop));
    }

    #[test]
    fn buddy_command_is_available_during_task() {
        assert_eq!(SlashCommand::from_str("buddy"), Ok(SlashCommand::Buddy));
        assert!(SlashCommand::Buddy.available_during_task());
    }

    #[test]
    fn bottom_command_is_available_during_task() {
        assert_eq!(SlashCommand::from_str("bottom"), Ok(SlashCommand::Bottom));
        assert_eq!(SlashCommand::Bottom.command(), "bottom");
        assert!(SlashCommand::Bottom.available_during_task());
    }

    #[test]
    fn plan_command_is_available_during_task() {
        assert_eq!(SlashCommand::from_str("plan"), Ok(SlashCommand::Plan));
        assert_eq!(SlashCommand::Plan.command(), "plan");
        assert!(SlashCommand::Plan.available_during_task());
    }

    #[test]
    fn built_in_commands_include_bottom() {
        assert!(
            built_in_slash_commands()
                .into_iter()
                .any(|(command, slash_command)| {
                    command == "bottom" && slash_command == SlashCommand::Bottom
                })
        );
    }

    #[test]
    fn built_in_commands_include_plan() {
        assert!(
            built_in_slash_commands()
                .into_iter()
                .any(|(command, slash_command)| {
                    command == "plan" && slash_command == SlashCommand::Plan
                })
        );
    }

    #[test]
    fn apps_command_is_not_available() {
        assert_eq!(
            SlashCommand::from_str("apps"),
            Err(strum::ParseError::VariantNotFound)
        );
        assert!(
            !built_in_slash_commands()
                .into_iter()
                .any(|(command, _)| command == "apps")
        );
    }
}
