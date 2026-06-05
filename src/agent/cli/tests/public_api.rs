use lha_cli::CliConfigOverrides;
use lha_cli::LandlockCommand;
use lha_cli::SeatbeltCommand;
use lha_cli::WindowsCommand;
use pretty_assertions::assert_eq;

#[test]
fn debug_sandbox_commands_expose_config_override_type() {
    let overrides = CliConfigOverrides::default();
    assert_eq!(overrides.raw_overrides, Vec::<String>::new());

    let seatbelt = SeatbeltCommand {
        full_auto: false,
        log_denials: false,
        config_overrides: overrides.clone(),
        command: vec!["true".to_string()],
    };

    let landlock = LandlockCommand {
        full_auto: false,
        config_overrides: overrides.clone(),
        command: vec!["true".to_string()],
    };

    let windows = WindowsCommand {
        full_auto: false,
        config_overrides: overrides,
        command: vec!["cmd".to_string()],
    };

    assert_eq!(seatbelt.command, vec!["true".to_string()]);
    assert_eq!(landlock.command, vec!["true".to_string()]);
    assert_eq!(windows.command, vec!["cmd".to_string()]);
}
