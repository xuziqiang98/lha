use std::path::PathBuf;

use lha_agent::config::Config;
use lha_agent::protocol::Event;
use lha_agent::protocol::EventMsg;
use lha_agent::protocol::SessionConfiguredEvent;
use tracing::error;

use crate::event_processor::CodexStatus;
use crate::event_processor::EventProcessor;
use crate::event_processor::handle_last_message;

pub(crate) struct EventProcessorWithRawEventOutput {
    last_message_path: Option<PathBuf>,
}

impl EventProcessorWithRawEventOutput {
    pub(crate) fn new(last_message_path: Option<PathBuf>) -> Self {
        Self { last_message_path }
    }

    #[allow(clippy::print_stdout)]
    fn print_event(&self, event: &Event) {
        match serde_json::to_string(event) {
            Ok(line) => println!("{line}"),
            Err(err) => error!("Failed to serialize raw event: {err:?}"),
        }
    }
}

impl EventProcessor for EventProcessorWithRawEventOutput {
    fn print_config_summary(&mut self, _: &Config, _: &str, ev: &SessionConfiguredEvent) {
        self.print_event(&Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(ev.clone()),
        });
    }

    fn process_event(&mut self, event: Event) -> CodexStatus {
        self.print_event(&event);

        match event.msg {
            EventMsg::TurnComplete(turn_complete) => {
                if let Some(output_file) = self.last_message_path.as_deref() {
                    handle_last_message(turn_complete.last_agent_message.as_deref(), output_file);
                }
                CodexStatus::InitiateShutdown
            }
            EventMsg::ShutdownComplete => CodexStatus::Shutdown,
            _ => CodexStatus::Running,
        }
    }
}
