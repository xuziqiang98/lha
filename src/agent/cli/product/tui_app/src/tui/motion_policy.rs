use std::time::Duration;

const LOCAL_MAX_FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);
const REMOTE_MAX_FRAME_INTERVAL: Duration = Duration::from_nanos(33_333_334);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TerminalEnvironment {
    term_program: Option<String>,
    ssh_client: Option<String>,
    ssh_tty: Option<String>,
    tmux: Option<String>,
    sty: Option<String>,
}

impl TerminalEnvironment {
    pub(crate) fn from_process() -> Self {
        Self {
            term_program: std::env::var("TERM_PROGRAM").ok(),
            ssh_client: std::env::var("SSH_CLIENT").ok(),
            ssh_tty: std::env::var("SSH_TTY").ok(),
            tmux: std::env::var("TMUX").ok(),
            sty: std::env::var("STY").ok(),
        }
    }

    #[cfg(test)]
    fn with_term_program(mut self, value: &str) -> Self {
        self.term_program = Some(value.to_string());
        self
    }

    #[cfg(test)]
    fn with_ssh_client(mut self, value: &str) -> Self {
        self.ssh_client = Some(value.to_string());
        self
    }

    #[cfg(test)]
    fn with_ssh_tty(mut self, value: &str) -> Self {
        self.ssh_tty = Some(value.to_string());
        self
    }

    #[cfg(test)]
    fn with_tmux(mut self, value: &str) -> Self {
        self.tmux = Some(value.to_string());
        self
    }

    #[cfg(test)]
    fn with_sty(mut self, value: &str) -> Self {
        self.sty = Some(value.to_string());
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MotionPolicy {
    min_frame_interval: Duration,
    force_disable_animations: bool,
}

impl MotionPolicy {
    pub(crate) fn for_environment(environment: &TerminalEnvironment) -> Self {
        let is_termius = environment
            .term_program
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("termius"));
        let is_ssh = environment.ssh_client.is_some() || environment.ssh_tty.is_some();
        if is_termius || is_ssh {
            return Self {
                min_frame_interval: REMOTE_MAX_FRAME_INTERVAL,
                force_disable_animations: true,
            };
        }

        if environment.tmux.is_some() || environment.sty.is_some() {
            return Self {
                min_frame_interval: REMOTE_MAX_FRAME_INTERVAL,
                force_disable_animations: false,
            };
        }

        Self {
            min_frame_interval: LOCAL_MAX_FRAME_INTERVAL,
            force_disable_animations: false,
        }
    }

    pub(crate) const fn min_frame_interval(self) -> Duration {
        self.min_frame_interval
    }

    pub(crate) const fn effective_animations(self, configured: bool) -> bool {
        configured && !self.force_disable_animations
    }
}

impl Default for MotionPolicy {
    fn default() -> Self {
        Self::for_environment(&TerminalEnvironment::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn termius_and_ssh_use_low_motion_policy() {
        for environment in [
            TerminalEnvironment::default().with_term_program("Termius"),
            TerminalEnvironment::default().with_ssh_client("10.0.0.1 1234 22"),
            TerminalEnvironment::default().with_ssh_tty("/dev/pts/1"),
        ] {
            let policy = MotionPolicy::for_environment(&environment);
            assert_eq!(policy.min_frame_interval(), REMOTE_MAX_FRAME_INTERVAL);
            assert!(!policy.effective_animations(true));
            assert!(!policy.effective_animations(false));
        }
    }

    #[test]
    fn tmux_uses_constrained_frame_rate() {
        let policy = MotionPolicy::for_environment(
            &TerminalEnvironment::default().with_tmux("/tmp/tmux-501/default,1,0"),
        );

        assert_eq!(policy.min_frame_interval(), REMOTE_MAX_FRAME_INTERVAL);
        assert!(policy.effective_animations(true));
        assert!(!policy.effective_animations(false));
    }

    #[test]
    fn screen_uses_constrained_frame_rate() {
        let policy =
            MotionPolicy::for_environment(&TerminalEnvironment::default().with_sty("1234.session"));

        assert_eq!(policy.min_frame_interval(), REMOTE_MAX_FRAME_INTERVAL);
        assert!(policy.effective_animations(true));
        assert!(!policy.effective_animations(false));
    }

    #[test]
    fn local_terminal_uses_sixty_fps_and_configured_animations() {
        let policy = MotionPolicy::default();

        assert_eq!(policy.min_frame_interval(), LOCAL_MAX_FRAME_INTERVAL);
        assert!(policy.effective_animations(true));
        assert!(!policy.effective_animations(false));
    }
}
