use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::num::NonZeroUsize;
use std::time::Duration;
use std::time::Instant;

use lru::LruCache;
use sha2::Digest;
use sha2::Sha256;
use tokio::sync::Mutex;

use crate::product::agent::input_slimming::DEFAULT_STORE_CAPACITY;
use crate::product::agent::input_slimming::DEFAULT_STORE_TTL_SECONDS;
use crate::product::agent::input_slimming::InputSlimmingRef;
use crate::product::agent::input_slimming::InputSlimmingStrategy;
use crate::product::agent::input_slimming::InputSlimmingTokenGateDecision;
use crate::product::agent::input_slimming::candidate::CandidateZone;
use crate::product::agent::truncate::TruncationPolicy;
use crate::product::agent::truncate::truncate_text;

pub(crate) const INPUT_RETRIEVE_MAX_TOKENS: usize = 20_000;
const QUERY_CONTEXT_LINES: usize = 2;

#[derive(Debug)]
pub(crate) struct InputSlimmingStore {
    inner: Mutex<StoreInner>,
}

#[derive(Debug)]
struct StoreInner {
    entries: LruCache<String, StoredInput>,
    slimmed_replacements: LruCache<SlimmedReplacementCacheKey, CachedSlimmedReplacement>,
    ttl: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredInput {
    pub(crate) original: String,
    pub(crate) metadata: StoredInputMetadata,
    pub(crate) retrieval_count: u64,
    created_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredInputMetadata {
    pub(crate) strategy: InputSlimmingStrategy,
    pub(crate) tool_name: String,
    pub(crate) original_tokens: usize,
    pub(crate) compressed_tokens: usize,
    pub(crate) created_turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SlimmedReplacementCacheKey {
    pub(crate) original_hash: String,
    pub(crate) tool_name: String,
    pub(crate) zone: CandidateZone,
    pub(crate) success: Option<bool>,
    pub(crate) strategy_version: u32,
    pub(crate) min_candidate_tokens: usize,
    pub(crate) target_tokens: usize,
    pub(crate) min_saved_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlimmedReplacement {
    pub(crate) replacement: String,
    pub(crate) reference: InputSlimmingRef,
    pub(crate) gate: InputSlimmingTokenGateDecision,
    pub(crate) metadata: StoredInputMetadata,
    pub(crate) replacement_tokens_approx: usize,
}

#[derive(Debug, Clone)]
struct CachedSlimmedReplacement {
    replacement: SlimmedReplacement,
    created_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetrieveResult {
    pub(crate) content: String,
    pub(crate) success: bool,
    pub(crate) strategy: Option<InputSlimmingStrategy>,
    pub(crate) tool_name: Option<String>,
    pub(crate) query_matched: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputSlimmingStoreError {
    HashMismatch { expected: String, actual: String },
}

impl fmt::Display for InputSlimmingStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HashMismatch { expected, actual } => {
                write!(
                    f,
                    "input slimming store hash mismatch: expected {expected}, got {actual}"
                )
            }
        }
    }
}

impl std::error::Error for InputSlimmingStoreError {}

impl Default for InputSlimmingStore {
    fn default() -> Self {
        Self::new(
            NonZeroUsize::new(DEFAULT_STORE_CAPACITY).unwrap_or(NonZeroUsize::MIN),
            Duration::from_secs(DEFAULT_STORE_TTL_SECONDS),
        )
    }
}

impl InputSlimmingStore {
    pub(crate) fn new(capacity: NonZeroUsize, ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(StoreInner {
                entries: LruCache::new(capacity),
                slimmed_replacements: LruCache::new(capacity),
                ttl,
            }),
        }
    }

    pub(crate) async fn put(&self, original: String, metadata: StoredInputMetadata) -> String {
        let hash = hash_text(&original);
        self.put_validated(hash.clone(), original, metadata).await;
        hash
    }

    pub(crate) async fn put_with_hash(
        &self,
        hash: String,
        original: String,
        metadata: StoredInputMetadata,
    ) -> Result<(), InputSlimmingStoreError> {
        let expected = hash_text(&original);
        if hash != expected {
            return Err(InputSlimmingStoreError::HashMismatch {
                expected,
                actual: hash,
            });
        }
        self.put_validated(hash, original, metadata).await;
        Ok(())
    }

    async fn put_validated(&self, hash: String, original: String, metadata: StoredInputMetadata) {
        let mut guard = self.inner.lock().await;
        guard.entries.put(
            hash,
            StoredInput {
                original,
                metadata,
                retrieval_count: 0,
                created_at: Instant::now(),
            },
        );
    }

    pub(crate) async fn get(&self, hash: &str) -> Option<StoredInput> {
        let mut guard = self.inner.lock().await;
        let ttl = guard.ttl;
        let entry = guard.entries.get(hash).cloned()?;
        if entry.created_at.elapsed() >= ttl {
            guard.entries.pop(hash);
            return None;
        }
        Some(entry)
    }

    pub(crate) async fn get_slimmed_replacement(
        &self,
        key: &SlimmedReplacementCacheKey,
    ) -> Option<SlimmedReplacement> {
        let mut guard = self.inner.lock().await;
        let ttl = guard.ttl;
        let entry = guard.slimmed_replacements.get(key).cloned()?;
        if entry.created_at.elapsed() >= ttl {
            guard.slimmed_replacements.pop(key);
            return None;
        }
        Some(entry.replacement)
    }

    pub(crate) async fn put_slimmed_replacement(
        &self,
        key: SlimmedReplacementCacheKey,
        replacement: SlimmedReplacement,
    ) {
        let mut guard = self.inner.lock().await;
        guard.slimmed_replacements.put(
            key,
            CachedSlimmedReplacement {
                replacement,
                created_at: Instant::now(),
            },
        );
    }

    pub(crate) async fn retrieve(&self, hash: &str, query: Option<&str>) -> RetrieveResult {
        let Some(entry) = self.get_for_retrieval(hash).await else {
            return RetrieveResult {
                content: format!(
                    "Input Slimming store miss for hash `{hash}`. The original may be unavailable because the marker predates resume-safe storage, expired, or the rollout entry is missing."
                ),
                success: false,
                strategy: None,
                tool_name: None,
                query_matched: query.map(|_| false),
            };
        };

        let (content, query_matched) = match query.map(str::trim).filter(|query| !query.is_empty())
        {
            Some(query) => {
                let query_result = retrieve_query(&entry, hash, query);
                (query_result.content, Some(query_result.matched))
            }
            None => (retrieve_full(&entry, hash), None),
        };

        RetrieveResult {
            content,
            success: true,
            strategy: Some(entry.metadata.strategy),
            tool_name: Some(entry.metadata.tool_name),
            query_matched,
        }
    }

    async fn get_for_retrieval(&self, hash: &str) -> Option<StoredInput> {
        let mut guard = self.inner.lock().await;
        let ttl = guard.ttl;
        let entry = guard.entries.get_mut(hash)?;
        if entry.created_at.elapsed() >= ttl {
            guard.entries.pop(hash);
            return None;
        }
        entry.retrieval_count = entry.retrieval_count.saturating_add(1);
        Some(entry.clone())
    }
}

pub(crate) fn hash_text(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let hex = format!("{digest:x}");
    hex[..24].to_string()
}

fn retrieve_full(entry: &StoredInput, hash: &str) -> String {
    let header = metadata_header(entry, hash);
    let truncated = truncate_text(
        &entry.original,
        TruncationPolicy::Tokens(INPUT_RETRIEVE_MAX_TOKENS),
    );
    if truncated == entry.original {
        format!("{header}\n\n{}", entry.original)
    } else {
        format!(
            "{header}\n\nOriginal input was larger than the retrieval budget; returning a head/tail view.\n\n{truncated}"
        )
    }
}

struct QueryRetrieveResult {
    content: String,
    matched: bool,
}

fn retrieve_query(entry: &StoredInput, hash: &str, query: &str) -> QueryRetrieveResult {
    if let Some(content) = retrieve_path_query(entry, hash, query) {
        return QueryRetrieveResult {
            content,
            matched: true,
        };
    }
    if let Some(content) = retrieve_section_query(entry, hash, query) {
        return QueryRetrieveResult {
            content,
            matched: true,
        };
    }
    retrieve_line_query(entry, hash, query)
}

fn metadata_header(entry: &StoredInput, hash: &str) -> String {
    format!(
        "Original input for <<lha-input:{hash}>>:\nstrategy={}\ntool_name={}\noriginal_tokens={}\ncompressed_tokens={}\ncreated_turn_id={}",
        entry.metadata.strategy.as_str(),
        entry.metadata.tool_name,
        entry.metadata.original_tokens,
        entry.metadata.compressed_tokens,
        entry.metadata.created_turn_id
    )
}

fn retrieve_path_query(entry: &StoredInput, hash: &str, query: &str) -> Option<String> {
    let query_lower = query.to_lowercase();
    let lines: Vec<&str> = entry.original.lines().collect();
    let mut grouped: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for (idx, line) in lines.iter().enumerate() {
        let Some(path) = path_line_prefix(line) else {
            continue;
        };
        if line.to_lowercase().contains(&query_lower) || path.to_lowercase().contains(&query_lower)
        {
            let start = idx.saturating_sub(QUERY_CONTEXT_LINES);
            let end = (idx + QUERY_CONTEXT_LINES + 1).min(lines.len());
            grouped
                .entry(path.to_string())
                .or_default()
                .extend(start..end);
        }
    }
    if grouped.is_empty() {
        return None;
    }

    let mut out = format!(
        "{}\n\nPath-grouped matches for query `{query}`:\n",
        metadata_header(entry, hash)
    );
    for (path, indices) in grouped {
        out.push_str(&format!("\n# {path}\n"));
        let mut last = None;
        for idx in indices {
            if let Some(prev) = last
                && idx > prev + 1
            {
                out.push_str("...\n");
            }
            last = Some(idx);
            out.push_str(&format!("{}:{}\n", idx + 1, lines[idx]));
        }
    }
    Some(truncate_text(
        &out,
        TruncationPolicy::Tokens(INPUT_RETRIEVE_MAX_TOKENS),
    ))
}

fn path_line_prefix(line: &str) -> Option<&str> {
    let mut parts = line.splitn(3, ':');
    let path = parts.next()?;
    let line_number = parts.next()?;
    if path.is_empty() || line_number.is_empty() || !line_number.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(path)
}

fn retrieve_section_query(entry: &StoredInput, hash: &str, query: &str) -> Option<String> {
    let query_lower = query.to_lowercase();
    let lines: Vec<&str> = entry.original.lines().collect();
    let mut sections = Vec::new();
    let mut start = 0usize;
    let mut saw_heading = false;
    for (idx, line) in lines.iter().enumerate() {
        if line.starts_with('#') {
            saw_heading = true;
            if idx > start {
                sections.push((start, idx));
            }
            start = idx;
        }
    }
    if !saw_heading {
        return None;
    }
    if start < lines.len() {
        sections.push((start, lines.len()));
    }

    let mut selected = Vec::new();
    for (start, end) in sections {
        let section_text = lines[start..end].join("\n");
        if section_text.to_lowercase().contains(&query_lower) {
            selected.push((start, end.min(start + 80)));
        }
    }
    if selected.is_empty() {
        return None;
    }

    let mut out = format!(
        "{}\n\nSection matches for query `{query}`:\n",
        metadata_header(entry, hash)
    );
    for (start, end) in selected {
        out.push_str("\n--- section ---\n");
        for line in lines.iter().take(end).skip(start) {
            out.push_str(line);
            out.push('\n');
        }
        if end < lines.len() {
            out.push_str("...\n");
        }
    }
    Some(truncate_text(
        &out,
        TruncationPolicy::Tokens(INPUT_RETRIEVE_MAX_TOKENS),
    ))
}

fn retrieve_line_query(entry: &StoredInput, hash: &str, query: &str) -> QueryRetrieveResult {
    let query_lower = query.to_lowercase();
    let lines: Vec<&str> = entry.original.lines().collect();
    let mut selected = BTreeSet::new();
    for (idx, line) in lines.iter().enumerate() {
        if line.to_lowercase().contains(&query_lower) {
            let start = idx.saturating_sub(QUERY_CONTEXT_LINES);
            let end = (idx + QUERY_CONTEXT_LINES + 1).min(lines.len());
            selected.extend(start..end);
        }
    }

    if selected.is_empty() {
        return QueryRetrieveResult {
            content: format!(
                "{}\n\nInput Slimming entry <<lha-input:{hash}>> exists, but query `{query}` did not match any lines or sections.",
                metadata_header(entry, hash)
            ),
            matched: false,
        };
    }

    let mut out = format!(
        "{}\n\nMatches for query `{query}` in <<lha-input:{hash}>>:\n",
        metadata_header(entry, hash)
    );
    let mut last = None;
    for idx in selected {
        if let Some(prev) = last
            && idx > prev + 1
        {
            out.push_str("...\n");
        }
        last = Some(idx);
        out.push_str(&format!("{}:{}\n", idx + 1, lines[idx]));
    }

    QueryRetrieveResult {
        content: truncate_text(&out, TruncationPolicy::Tokens(INPUT_RETRIEVE_MAX_TOKENS)),
        matched: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::agent::input_slimming::InputSlimmingStrategy;
    use pretty_assertions::assert_eq;

    fn metadata() -> StoredInputMetadata {
        StoredInputMetadata {
            strategy: InputSlimmingStrategy::PlainTextHeadTail,
            tool_name: "shell".to_string(),
            original_tokens: 42,
            compressed_tokens: 7,
            created_turn_id: "turn-1".to_string(),
        }
    }

    #[test]
    fn hash_is_stable_and_24_hex_chars() {
        let first = hash_text("same payload");
        let second = hash_text("same payload");

        assert_eq!(first, second);
        assert_eq!(first.len(), 24);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(first, hash_text("different payload"));
    }

    #[tokio::test]
    async fn stored_payload_can_be_retrieved() {
        let store = InputSlimmingStore::new(
            NonZeroUsize::new(2).expect("non-zero"),
            Duration::from_secs(60),
        );
        let hash = store.put("alpha\nbeta".to_string(), metadata()).await;

        let got = store.get(&hash).await.expect("entry");

        assert_eq!(got.original, "alpha\nbeta");
        assert_eq!(got.metadata, metadata());
        assert_eq!(got.retrieval_count, 0);
    }

    #[tokio::test]
    async fn put_with_hash_accepts_matching_hash() {
        let store = InputSlimmingStore::default();
        let original = "alpha\nbeta".to_string();
        let hash = hash_text(&original);

        store
            .put_with_hash(hash.clone(), original.clone(), metadata())
            .await
            .expect("matching hash should store");

        assert_eq!(
            store.get(&hash).await.map(|entry| entry.original),
            Some(original)
        );
    }

    #[tokio::test]
    async fn put_with_hash_rejects_mismatched_hash() {
        let store = InputSlimmingStore::default();
        let err = store
            .put_with_hash("abc123".to_string(), "alpha".to_string(), metadata())
            .await
            .expect_err("mismatched hash should fail");

        assert!(matches!(err, InputSlimmingStoreError::HashMismatch { .. }));
        assert_eq!(store.get("abc123").await, None);
    }

    #[tokio::test]
    async fn put_reuses_hash_text_and_retrieves() {
        let store = InputSlimmingStore::default();
        let original = "same payload".to_string();
        let expected_hash = hash_text(&original);

        let hash = store.put(original.clone(), metadata()).await;

        assert_eq!(hash, expected_hash);
        assert_eq!(
            store.get(&hash).await.map(|entry| entry.original),
            Some(original)
        );
    }

    #[tokio::test]
    async fn retrieval_increments_count_and_returns_metadata() {
        let store = InputSlimmingStore::default();
        let hash = store.put("alpha\nbeta".to_string(), metadata()).await;

        let result = store.retrieve(&hash, None).await;
        let got = store.get(&hash).await.expect("entry");

        assert!(result.success);
        assert!(result.content.contains("strategy=plain_text_head_tail"));
        assert!(result.content.contains("tool_name=shell"));
        assert!(result.content.contains("created_turn_id=turn-1"));
        assert_eq!(got.retrieval_count, 1);
    }

    #[tokio::test]
    async fn ttl_expiry_returns_miss() {
        let store = InputSlimmingStore::new(
            NonZeroUsize::new(2).expect("non-zero"),
            Duration::from_millis(0),
        );
        let hash = store.put("alpha".to_string(), metadata()).await;

        assert_eq!(store.get(&hash).await, None);
        let result = store.retrieve(&hash, None).await;
        assert!(!result.success);
        assert!(result.content.contains(&hash));
    }

    #[tokio::test]
    async fn lru_capacity_evicts_old_entries() {
        let store = InputSlimmingStore::new(
            NonZeroUsize::new(1).expect("non-zero"),
            Duration::from_secs(60),
        );
        let first = store.put("first".to_string(), metadata()).await;
        let second = store.put("second".to_string(), metadata()).await;

        assert_eq!(store.get(&first).await, None);
        assert_eq!(
            store.get(&second).await.map(|entry| entry.original),
            Some("second".to_string())
        );
    }

    #[tokio::test]
    async fn query_retrieval_returns_matching_lines_with_context() {
        let store = InputSlimmingStore::default();
        let hash = store
            .put("one\ntwo\nneedle\nfour\nfive".to_string(), metadata())
            .await;

        let result = store.retrieve(&hash, Some("needle")).await;

        assert!(result.success);
        assert_eq!(result.query_matched, Some(true));
        assert!(result.content.contains("3:needle"));
        assert!(result.content.contains("1:one"));
        assert!(result.content.contains("5:five"));
    }

    #[tokio::test]
    async fn path_query_retrieval_groups_matches() {
        let store = InputSlimmingStore::default();
        let hash = store
            .put(
                "src/main.rs:10:alpha\nsrc/main.rs:20:needle\nsrc/lib.rs:3:needle".to_string(),
                metadata(),
            )
            .await;

        let result = store.retrieve(&hash, Some("src/main.rs")).await;

        assert!(result.success);
        assert!(result.content.contains("Path-grouped matches"));
        assert!(result.content.contains("# src/main.rs"));
        assert!(result.content.contains("src/main.rs:20:needle"));
    }

    #[tokio::test]
    async fn section_query_retrieval_returns_section() {
        let store = InputSlimmingStore::default();
        let hash = store
            .put("# Alpha\none\n# Beta\nneedle\nmore".to_string(), metadata())
            .await;

        let result = store.retrieve(&hash, Some("needle")).await;

        assert!(result.success);
        assert!(result.content.contains("Section matches"));
        assert!(result.content.contains("# Beta"));
        assert!(result.content.contains("needle"));
    }

    #[tokio::test]
    async fn query_retrieval_with_no_match_is_clear() {
        let store = InputSlimmingStore::default();
        let hash = store.put("alpha".to_string(), metadata()).await;

        let result = store.retrieve(&hash, Some("missing")).await;

        assert!(result.success);
        assert_eq!(result.query_matched, Some(false));
        assert!(
            result
                .content
                .contains("did not match any lines or sections")
        );
    }

    #[tokio::test]
    async fn missing_hash_returns_failure() {
        let store = InputSlimmingStore::default();

        let result = store.retrieve("missing", None).await;

        assert!(!result.success);
        assert!(result.content.contains("missing"));
    }

    #[tokio::test]
    async fn large_retrieval_is_truncated_to_budget() {
        let store = InputSlimmingStore::default();
        let hash = store.put("abcdef".repeat(100_000), metadata()).await;

        let result = store.retrieve(&hash, None).await;

        assert!(result.success);
        assert!(result.content.contains("larger than the retrieval budget"));
        assert!(result.content.contains("truncated"));
    }
}
