use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::collections::btree_map::Entry;
use std::fs;
use std::io::Write;
use std::io::{self};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use lha_protocol::ThreadId;
use lha_protocol::protocol::SessionSource;
use serde::Deserialize;
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tracing::Event;
use tracing::Level;
use tracing::field::Visit;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::writer::MakeWriter;
use tracing_subscriber::registry::LookupSpan;

const DEFAULT_MAX_BYTES: usize = 4 * 1024 * 1024; // 4 MiB
const FEEDBACK_TAGS_TARGET: &str = "feedback_tags";
const MAX_FEEDBACK_TAGS: usize = 64;
const FEEDBACK_SUBDIR: &str = "feedback";
const LOG_FILENAME: &str = "lha-logs.log";
const METADATA_FILENAME: &str = "metadata.json";

#[derive(Clone)]
pub struct CodexFeedback {
    inner: Arc<FeedbackInner>,
}

impl Default for CodexFeedback {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexFeedback {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_MAX_BYTES)
    }

    pub(crate) fn with_capacity(max_bytes: usize) -> Self {
        Self {
            inner: Arc::new(FeedbackInner::new(max_bytes)),
        }
    }

    pub fn make_writer(&self) -> FeedbackMakeWriter {
        FeedbackMakeWriter {
            inner: self.inner.clone(),
        }
    }

    /// Returns a [`tracing_subscriber`] layer that captures full-fidelity logs into this feedback
    /// ring buffer.
    ///
    /// This is intended for initialization code so call sites don't have to duplicate the exact
    /// `fmt::layer()` configuration and filter logic.
    pub fn logger_layer<S>(&self) -> impl Layer<S> + Send + Sync + 'static
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        tracing_subscriber::fmt::layer()
            .with_writer(self.make_writer())
            .with_ansi(false)
            .with_target(false)
            // Capture everything, regardless of the caller's `RUST_LOG`, so feedback includes the
            // full trace when the user saves a feedback bundle.
            .with_filter(Targets::new().with_default(Level::TRACE))
    }

    /// Returns a [`tracing_subscriber`] layer that collects structured metadata for feedback.
    ///
    /// Events with `target: "feedback_tags"` are treated as key/value tags to attach to feedback
    /// bundles later.
    pub fn metadata_layer<S>(&self) -> impl Layer<S> + Send + Sync + 'static
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        FeedbackMetadataLayer {
            inner: self.inner.clone(),
        }
        .with_filter(Targets::new().with_target(FEEDBACK_TAGS_TARGET, Level::TRACE))
    }

    pub fn snapshot(&self, session_id: Option<ThreadId>) -> CodexLogSnapshot {
        let bytes = {
            let guard = self.inner.ring.lock().expect("mutex poisoned");
            guard.snapshot_bytes()
        };
        let tags = {
            let guard = self.inner.tags.lock().expect("mutex poisoned");
            guard.clone()
        };
        CodexLogSnapshot {
            bytes,
            tags,
            thread_id: session_id
                .map(|id| id.to_string())
                .unwrap_or("no-active-thread-".to_string() + &ThreadId::new().to_string()),
        }
    }
}

struct FeedbackInner {
    ring: Mutex<RingBuffer>,
    tags: Mutex<BTreeMap<String, String>>,
}

impl FeedbackInner {
    fn new(max_bytes: usize) -> Self {
        Self {
            ring: Mutex::new(RingBuffer::new(max_bytes)),
            tags: Mutex::new(BTreeMap::new()),
        }
    }
}

#[derive(Clone)]
pub struct FeedbackMakeWriter {
    inner: Arc<FeedbackInner>,
}

impl<'a> MakeWriter<'a> for FeedbackMakeWriter {
    type Writer = FeedbackWriter;

    fn make_writer(&'a self) -> Self::Writer {
        FeedbackWriter {
            inner: self.inner.clone(),
        }
    }
}

pub struct FeedbackWriter {
    inner: Arc<FeedbackInner>,
}

