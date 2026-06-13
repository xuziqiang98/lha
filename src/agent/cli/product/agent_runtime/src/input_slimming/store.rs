use std::num::NonZeroUsize;
use std::time::Duration;
use std::time::Instant;

use lru::LruCache;
use sha2::Digest;
use sha2::Sha256;
use tokio::sync::Mutex;

use crate::product::agent::input_slimming::DEFAULT_STORE_CAPACITY;
use crate::product::agent::input_slimming::DEFAULT_STORE_TTL_SECONDS;
use crate::product::agent::input_slimming::InputSlimmingStrategy;
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
    ttl: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredInput {
    pub(crate) original: String,
    pub(crate) metadata: StoredInputMetadata,
    created_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredInputMetadata {
    pub(crate) strategy: InputSlimmingStrategy,
    pub(crate) tool_name: String,
    pub(crate) original_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetrieveResult {
    pub(crate) content: String,
    pub(crate) success: bool,
}

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
                ttl,
            }),
        }
    }

    pub(crate) async fn put(&self, original: String, metadata: StoredInputMetadata) -> String {
        let hash = hash_text(&original);
        let mut guard = self.inner.lock().await;
        guard.entries.put(
            hash.clone(),
            StoredInput {
                original,
                metadata,
                created_at: Instant::now(),
            },
        );
        hash
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

    pub(crate) async fn retrieve(&self, hash: &str, query: Option<&str>) -> RetrieveResult {
        let Some(entry) = self.get(hash).await else {
            return RetrieveResult {
                content: format!("Input Slimming store miss for hash `{hash}`."),
                success: false,
            };
        };

        let content = match query.map(str::trim).filter(|query| !query.is_empty()) {
            Some(query) => retrieve_query(&entry.original, hash, query),
            None => retrieve_full(&entry.original, hash),
        };

        RetrieveResult {
            content,
            success: true,
        }
    }
}

pub(crate) fn hash_text(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let hex = format!("{digest:x}");
    hex[..24].to_string()
}

fn retrieve_full(original: &str, hash: &str) -> String {
    let truncated = truncate_text(
        original,
        TruncationPolicy::Tokens(INPUT_RETRIEVE_MAX_TOKENS),
    );
    if truncated == original {
        format!("Original input for <<lha-input:{hash}>>:\n\n{original}")
    } else {
        format!(
            "Original input for <<lha-input:{hash}>> was larger than the retrieval budget; returning a head/tail view.\n\n{truncated}"
        )
    }
}

fn retrieve_query(original: &str, hash: &str, query: &str) -> String {
    let query_lower = query.to_lowercase();
    let lines: Vec<&str> = original.lines().collect();
    let mut selected = std::collections::BTreeSet::new();
    for (idx, line) in lines.iter().enumerate() {
        if line.to_lowercase().contains(&query_lower) {
            let start = idx.saturating_sub(QUERY_CONTEXT_LINES);
            let end = (idx + QUERY_CONTEXT_LINES + 1).min(lines.len());
            selected.extend(start..end);
        }
    }

    if selected.is_empty() {
        return format!(
            "Input Slimming entry <<lha-input:{hash}>> exists, but query `{query}` did not match any lines."
        );
    }

    let mut out = format!("Matches for query `{query}` in <<lha-input:{hash}>>:\n");
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

    truncate_text(&out, TruncationPolicy::Tokens(INPUT_RETRIEVE_MAX_TOKENS))
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
    }

    #[tokio::test]
    async fn ttl_expiry_returns_miss() {
        let store = InputSlimmingStore::new(
            NonZeroUsize::new(2).expect("non-zero"),
            Duration::from_millis(0),
        );
        let hash = store.put("alpha".to_string(), metadata()).await;

        assert_eq!(store.get(&hash).await, None);
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
        assert!(result.content.contains("3:needle"));
        assert!(result.content.contains("1:one"));
        assert!(result.content.contains("5:five"));
    }

    #[tokio::test]
    async fn query_retrieval_with_no_match_is_clear() {
        let store = InputSlimmingStore::default();
        let hash = store.put("alpha".to_string(), metadata()).await;

        let result = store.retrieve(&hash, Some("missing")).await;

        assert!(result.success);
        assert!(result.content.contains("did not match"));
    }

    #[tokio::test]
    async fn missing_hash_returns_failure() {
        let store = InputSlimmingStore::default();

        let result = store.retrieve("abc123", None).await;

        assert_eq!(
            result,
            RetrieveResult {
                content: "Input Slimming store miss for hash `abc123`.".to_string(),
                success: false,
            }
        );
    }

    #[tokio::test]
    async fn large_retrieval_is_truncated_to_budget() {
        let store = InputSlimmingStore::default();
        let hash = store.put("x".repeat(100_000), metadata()).await;

        let result = store.retrieve(&hash, None).await;

        assert!(result.success);
        assert!(result.content.contains("larger than the retrieval budget"));
        assert!(result.content.len() < 100_000);
    }
}
