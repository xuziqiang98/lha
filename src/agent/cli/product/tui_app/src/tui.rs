use std::fmt;
use std::future::Future;
use std::io::IsTerminal;
use std::io::Result;
use std::io::Stdout;
use std::io::Write;
use std::io::stdin;
use std::io::stdout;
use std::panic;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::TryLockError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use crossterm::Command;
use crossterm::event::DisableBracketedPaste;
use crossterm::event::DisableFocusChange;
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableBracketedPaste;
use crossterm::event::EnableFocusChange;
use crossterm::event::EnableMouseCapture;
use crossterm::event::KeyEvent;
use crossterm::event::KeyboardEnhancementFlags;
use crossterm::event::MouseEvent;
use crossterm::event::PopKeyboardEnhancementFlags;
use crossterm::event::PushKeyboardEnhancementFlags;
use crossterm::terminal::BeginSynchronizedUpdate;
use crossterm::terminal::EndSynchronizedUpdate;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use crossterm::terminal::supports_keyboard_enhancement;
use ratatui::backend::Backend;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::execute;
use ratatui::crossterm::queue;
use ratatui::crossterm::terminal::disable_raw_mode;
use ratatui::crossterm::terminal::enable_raw_mode;
use tokio::sync::broadcast;
use tokio_stream::Stream;

pub use self::frame_requester::FrameRequester;
use crate::product::agent::config::types::NotificationMethod;
use crate::product::tui_app::custom_terminal;
use crate::product::tui_app::custom_terminal::Terminal as CustomTerminal;
use crate::product::tui_app::notifications::DesktopNotificationBackend;
use crate::product::tui_app::notifications::detect_backend;
use crate::product::tui_app::tui::event_stream::EventBroker;
use crate::product::tui_app::tui::event_stream::TuiEventStream;
#[cfg(unix)]
use crate::product::tui_app::tui::job_control::SuspendContext;

mod event_stream;
mod frame_rate_limiter;
mod frame_requester;
#[cfg(unix)]
mod job_control;
mod stderr_guard;

use stderr_guard::TuiStderrGuard;

type SharedStderrGuard = Arc<Mutex<Option<TuiStderrGuard>>>;

const RESIZE_RECONCILE_DELAY: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResizeReconcileAction {
    None,
    ScheduleAt(Instant),
    Reconcile,
}

#[derive(Debug, Default)]
struct ResizeReconcileState {
    deadline: Option<Instant>,
}

impl ResizeReconcileState {
    fn next_action(&mut self, now: Instant, resize_observed: bool) -> ResizeReconcileAction {
        if resize_observed {
            let deadline = now + RESIZE_RECONCILE_DELAY;
            self.deadline = Some(deadline);
            return ResizeReconcileAction::ScheduleAt(deadline);
        }

        match self.deadline {
            None => ResizeReconcileAction::None,
            Some(deadline) if now < deadline => ResizeReconcileAction::ScheduleAt(deadline),
            Some(_) => ResizeReconcileAction::Reconcile,
        }
    }

    fn finish_reconcile(&mut self) {
        self.deadline = None;
    }

    fn retry_pending(&mut self, now: Instant) -> ResizeReconcileAction {
        if self.deadline.is_none() {
            return ResizeReconcileAction::None;
        }
        let deadline = now + RESIZE_RECONCILE_DELAY;
        self.deadline = Some(deadline);
        ResizeReconcileAction::ScheduleAt(deadline)
    }

    fn finish_draw(
        &mut self,
        action: ResizeReconcileAction,
        now: Instant,
        succeeded: bool,
    ) -> ResizeReconcileAction {
        if !succeeded {
            return self.retry_pending(now);
        }
        if action == ResizeReconcileAction::Reconcile {
            self.finish_reconcile();
            ResizeReconcileAction::None
        } else {
            action
        }
    }
}

/// A type alias for the terminal type used in this application
pub type Terminal = CustomTerminal<CrosstermBackend<Stdout>>;

