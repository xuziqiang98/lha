/// Update action the CLI should perform after the TUI exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    /// Update via `cargo install lha --locked`.
    CargoInstall,
}

impl UpdateAction {
    /// Returns the list of command-line arguments for invoking the update.
    pub fn command_args(self) -> (&'static str, &'static [&'static str]) {
        match self {
            UpdateAction::CargoInstall => ("cargo", &["install", "lha", "--locked"]),
        }
    }

    /// Returns string representation of the command-line arguments for invoking the update.
    pub fn command_str(self) -> String {
        let (command, args) = self.command_args();
        shlex::try_join(std::iter::once(command).chain(args.iter().copied()))
            .unwrap_or_else(|_| format!("{command} {}", args.join(" ")))
    }
}

#[cfg(any(not(debug_assertions), test))]
pub(crate) fn get_update_action() -> Option<UpdateAction> {
    Some(UpdateAction::CargoInstall)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_install_update_action_command() {
        assert_eq!(
            UpdateAction::CargoInstall.command_str(),
            "cargo install lha --locked"
        );
    }

    #[test]
    fn get_update_action_returns_cargo_install() {
        assert_eq!(get_update_action(), Some(UpdateAction::CargoInstall));
    }
}
