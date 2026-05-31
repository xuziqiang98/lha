use std::sync::Arc;

use lha_agent::CodexThread;
use lha_agent::NewThread;
use lha_agent::ThreadManager;
use lha_agent::config::Config;
use lha_agent::protocol::Event;
use lha_agent::protocol::EventMsg;
use lha_agent::protocol::Op;
use lha_agent::protocol::SessionConfiguredEvent;
use lha_protocol::ThreadId;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;

/// Spawn the agent bootstrapper and op forwarding loop, returning the
/// `UnboundedSender<Op>` used by the UI to submit operations.
pub(crate) fn spawn_agent(
    config: Config,
    app_event_tx: AppEventSender,
    server: Arc<ThreadManager>,
) -> UnboundedSender<Op> {
    let (codex_op_tx, codex_op_rx) = unbounded_channel::<Op>();

    let app_event_tx_clone = app_event_tx;
    tokio::spawn(async move {
        let NewThread {
            thread,
            thread_id,
            session_configured,
            ..
        } = match server.start_thread(config).await {
            Ok(v) => v,
            Err(err) => {
                let message = format!("Failed to initialize codex: {err}");
                tracing::error!("{message}");
                app_event_tx_clone.send(AppEvent::CodexEvent(Event {
                    id: "".to_string(),
                    msg: EventMsg::Error(err.to_error_event(None)),
                }));
                app_event_tx_clone.send(AppEvent::FatalExitRequest(message));
                tracing::error!("failed to initialize codex: {err}");
                return;
            }
        };

        run_thread_bridge(
            thread,
            thread_id,
            session_configured,
            app_event_tx_clone,
            codex_op_rx,
        )
        .await;
    });

    codex_op_tx
}

/// Attach an existing thread to the chat widget event/op bridge.
///
/// Sends the provided `SessionConfiguredEvent` immediately, then forwards
/// subsequent events and accepts Ops for submission.
pub(crate) fn attach_existing_thread(
    thread: std::sync::Arc<CodexThread>,
    thread_id: ThreadId,
    session_configured: lha_agent::protocol::SessionConfiguredEvent,
    app_event_tx: AppEventSender,
) -> UnboundedSender<Op> {
    let (codex_op_tx, codex_op_rx) = unbounded_channel::<Op>();

    let app_event_tx_clone = app_event_tx;
    tokio::spawn(async move {
        run_thread_bridge(
            thread,
            thread_id,
            session_configured,
            app_event_tx_clone,
            codex_op_rx,
        )
        .await;
    });

    codex_op_tx
}

async fn run_thread_bridge(
    thread: Arc<CodexThread>,
    thread_id: ThreadId,
    session_configured: SessionConfiguredEvent,
    app_event_tx: AppEventSender,
    mut codex_op_rx: UnboundedReceiver<Op>,
) {
    // Forward the captured `SessionConfigured` event so it can be rendered in the UI.
    let ev = lha_agent::protocol::Event {
        // The `id` does not matter for rendering, so we can use a fake value.
        id: "".to_string(),
        msg: lha_agent::protocol::EventMsg::SessionConfigured(session_configured),
    };
    app_event_tx.send(AppEvent::CodexEvent(ev));

    let thread_clone = thread.clone();
    let op_forwarder = tokio::spawn(async move {
        while let Some(op) = codex_op_rx.recv().await {
            let id = thread_clone.submit(op).await;
            if let Err(e) = id {
                tracing::error!("failed to submit op: {e}");
            }
        }
    });

    loop {
        match thread.next_event().await {
            Ok(event) => {
                let is_shutdown_complete = matches!(event.msg, EventMsg::ShutdownComplete);
                app_event_tx.send(AppEvent::ThreadEventReceived { thread_id, event });
                if is_shutdown_complete {
                    tracing::debug!("agent event bridge exited after ShutdownComplete");
                    break;
                }
            }
            Err(err) => {
                let message = format!("Agent event stream closed unexpectedly: {err}");
                tracing::error!("{message}");
                app_event_tx.send(AppEvent::FatalExitRequest(message));
                break;
            }
        }
    }

    op_forwarder.abort();
}