pub fn set_modes(use_mouse_capture: bool) -> Result<()> {
    execute!(stdout(), EnableBracketedPaste)?;

    enable_raw_mode()?;
    // Enable keyboard enhancement flags so modifiers for keys like Enter are disambiguated.
    // chat_composer.rs is using a keyboard event listener to enter for any modified keys
    // to create a new line that require this.
    // Some terminals (notably legacy Windows consoles) do not support
    // keyboard enhancement flags. Attempt to enable them, but continue
    // gracefully if unsupported.
    let _ = execute!(
        stdout(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
    );

    let _ = execute!(stdout(), EnableFocusChange);
    if use_mouse_capture {
        let _ = execute!(stdout(), EnableMouseCapture);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DisableAlternateScroll;

impl Command for DisableAlternateScroll {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[?1007l")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> Result<()> {
        Err(std::io::Error::other(
            "tried to execute DisableAlternateScroll using WinAPI; use ANSI instead",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

fn restore_common(should_disable_raw_mode: bool) -> Result<()> {
    let _ = execute!(stdout(), DisableMouseCapture);
    let _ = execute!(stdout(), DisableAlternateScroll);
    let _ = execute!(stdout(), LeaveAlternateScreen);
    // Pop may fail on platforms that didn't support the push; ignore errors.
    let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    execute!(stdout(), DisableBracketedPaste)?;
    let _ = execute!(stdout(), DisableFocusChange);
    if should_disable_raw_mode {
        disable_raw_mode()?;
    }
    let _ = execute!(stdout(), crossterm::cursor::Show);
    Ok(())
}

/// Restore the terminal to its original state.
/// Inverse of `set_modes`.
pub fn restore() -> Result<()> {
    let should_disable_raw_mode = true;
    restore_common(should_disable_raw_mode)
}

/// Restore the terminal to its original state, but keep raw mode enabled.
pub fn restore_keep_raw() -> Result<()> {
    let should_disable_raw_mode = false;
    restore_common(should_disable_raw_mode)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMode {
    #[allow(dead_code)]
    Full, // Fully restore the terminal (disables raw mode).
    KeepRaw, // Restore the terminal but keep raw mode enabled.
}

impl RestoreMode {
    fn restore(self) -> Result<()> {
        match self {
            RestoreMode::Full => restore(),
            RestoreMode::KeepRaw => restore_keep_raw(),
        }
    }
}

/// Flush the underlying stdin buffer to clear any input that may be buffered at the terminal level.
/// For example, clears any user input that occurred while the crossterm EventStream was dropped.
#[cfg(unix)]
fn flush_terminal_input_buffer() {
    // Safety: flushing the stdin queue is safe and does not move ownership.
    let result = unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) };
    if result != 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!("failed to tcflush stdin: {err}");
    }
}

/// Flush the underlying stdin buffer to clear any input that may be buffered at the terminal level.
/// For example, clears any user input that occurred while the crossterm EventStream was dropped.
#[cfg(windows)]
fn flush_terminal_input_buffer() {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::FlushConsoleInputBuffer;
    use windows_sys::Win32::System::Console::GetStdHandle;
    use windows_sys::Win32::System::Console::STD_INPUT_HANDLE;

    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle == INVALID_HANDLE_VALUE || handle == 0 {
        let err = unsafe { GetLastError() };
        tracing::warn!("failed to get stdin handle for flush: error {err}");
        return;
    }

    let result = unsafe { FlushConsoleInputBuffer(handle) };
    if result == 0 {
        let err = unsafe { GetLastError() };
        tracing::warn!("failed to flush stdin buffer: error {err}");
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn flush_terminal_input_buffer() {}

/// Initialize the terminal for the fullscreen TUI.
pub fn init(use_mouse_capture: bool) -> Result<Terminal> {
    if !stdin().is_terminal() {
        return Err(std::io::Error::other("stdin is not a terminal"));
    }
    if !stdout().is_terminal() {
        return Err(std::io::Error::other("stdout is not a terminal"));
    }
    set_modes(use_mouse_capture)?;

    set_panic_hook();

    let backend = CrosstermBackend::new(stdout());
    let tui = CustomTerminal::with_options(backend)?;
    Ok(tui)
}

fn set_panic_hook() {
    let hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let _ = execute!(stdout(), EndSynchronizedUpdate);
        let _ = restore(); // ignore any errors as we are already failing
        hook(panic_info);
    }));
}

fn set_stderr_panic_hook(stderr_guard: SharedStderrGuard) {
    let hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let mut guard = match stderr_guard.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(err)) => err.into_inner(),
            Err(TryLockError::WouldBlock) => {
                hook(panic_info);
                return;
            }
        };
        if let Some(guard) = guard.as_mut() {
            let _ = guard.suspend();
        }
        drop(guard);
        hook(panic_info);
    }));
}

#[derive(Clone, Debug)]
pub enum TuiEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Draw,
}