impl Write for FeedbackWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut guard = self.inner.ring.lock().map_err(|_| io::ErrorKind::Other)?;
        guard.push_bytes(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct RingBuffer {
    max: usize,
    buf: VecDeque<u8>,
}

impl RingBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            max: capacity,
            buf: VecDeque::with_capacity(capacity),
        }
    }

    fn len(&self) -> usize {
        self.buf.len()
    }

    fn push_bytes(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        // If the incoming chunk is larger than capacity, keep only the trailing bytes.
        if data.len() >= self.max {
            self.buf.clear();
            let start = data.len() - self.max;
            self.buf.extend(data[start..].iter().copied());
            return;
        }

        // Evict from the front if we would exceed capacity.
        let needed = self.len() + data.len();
        if needed > self.max {
            let to_drop = needed - self.max;
            for _ in 0..to_drop {
                let _ = self.buf.pop_front();
            }
        }

        self.buf.extend(data.iter().copied());
    }

    fn snapshot_bytes(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }
}

pub struct CodexLogSnapshot {
    bytes: Vec<u8>,
    tags: BTreeMap<String, String>,
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedFeedback {
    pub thread_id: String,
    pub saved_path: PathBuf,
}

impl CodexLogSnapshot {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn save_to_temp_file(&self) -> io::Result<PathBuf> {
        let dir = std::env::temp_dir();
        let filename = format!("lha-feedback-{}.log", self.thread_id);
        let path = dir.join(filename);
        fs::write(&path, self.as_bytes())?;
        Ok(path)
    }

    /// Persist feedback to a local bundle under `LHA_HOME/feedback/`.
    pub fn persist_feedback(
        &self,
        lha_home: &Path,
        classification: &str,
        reason: Option<&str>,
        include_logs: bool,
        rollout_path: Option<&Path>,
        session_source: Option<SessionSource>,
    ) -> Result<PersistedFeedback> {
        let created_at = OffsetDateTime::now_utc();
        let timestamp_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let saved_path =
            create_feedback_bundle_dir(lha_home, created_at, timestamp_ms, &self.thread_id)?;

        let logs_filename = if include_logs {
            fs::write(saved_path.join(LOG_FILENAME), &self.bytes)?;
            Some(LOG_FILENAME.to_string())
        } else {
            None
        };

        let rollout_filename = if include_logs {
            persist_rollout_attachment(&saved_path, rollout_path)?
        } else {
            None
        };

        let tags = self.feedback_tags(classification, reason, session_source.as_ref());
        let metadata = FeedbackMetadata {
            schema_version: 1,
            created_at: created_at.format(&Rfc3339)?,
            thread_id: self.thread_id.clone(),
            classification: classification.to_string(),
            reason: reason.map(str::to_string),
            include_logs,
            session_source: session_source.map(|source| source.to_string()),
            cli_version: env!("CARGO_PKG_VERSION").to_string(),
            tags,
            files: FeedbackFiles {
                logs: logs_filename,
                rollout: rollout_filename,
            },
        };
        fs::write(
            saved_path.join(METADATA_FILENAME),
            serde_json::to_vec_pretty(&metadata)?,
        )?;

        Ok(PersistedFeedback {
            thread_id: self.thread_id.clone(),
            saved_path,
        })
    }

