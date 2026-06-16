use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

use tokio::sync::mpsc::UnboundedSender;

use crate::product::protocol::ThreadId;

use crate::product::tui_app::app_event::AppEvent;
use crate::product::tui_app::history_cell::HistoryCell;
use crate::product::tui_app::session_log;

#[derive(Clone, Debug)]
pub(crate) struct AppEventSender {
    pub app_event_tx: UnboundedSender<AppEvent>,
    history_thread_id: Arc<Mutex<Option<ThreadId>>>,
}

impl AppEventSender {
    pub(crate) fn new(app_event_tx: UnboundedSender<AppEvent>) -> Self {
        Self {
            app_event_tx,
            history_thread_id: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn bind_history_to_widget(&self) -> Self {
        let history_thread_id = self
            .history_thread_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .to_owned();
        Self {
            app_event_tx: self.app_event_tx.clone(),
            history_thread_id: Arc::new(Mutex::new(history_thread_id)),
        }
    }

    pub(crate) fn set_history_thread_id(&self, thread_id: Option<ThreadId>) {
        *self
            .history_thread_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = thread_id;
    }

    pub(crate) fn send_history_cell(&self, cell: Box<dyn HistoryCell>) {
        let event = if let Some(thread_id) = *self
            .history_thread_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            AppEvent::InsertThreadHistoryCell { thread_id, cell }
        } else {
            AppEvent::InsertHistoryCell(cell)
        };
        self.send(event);
    }

    pub(crate) fn send_history_cell_with_viewport_repaint(&self, cell: Box<dyn HistoryCell>) {
        let event = if let Some(thread_id) = *self
            .history_thread_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            AppEvent::InsertThreadHistoryCellWithViewportRepaint { thread_id, cell }
        } else {
            AppEvent::InsertHistoryCellWithViewportRepaint(cell)
        };
        self.send(event);
    }

    /// Send an event to the app event channel. If it fails, we swallow the
    /// error and log it.
    pub(crate) fn send(&self, event: AppEvent) {
        // Record inbound events for high-fidelity session replay.
        // Avoid double-logging Ops; those are logged at the point of submission.
        if !matches!(event, AppEvent::CodexOp(_)) {
            session_log::log_inbound_app_event(&event);
        }
        if let Err(e) = self.app_event_tx.send(event) {
            tracing::error!("failed to send event: {e}");
        }
    }
}