pub struct Tui {
    frame_requester: FrameRequester,
    draw_tx: broadcast::Sender<()>,
    event_broker: Arc<EventBroker>,
    pub(crate) terminal: Terminal,
    alt_saved_viewport: Option<ratatui::layout::Rect>,
    #[cfg(unix)]
    suspend_context: SuspendContext,
    stderr_guard: SharedStderrGuard,
    // True when the fullscreen alternate screen is active.
    alt_screen_active: Arc<AtomicBool>,
    // True when terminal/tab is focused; updated internally from crossterm events
    terminal_focused: Arc<AtomicBool>,
    enhanced_keys_supported: bool,
    notification_backend: Option<DesktopNotificationBackend>,
    mouse_capture_enabled: bool,
    mouse_capture_bypass_active: bool,
    resize_reconcile: ResizeReconcileState,
}

impl Drop for Tui {
    fn drop(&mut self) {
        let guard = self
            .stderr_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(guard);
    }
}

impl Tui {
    pub fn new(terminal: Terminal, mouse_capture_enabled: bool) -> Self {
        let (draw_tx, _) = broadcast::channel(1);
        let frame_requester = FrameRequester::new(draw_tx.clone());
        let stderr_guard = Arc::new(Mutex::new(None));

        // Detect keyboard enhancement support before any EventStream is created so the
        // crossterm poller can acquire its lock without contention.
        let enhanced_keys_supported = supports_keyboard_enhancement().unwrap_or(false);
        // Cache this to avoid contention with the event reader.
        supports_color::on_cached(supports_color::Stream::Stdout);
        let _ = crate::product::tui_app::terminal_palette::default_colors();
        Self {
            frame_requester,
            draw_tx,
            event_broker: Arc::new(EventBroker::new()),
            terminal,
            alt_saved_viewport: None,
            #[cfg(unix)]
            suspend_context: SuspendContext::new(stderr_guard.clone()),
            stderr_guard,
            alt_screen_active: Arc::new(AtomicBool::new(false)),
            terminal_focused: Arc::new(AtomicBool::new(true)),
            enhanced_keys_supported,
            notification_backend: Some(detect_backend(NotificationMethod::default())),
            mouse_capture_enabled,
            mouse_capture_bypass_active: false,
            resize_reconcile: ResizeReconcileState::default(),
        }
    }

    pub fn set_notification_method(&mut self, method: NotificationMethod) {
        self.notification_backend = Some(detect_backend(method));
    }

    pub(crate) fn install_stderr_panic_hook(&self) {
        set_stderr_panic_hook(self.stderr_guard.clone());
    }

    pub(crate) fn redirect_stderr_to(&mut self, path: &std::path::Path) -> Result<()> {
        let guard = TuiStderrGuard::redirect_to(path)?;
        *self
            .stderr_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(guard);
        Ok(())
    }

    pub(crate) fn suspend_stderr(&self) {
        let mut guard = self
            .stderr_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(guard) = guard.as_mut()
            && let Err(err) = guard.suspend()
        {
            tracing::warn!("failed to restore stderr for external terminal use: {err}");
        }
    }

    fn resume_stderr(&self) {
        let mut guard = self
            .stderr_guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(guard) = guard.as_mut()
            && let Err(err) = guard.resume()
        {
            tracing::warn!("failed to redirect stderr back to the TUI log: {err}");
        }
    }

    pub fn frame_requester(&self) -> FrameRequester {
        self.frame_requester.clone()
    }

    pub fn enhanced_keys_supported(&self) -> bool {
        self.enhanced_keys_supported
    }

    pub fn is_alt_screen_active(&self) -> bool {
        self.alt_screen_active.load(Ordering::Relaxed)
    }

    pub fn mouse_capture_enabled(&self) -> bool {
        self.mouse_capture_enabled
    }

    pub fn set_mouse_capture_enabled(&mut self, enabled: bool) -> Result<()> {
        if self.mouse_capture_enabled == enabled {
            if !enabled {
                self.mouse_capture_bypass_active = false;
            }
            return Ok(());
        }

        if enabled {
            self.mouse_capture_enabled = true;
            self.mouse_capture_bypass_active = false;
            if self.is_alt_screen_active() {
                execute!(self.terminal.backend_mut(), EnableMouseCapture)?;
            }
        } else {
            execute!(self.terminal.backend_mut(), DisableMouseCapture)?;
            self.mouse_capture_enabled = false;
            self.mouse_capture_bypass_active = false;
        }

        Ok(())
    }