    fn feedback_tags(
        &self,
        classification: &str,
        reason: Option<&str>,
        session_source: Option<&SessionSource>,
    ) -> BTreeMap<String, String> {
        let mut tags = BTreeMap::from([
            (String::from("thread_id"), self.thread_id.to_string()),
            (String::from("classification"), classification.to_string()),
            (
                String::from("cli_version"),
                env!("CARGO_PKG_VERSION").to_string(),
            ),
        ]);
        if let Some(source) = session_source {
            tags.insert(String::from("session_source"), source.to_string());
        }
        if let Some(value) = reason {
            tags.insert(String::from("reason"), value.to_string());
        }

        let reserved = [
            "thread_id",
            "classification",
            "cli_version",
            "session_source",
            "reason",
        ];
        for (key, value) in &self.tags {
            if reserved.contains(&key.as_str()) {
                continue;
            }
            if let Entry::Vacant(entry) = tags.entry(key.clone()) {
                entry.insert(value.clone());
            }
        }

        tags
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct FeedbackMetadata {
    schema_version: u32,
    created_at: String,
    thread_id: String,
    classification: String,
    reason: Option<String>,
    include_logs: bool,
    session_source: Option<String>,
    cli_version: String,
    tags: BTreeMap<String, String>,
    files: FeedbackFiles,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct FeedbackFiles {
    logs: Option<String>,
    rollout: Option<String>,
}

fn create_feedback_bundle_dir(
    lha_home: &Path,
    created_at: OffsetDateTime,
    timestamp_ms: u128,
    thread_id: &str,
) -> io::Result<PathBuf> {
    let root = feedback_day_dir(lha_home, created_at);
    fs::create_dir_all(&root)?;

    let base_name = format!("feedback-{timestamp_ms}-{thread_id}");
    for suffix in 0.. {
        let dir_name = if suffix == 0 {
            base_name.clone()
        } else {
            format!("{base_name}-{suffix}")
        };
        let candidate = root.join(dir_name);
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }

    Err(io::Error::other(
        "failed to create feedback bundle directory",
    ))
}

fn feedback_day_dir(lha_home: &Path, created_at: OffsetDateTime) -> PathBuf {
    lha_home
        .join(FEEDBACK_SUBDIR)
        .join(format!("{:04}", created_at.year()))
        .join(format!("{:02}", u8::from(created_at.month())))
        .join(format!("{:02}", created_at.day()))
}

fn persist_rollout_attachment(
    saved_path: &Path,
    rollout_path: Option<&Path>,
) -> io::Result<Option<String>> {
    let Some(path) = rollout_path else {
        return Ok(None);
    };
    let Ok(data) = fs::read(path) else {
        return Ok(None);
    };
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "rollout.jsonl".to_string());
    fs::write(saved_path.join(&filename), data)?;
    Ok(Some(filename))
}

#[derive(Clone)]
struct FeedbackMetadataLayer {
    inner: Arc<FeedbackInner>,
}

impl<S> Layer<S> for FeedbackMetadataLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        // This layer is filtered by `Targets`, but keep the guard anyway in case it is used without
        // the filter.
        if event.metadata().target() != FEEDBACK_TAGS_TARGET {
            return;
        }

        let mut visitor = FeedbackTagsVisitor::default();
        event.record(&mut visitor);
        if visitor.tags.is_empty() {
            return;
        }

        let mut guard = self.inner.tags.lock().expect("mutex poisoned");
        for (key, value) in visitor.tags {
            if guard.len() >= MAX_FEEDBACK_TAGS && !guard.contains_key(&key) {
                continue;
            }
            guard.insert(key, value);
        }
    }
}

#[derive(Default)]
struct FeedbackTagsVisitor {
    tags: BTreeMap<String, String>,
}

impl Visit for FeedbackTagsVisitor {
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.tags
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.tags
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.tags
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.tags
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.tags
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.tags
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    #[test]
    fn ring_buffer_drops_front_when_full() {
        let fb = CodexFeedback::with_capacity(8);
        {
            let mut w = fb.make_writer().make_writer();
            w.write_all(b"abcdefgh").unwrap();
            w.write_all(b"ij").unwrap();
        }
        let snap = fb.snapshot(None);
        // Capacity 8: after writing 10 bytes, we should keep the last 8.
        pretty_assertions::assert_eq!(std::str::from_utf8(snap.as_bytes()).unwrap(), "cdefghij");
    }

    #[test]
    fn metadata_layer_records_tags_from_feedback_target() {
        let fb = CodexFeedback::new();
        let _guard = tracing_subscriber::registry()
            .with(fb.metadata_layer())
            .set_default();

        tracing::info!(target: FEEDBACK_TAGS_TARGET, model = "gpt-5", cached = true, "tags");

        let snap = fb.snapshot(None);
        assert_eq!(snap.tags.get("model").map(String::as_str), Some("gpt-5"));
        assert_eq!(snap.tags.get("cached").map(String::as_str), Some("true"));
    }