    pub fn disable_mouse_capture_temporarily(&mut self) {
        if self.mouse_capture_enabled && !self.mouse_capture_bypass_active {
            let _ = execute!(self.terminal.backend_mut(), DisableMouseCapture);
            self.mouse_capture_bypass_active = true;
        }
    }

    pub fn restore_mouse_capture_after_bypass(&mut self) {
        if self.mouse_capture_enabled && self.mouse_capture_bypass_active {
            let _ = execute!(self.terminal.backend_mut(), EnableMouseCapture);
            self.mouse_capture_bypass_active = false;
        }
    }

    // Drop crossterm EventStream to avoid stdin conflicts with other processes.
    pub fn pause_events(&mut self) {
        self.event_broker.pause_events();
    }

    // Resume crossterm EventStream to resume stdin polling.
    // Inverse of `pause_events`.
    pub fn resume_events(&mut self) {
        self.event_broker.resume_events();
    }

    /// Temporarily restore terminal state to run an external interactive program `f`.
    ///
    /// This pauses crossterm's stdin polling by dropping the underlying event stream, restores
    /// terminal modes (optionally keeping raw mode enabled), then re-applies LHA TUI modes and
    /// flushes pending stdin input before resuming events.
    pub async fn with_restored<R, F, Fut>(&mut self, mode: RestoreMode, f: F) -> R
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = R>,
    {
        // Pause crossterm events to avoid stdin conflicts with external program `f`.
        self.pause_events();
        // An external program must inherit the user's original stderr even when the TUI is
        // running inline and therefore never leaves the alternate screen.
        self.suspend_stderr();

        // Leave alt screen if active to avoid conflicts with external program `f`.
        let was_alt_screen = self.is_alt_screen_active();
        if was_alt_screen {
            let _ = self.leave_alt_screen();
        }

        if let Err(err) = mode.restore() {
            tracing::warn!("failed to restore terminal modes before external program: {err}");
        }

        let output = f().await;

        if let Err(err) = set_modes(self.mouse_capture_enabled) {
            tracing::warn!("failed to re-enable terminal modes after external program: {err}");
        }
        // After the external program `f` finishes, reset terminal state and flush any buffered keypresses.
        flush_terminal_input_buffer();

        if was_alt_screen {
            let _ = self.enter_alt_screen();
        } else {
            if let Err(err) = self.terminal.clear() {
                tracing::warn!("failed to clear inline viewport after external program: {err}");
            }
            self.resume_stderr();
        }

        self.resume_events();
        output
    }

    /// Emit a desktop notification now if the terminal is unfocused.
    /// Returns true if a notification was posted.
    pub fn notify(&mut self, message: impl AsRef<str>) -> bool {
        if self.terminal_focused.load(Ordering::Relaxed) {
            return false;
        }

        let Some(notification_backend) = self.notification_backend.as_mut() else {
            return false;
        };

        let message = message.as_ref().to_string();
        // Notifications are serialized through the same backend writer as frame output. OSC 9
        // and BEL do not move the drawing cursor, so they remain out-of-band from frame diffs.
        match notification_backend.notify(&message, self.terminal.backend_mut()) {
            Ok(()) => true,
            Err(err) => {
                let method = notification_backend.method();
                tracing::warn!(
                    error = %err,
                    method = %method,
                    "Failed to emit terminal notification; disabling future notifications"
                );
                self.notification_backend = None;
                false
            }
        }
    }

    pub fn event_stream(&self) -> Pin<Box<dyn Stream<Item = TuiEvent> + Send + 'static>> {
        #[cfg(unix)]
        let stream = TuiEventStream::new(
            self.event_broker.clone(),
            self.draw_tx.subscribe(),
            self.terminal_focused.clone(),
            self.suspend_context.clone(),
            self.alt_screen_active.clone(),
            self.mouse_capture_enabled,
        );
        #[cfg(not(unix))]
        let stream = TuiEventStream::new(
            self.event_broker.clone(),
            self.draw_tx.subscribe(),
            self.terminal_focused.clone(),
        );
        Box::pin(stream)
    }

    /// Enter alternate screen and expand the viewport to full terminal size, saving the current
    /// viewport for restoration when leaving.
    pub fn enter_alt_screen(&mut self) -> Result<()> {
        let _ = execute!(self.terminal.backend_mut(), EnterAlternateScreen);
        if self.mouse_capture_enabled {
            let _ = execute!(self.terminal.backend_mut(), EnableMouseCapture);
        }
        let _ = execute!(self.terminal.backend_mut(), DisableAlternateScroll);
        if let Ok(size) = self.terminal.size() {
            self.alt_saved_viewport = Some(self.terminal.viewport_area);
            self.terminal.set_viewport_area(ratatui::layout::Rect::new(
                0,
                0,
                size.width,
                size.height,
            ));
            let _ = self.terminal.clear();
        }
        self.alt_screen_active.store(true, Ordering::Relaxed);
        self.mouse_capture_bypass_active = false;
        self.resume_stderr();
        Ok(())
    }

    /// Leave alternate screen and restore the previously saved viewport, if any.
    pub fn leave_alt_screen(&mut self) -> Result<()> {
        self.suspend_stderr();
        if self.mouse_capture_enabled {
            let _ = execute!(self.terminal.backend_mut(), DisableMouseCapture);
        }
        let _ = execute!(self.terminal.backend_mut(), DisableAlternateScroll);
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        if let Some(saved) = self.alt_saved_viewport.take() {
            self.terminal.set_viewport_area(saved);
        }
        self.alt_screen_active.store(false, Ordering::Relaxed);
        self.mouse_capture_bypass_active = false;
        Ok(())
    }

    fn update_viewport<B>(terminal: &mut CustomTerminal<B>, height: u16) -> Result<()>
    where
        B: Backend + Write,
    {
        let size = terminal.size()?;

        let mut area = terminal.viewport_area;
        area.height = height.min(size.height);
        area.width = size.width;
        if area.bottom() > size.height {
            let scroll_by = area.bottom() - size.height;
            terminal
                .backend_mut()
                .scroll_region_up(0..area.top(), scroll_by)?;
            area.y = size.height - area.height;
        }
        let old_area = terminal.viewport_area;
        if area != old_area {
            terminal.clear_area(old_area)?;
            terminal.set_viewport_area(area);
            terminal.clear_area(area)?;
        }

        Ok(())
    }

    pub fn draw(
        &mut self,
        height: u16,
        draw_fn: impl FnOnce(&mut custom_terminal::Frame),
    ) -> Result<()> {
        // If we are resuming from ^Z, we need to prepare the resume action now so we can apply it
        // in the synchronized update.
        #[cfg(unix)]
        let mut prepared_resume = self
            .suspend_context
            .prepare_resume_action(&mut self.terminal, &mut self.alt_saved_viewport);
        #[cfg(unix)]
        let resume_stderr_after_draw = prepared_resume.is_some();
        #[cfg(unix)]
        let suspend_context = self.suspend_context.clone();

        let now = Instant::now();
        let explicit_resize = self.event_broker.take_resize_event();
        let resize_reconcile = &mut self.resize_reconcile;
        let mut reconcile_action = resize_reconcile.next_action(now, explicit_resize);
        let draw_result = synchronized_terminal_update(&mut self.terminal, |terminal| {
            #[cfg(unix)]
            if let Some(prepared) = prepared_resume.take() {
                prepared.apply(terminal)?;
            }

            if terminal.autoresize()? {
                reconcile_action = resize_reconcile.next_action(now, true);
            }
            Self::update_viewport(terminal, height)?;
            let area = terminal.viewport_area;
            if reconcile_action == ResizeReconcileAction::Reconcile {
                terminal.clear_area(area)?;
            }

            // Update the cursor row so Ctrl-Z can place the cursor correctly before suspending.
            #[cfg(unix)]
            suspend_context.set_cursor_y(area.bottom().saturating_sub(1));

            terminal.try_draw_unflushed_at_current_size(|frame| {
                draw_fn(frame);
                Result::Ok(())
            })
        });

        let next_action =
            resize_reconcile.finish_draw(reconcile_action, Instant::now(), draw_result.is_ok());
        if let ResizeReconcileAction::ScheduleAt(deadline) = next_action {
            self.frame_requester
                .schedule_frame_in(deadline.saturating_duration_since(Instant::now()));
        }

        #[cfg(unix)]
        if resume_stderr_after_draw {
            self.resume_stderr();
        }
        draw_result
    }
}