    #[test]
    fn persist_feedback_without_logs_writes_metadata_only() {
        let lha_home = TempDir::new().expect("tempdir");
        let snap = CodexFeedback::new().snapshot(None);

        let persisted = snap
            .persist_feedback(
                lha_home.path(),
                "good_result",
                Some("nice"),
                false,
                None,
                Some(SessionSource::Cli),
            )
            .expect("persist feedback");

        assert!(
            persisted
                .saved_path
                .starts_with(lha_home.path().join(FEEDBACK_SUBDIR))
        );
        assert!(!persisted.saved_path.join(LOG_FILENAME).exists());
        let metadata = read_metadata(&persisted.saved_path);
        assert_eq!(
            metadata,
            FeedbackMetadata {
                schema_version: 1,
                created_at: metadata.created_at.clone(),
                thread_id: persisted.thread_id.clone(),
                classification: "good_result".to_string(),
                reason: Some("nice".to_string()),
                include_logs: false,
                session_source: Some(SessionSource::Cli.to_string()),
                cli_version: env!("CARGO_PKG_VERSION").to_string(),
                tags: BTreeMap::from([
                    ("classification".to_string(), "good_result".to_string()),
                    (
                        "cli_version".to_string(),
                        env!("CARGO_PKG_VERSION").to_string()
                    ),
                    ("reason".to_string(), "nice".to_string()),
                    ("session_source".to_string(), SessionSource::Cli.to_string()),
                    ("thread_id".to_string(), persisted.thread_id.clone()),
                ]),
                files: FeedbackFiles {
                    logs: None,
                    rollout: None,
                },
            }
        );
    }

    #[test]
    fn persist_feedback_with_logs_and_rollout_writes_bundle_files() {
        let lha_home = TempDir::new().expect("tempdir");
        let fb = CodexFeedback::new();
        {
            let mut writer = fb.make_writer().make_writer();
            writer.write_all(b"log line\n").expect("write log");
        }
        let rollout_path = lha_home.path().join("input-rollout.jsonl");
        fs::write(&rollout_path, "rollout line\n").expect("write rollout");

        let persisted = fb
            .snapshot(None)
            .persist_feedback(
                lha_home.path(),
                "bug",
                None,
                true,
                Some(rollout_path.as_path()),
                Some(SessionSource::Cli),
            )
            .expect("persist feedback");

        assert_eq!(
            fs::read_to_string(persisted.saved_path.join(LOG_FILENAME)).expect("read logs"),
            "log line\n"
        );
        assert_eq!(
            fs::read_to_string(persisted.saved_path.join("input-rollout.jsonl"))
                .expect("read copied rollout"),
            "rollout line\n"
        );

        let metadata = read_metadata(&persisted.saved_path);
        assert_eq!(
            metadata.files,
            FeedbackFiles {
                logs: Some(LOG_FILENAME.to_string()),
                rollout: Some("input-rollout.jsonl".to_string()),
            }
        );
        assert_eq!(metadata.classification, "bug".to_string());
        assert_eq!(metadata.include_logs, true);
    }

    #[test]
    fn persist_feedback_preserves_feedback_tags() {
        let lha_home = TempDir::new().expect("tempdir");
        let fb = CodexFeedback::new();
        let _guard = tracing_subscriber::registry()
            .with(fb.metadata_layer())
            .set_default();

        tracing::info!(target: FEEDBACK_TAGS_TARGET, model = "gpt-5", cached = true, "tags");

        let persisted = fb
            .snapshot(None)
            .persist_feedback(
                lha_home.path(),
                "other",
                Some("details"),
                false,
                None,
                Some(SessionSource::Cli),
            )
            .expect("persist feedback");

        let metadata = read_metadata(&persisted.saved_path);
        assert_eq!(
            metadata.tags.get("model").map(String::as_str),
            Some("gpt-5")
        );
        assert_eq!(
            metadata.tags.get("cached").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            metadata.tags.get("reason").map(String::as_str),
            Some("details")
        );
    }

    #[test]
    fn create_feedback_bundle_dir_appends_suffix_on_collision() {
        let lha_home = TempDir::new().expect("tempdir");
        let created_at = OffsetDateTime::now_utc();
        let first = create_feedback_bundle_dir(lha_home.path(), created_at, 123, "thread")
            .expect("first dir");
        let second = create_feedback_bundle_dir(lha_home.path(), created_at, 123, "thread")
            .expect("second dir");

        assert_eq!(
            first.file_name().and_then(|name| name.to_str()),
            Some("feedback-123-thread")
        );
        assert_eq!(
            second.file_name().and_then(|name| name.to_str()),
            Some("feedback-123-thread-1")
        );
    }

    fn read_metadata(saved_path: &Path) -> FeedbackMetadata {
        serde_json::from_slice(
            &fs::read(saved_path.join(METADATA_FILENAME)).expect("read metadata"),
        )
        .expect("parse metadata")
    }
}