fn synchronized_terminal_update<B>(
    terminal: &mut CustomTerminal<B>,
    update: impl FnOnce(&mut CustomTerminal<B>) -> Result<()>,
) -> Result<()>
where
    B: Backend + Write,
{
    queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
    let update_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| update(terminal)));
    let end_result = queue!(terminal.backend_mut(), EndSynchronizedUpdate);
    let flush_result = Backend::flush(terminal.backend_mut());
    let output_result = end_result.and(flush_result);

    match update_result {
        Ok(Err(err)) => Err(err),
        Ok(Ok(())) => terminal.complete_frame(output_result),
        Err(payload) => {
            let _ = output_result;
            std::panic::resume_unwind(payload);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::tui_app::test_backend::AuditedVT100Backend;
    use pretty_assertions::assert_eq;
    use ratatui::layout::Rect;
    use ratatui::style::Style;
    use std::path::Path;
    use std::process::Command as ProcessCommand;

    const PANIC_HOOK_CHILD_LOG_PATH: &str = "LHA_PANIC_HOOK_TEST_LOG_PATH";

    fn write_raw_stderr(message: &str) {
        let mut stderr = std::io::stderr().lock();
        std::io::Write::write_all(&mut stderr, message.as_bytes()).expect("write stderr");
        std::io::Write::flush(&mut stderr).expect("flush stderr");
    }

    #[test]
    fn panic_hook_stderr_child() {
        let Some(path) = std::env::var_os(PANIC_HOOK_CHILD_LOG_PATH) else {
            return;
        };

        panic::set_hook(Box::new(|_| write_raw_stderr("panic-hook-report\n")));
        let guard = TuiStderrGuard::redirect_to(Path::new(&path)).expect("redirect child stderr");
        let stderr_guard = Arc::new(Mutex::new(Some(guard)));
        write_raw_stderr("redirected-before-panic\n");
        set_stderr_panic_hook(stderr_guard);

        panic!("trigger stderr-restoring panic hook");
    }

    #[test]
    fn panic_hook_restores_stderr_before_chaining() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log_path = temp.path().join("lha-tui.log");
        let output = ProcessCommand::new(std::env::current_exe().expect("current test binary"))
            .arg("panic_hook_stderr_child")
            .arg("--nocapture")
            .env(PANIC_HOOK_CHILD_LOG_PATH, &log_path)
            .output()
            .expect("run panic hook child");

        assert!(!output.status.success(), "{output:?}");
        let child_stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            child_stderr.contains("panic-hook-report"),
            "{child_stderr:?}"
        );
        assert!(
            !child_stderr.contains("redirected-before-panic"),
            "{child_stderr:?}"
        );

        let log = std::fs::read_to_string(log_path).expect("read redirected stderr");
        assert!(log.contains("redirected-before-panic"), "{log:?}");
        assert!(!log.contains("panic-hook-report"), "{log:?}");
    }

    fn command_bytes(command: impl crossterm::Command) -> Vec<u8> {
        let mut bytes = Vec::new();
        queue!(&mut bytes, command).expect("queue command");
        bytes
    }

    fn byte_index(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap_or_else(|| {
                panic!(
                    "missing {:?} in {:?}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(haystack)
                )
            })
    }

    #[test]
    fn same_size_resize_event_arms_reconcile() {
        let now = Instant::now();
        let deadline = now + RESIZE_RECONCILE_DELAY;
        let event_broker: EventBroker = EventBroker::new();
        let backend = AuditedVT100Backend::new(20, 2);
        let mut terminal = CustomTerminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 20, 2));
        let mut state = ResizeReconcileState::default();

        assert!(!terminal.autoresize().expect("unchanged terminal size"));
        event_broker.record_resize_event();
        assert_eq!(
            state.next_action(now, event_broker.take_resize_event()),
            ResizeReconcileAction::ScheduleAt(deadline)
        );
        assert_eq!(
            state.next_action(deadline, false),
            ResizeReconcileAction::Reconcile
        );
        assert_eq!(
            state.finish_draw(ResizeReconcileAction::Reconcile, deadline, true),
            ResizeReconcileAction::None
        );
        assert!(!event_broker.take_resize_event());
        assert_eq!(
            state.next_action(deadline + Duration::from_millis(1), false),
            ResizeReconcileAction::None
        );
    }

    #[test]
    fn resize_reconcile_waits_until_deadline_and_runs_once() {
        let now = Instant::now();
        let deadline = now + RESIZE_RECONCILE_DELAY;
        let mut state = ResizeReconcileState::default();

        assert_eq!(
            state.next_action(now, true),
            ResizeReconcileAction::ScheduleAt(deadline)
        );
        assert_eq!(
            state.next_action(deadline - Duration::from_millis(1), false),
            ResizeReconcileAction::ScheduleAt(deadline)
        );
        assert_eq!(
            state.next_action(deadline, false),
            ResizeReconcileAction::Reconcile
        );
        assert_eq!(
            state.finish_draw(ResizeReconcileAction::Reconcile, deadline, true),
            ResizeReconcileAction::None
        );
        assert_eq!(
            state.next_action(deadline + Duration::from_millis(1), false),
            ResizeReconcileAction::None
        );
    }

    #[test]
    fn repeated_resize_extends_reconcile_deadline() {
        let now = Instant::now();
        let second_resize = now + Duration::from_millis(60);
        let first_deadline = now + RESIZE_RECONCILE_DELAY;
        let second_deadline = second_resize + RESIZE_RECONCILE_DELAY;
        let mut state = ResizeReconcileState::default();

        assert_eq!(
            state.next_action(now, true),
            ResizeReconcileAction::ScheduleAt(first_deadline)
        );
        assert_eq!(
            state.next_action(second_resize, true),
            ResizeReconcileAction::ScheduleAt(second_deadline)
        );
        assert_eq!(
            state.next_action(first_deadline, false),
            ResizeReconcileAction::ScheduleAt(second_deadline)
        );
        assert_eq!(
            state.next_action(second_deadline, false),
            ResizeReconcileAction::Reconcile
        );
    }

    #[test]
    fn failed_resize_reconcile_is_retried() {
        let now = Instant::now();
        let first_deadline = now + RESIZE_RECONCILE_DELAY;
        let retry_deadline = first_deadline + RESIZE_RECONCILE_DELAY;
        let mut state = ResizeReconcileState::default();

        assert_eq!(
            state.next_action(now, true),
            ResizeReconcileAction::ScheduleAt(first_deadline)
        );
        assert_eq!(
            state.next_action(first_deadline, false),
            ResizeReconcileAction::Reconcile
        );
        assert_eq!(
            state.finish_draw(ResizeReconcileAction::Reconcile, first_deadline, false),
            ResizeReconcileAction::ScheduleAt(retry_deadline)
        );
        assert_eq!(
            state.next_action(retry_deadline - Duration::from_millis(1), false),
            ResizeReconcileAction::ScheduleAt(retry_deadline)
        );
        assert_eq!(
            state.next_action(retry_deadline, false),
            ResizeReconcileAction::Reconcile
        );
        assert_eq!(
            state.finish_draw(ResizeReconcileAction::Reconcile, retry_deadline, true),
            ResizeReconcileAction::None
        );
        assert_eq!(
            state.retry_pending(retry_deadline + Duration::from_millis(1)),
            ResizeReconcileAction::None
        );
    }

    #[test]
    fn failed_resize_draw_restarts_settle_delay() {
        let now = Instant::now();
        let failed_at = now + Duration::from_millis(25);
        let retry_deadline = failed_at + RESIZE_RECONCILE_DELAY;
        let event_broker: EventBroker = EventBroker::new();
        let mut state = ResizeReconcileState::default();
        event_broker.record_resize_event();
        let action = state.next_action(now, event_broker.take_resize_event());

        assert_eq!(
            state.finish_draw(action, failed_at, false),
            ResizeReconcileAction::ScheduleAt(retry_deadline)
        );
        assert!(!event_broker.take_resize_event());
        assert_eq!(
            state.next_action(retry_deadline, false),
            ResizeReconcileAction::Reconcile
        );
        assert_eq!(
            state.finish_draw(ResizeReconcileAction::Reconcile, retry_deadline, true),
            ResizeReconcileAction::None
        );
        assert_eq!(
            state.next_action(retry_deadline + Duration::from_millis(1), false),
            ResizeReconcileAction::None
        );
    }

    #[test]
    fn synchronized_update_uses_one_backend_and_always_ends() {
        let backend = AuditedVT100Backend::new(20, 2);
        let mut terminal = CustomTerminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 20, 2));

        synchronized_terminal_update(&mut terminal, |terminal| {
            terminal.try_draw_unflushed(|frame| {
                frame
                    .buffer_mut()
                    .set_string(0, 0, "frame", Style::default());
                Result::Ok(())
            })
        })
        .expect("synchronized draw");

        let frame = terminal.backend().last_frame().expect("audited frame");
        let begin = command_bytes(BeginSynchronizedUpdate);
        let end = command_bytes(EndSynchronizedUpdate);
        let begin_index = byte_index(&frame.raw_bytes, &begin);
        let text_index = byte_index(&frame.raw_bytes, b"frame");
        let end_index = byte_index(&frame.raw_bytes, &end);
        assert!(begin_index < text_index && text_index < end_index);

        terminal.backend_mut().clear_frames();
        terminal.backend_mut().fail_next_draw();
        let result = synchronized_terminal_update(&mut terminal, |terminal| {
            terminal.try_draw_unflushed(|frame| {
                frame
                    .buffer_mut()
                    .set_string(0, 0, "failed", Style::default());
                Result::Ok(())
            })
        });
        assert_eq!(
            result.expect_err("draw should fail").to_string(),
            "injected draw failure"
        );

        let failed = terminal
            .backend()
            .last_frame()
            .expect("failed draw should still flush END");
        assert!(byte_index(&failed.raw_bytes, &begin) < byte_index(&failed.raw_bytes, &end));

        terminal.backend_mut().clear_frames();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            synchronized_terminal_update(&mut terminal, |_terminal| -> Result<()> {
                panic!("injected render panic");
            })
        }));
        assert!(panic.is_err());

        let panicked = terminal
            .backend()
            .last_frame()
            .expect("panicked draw should still flush END");
        assert!(byte_index(&panicked.raw_bytes, &begin) < byte_index(&panicked.raw_bytes, &end));
    }

    #[cfg(unix)]
    #[test]
    fn synchronized_resume_uses_one_backend_flush() {
        let backend = AuditedVT100Backend::new(20, 2);
        let mut terminal = CustomTerminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 20, 2));

        synchronized_terminal_update(&mut terminal, |terminal| {
            job_control::PreparedResumeAction::RestoreAltScreen {
                use_mouse_capture: false,
            }
            .apply(terminal)?;
            terminal.try_draw_unflushed(|frame| {
                frame
                    .buffer_mut()
                    .set_string(0, 0, "resumed", Style::default());
                Result::Ok(())
            })
        })
        .expect("synchronized resumed draw");

        assert_eq!(terminal.backend().frames().len(), 1);
        let frame = terminal.backend().last_frame().expect("resumed frame");
        let begin = command_bytes(BeginSynchronizedUpdate);
        let end = command_bytes(EndSynchronizedUpdate);
        let begin_index = byte_index(&frame.raw_bytes, &begin);
        let text_index = byte_index(&frame.raw_bytes, b"resumed");
        let end_index = byte_index(&frame.raw_bytes, &end);
        assert!(begin_index < text_index && text_index < end_index);
    }

    #[cfg(unix)]
    #[test]
    fn resume_alt_screen_clears_before_first_draw() {
        let backend = AuditedVT100Backend::new(20, 2);
        let mut terminal = CustomTerminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 20, 2));
        terminal
            .draw(|frame| {
                frame
                    .buffer_mut()
                    .set_string(0, 0, "restored", Style::default());
            })
            .expect("initial draw");
        terminal.backend_mut().clear_frames();

        job_control::PreparedResumeAction::RestoreAltScreen {
            use_mouse_capture: false,
        }
        .apply(&mut terminal)
        .expect("prepare resumed alternate screen");
        terminal
            .draw(|frame| {
                frame
                    .buffer_mut()
                    .set_string(0, 0, "restored", Style::default());
            })
            .expect("first resumed draw");

        let frame = terminal.backend().last_frame().expect("resumed frame");
        let clear_index = byte_index(&frame.raw_bytes, b"\x1b[K");
        let text_index = byte_index(&frame.raw_bytes, b"restored");
        assert!(clear_index < text_index, "{}", frame.escaped_ansi());
    }

    #[cfg(unix)]
    #[test]
    fn resume_inline_screen_clears_before_first_draw() {
        let backend = AuditedVT100Backend::new(20, 2);
        let mut terminal = CustomTerminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 20, 2));
        terminal
            .draw(|frame| {
                frame
                    .buffer_mut()
                    .set_string(0, 0, "restored", Style::default());
            })
            .expect("initial draw");
        terminal.backend_mut().clear_frames();

        job_control::PreparedResumeAction::RestoreInlineScreen
            .apply(&mut terminal)
            .expect("prepare resumed inline screen");
        terminal
            .draw(|frame| {
                frame
                    .buffer_mut()
                    .set_string(0, 0, "restored", Style::default());
            })
            .expect("first resumed draw");

        let frame = terminal.backend().last_frame().expect("resumed frame");
        let clear_index = byte_index(&frame.raw_bytes, b"\x1b[K");
        let text_index = byte_index(&frame.raw_bytes, b"restored");
        assert!(clear_index < text_index, "{}", frame.escaped_ansi());
    }
}
