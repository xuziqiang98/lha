use lha_llm::ContentItem;
use lha_llm::ToolResultPayload;
use lha_llm::TranscriptItem;
use lha_llm::TurnRequest;
use pretty_assertions::assert_eq;

use super::INPUT_SLIMMING_MARKER_PREFIX;
use super::InputSlimmer;
use super::InputSlimmingStore;
use super::InputSlimmingStrategy;

struct SlimmingEvalCase {
    name: &'static str,
    tool_name: &'static str,
    input: String,
    required_compressed_needles: Vec<&'static str>,
    required_retrieval_needles: Vec<&'static str>,
}

struct SlimmingEvalResult {
    strategy: InputSlimmingStrategy,
    tokens_saved: usize,
    compressed_text: String,
    retrieval_successes: usize,
}

async fn run_slimming_eval_case(case: SlimmingEvalCase) -> SlimmingEvalResult {
    let request = TurnRequest {
        conversation: vec![
            user("old turn"),
            tool_text(case.tool_name, case.input),
            user("latest user turn"),
        ],
        ..Default::default()
    };
    let store = InputSlimmingStore::default();

    let outcome = InputSlimmer::default()
        .slim_request(&request, &store)
        .await
        .unwrap_or_else(|err| panic!("{}: slimming failed: {err}", case.name));

    assert!(
        outcome.requires_retrieval_tool,
        "{}: expected retrieval tool",
        case.name
    );
    assert!(
        outcome.approx_tokens_saved > 0,
        "{}: expected token savings",
        case.name
    );
    assert_eq!(
        outcome.metrics.refs.len(),
        1,
        "{}: expected one ref",
        case.name
    );

    let compressed_text = tool_output_text(&outcome.request.conversation[1])
        .unwrap_or_else(|| panic!("{}: missing compressed tool output", case.name))
        .to_string();
    assert!(
        compressed_text.contains(INPUT_SLIMMING_MARKER_PREFIX),
        "{}: expected marker",
        case.name
    );
    for needle in &case.required_compressed_needles {
        assert!(
            compressed_text.contains(needle),
            "{}: compressed output missing `{needle}`",
            case.name
        );
    }

    let reference = &outcome.metrics.refs[0];
    let mut retrieval_successes = 0usize;
    for needle in &case.required_retrieval_needles {
        let retrieval = store.retrieve(&reference.hash, Some(needle)).await;
        assert!(retrieval.success, "{}: retrieval failed", case.name);
        assert!(
            retrieval.content.contains(needle),
            "{}: retrieval missing `{needle}`",
            case.name
        );
        retrieval_successes += 1;
    }

    SlimmingEvalResult {
        strategy: reference.strategy,
        tokens_saved: outcome.approx_tokens_saved,
        compressed_text,
        retrieval_successes,
    }
}

fn user(text: &str) -> TranscriptItem {
    TranscriptItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        end_turn: None,
    }
}

fn tool_text(tool_name: &str, output: String) -> TranscriptItem {
    TranscriptItem::ToolResult {
        call_id: "call".to_string(),
        tool_name: tool_name.to_string(),
        payload: ToolResultPayload::Text { output },
    }
}

fn tool_output_text(item: &TranscriptItem) -> Option<&str> {
    match item {
        TranscriptItem::ToolResult {
            payload: ToolResultPayload::Text { output },
            ..
        } => Some(output.as_str()),
        TranscriptItem::Message { .. }
        | TranscriptItem::Reasoning { .. }
        | TranscriptItem::HostedActivity { .. }
        | TranscriptItem::ToolCall { .. }
        | TranscriptItem::ToolResult { .. }
        | TranscriptItem::Unknown { .. } => None,
    }
}

fn json_eval_input() -> String {
    let items = (0..260)
        .map(|idx| {
            if idx == 40 {
                serde_json::json!({
                    "id": idx,
                    "status": "error",
                    "message": "JSON_ERROR_NEEDLE",
                    "payload": "x".repeat(100),
                })
            } else if idx == 90 {
                serde_json::json!({
                    "id": idx,
                    "status": "ok",
                    "rare_key": "JSON_RARE_NEEDLE",
                    "payload": "x".repeat(100),
                })
            } else if idx == 120 {
                serde_json::json!({
                    "id": idx,
                    "status": "ok",
                    "latency_ms": 50_000,
                    "payload": "x".repeat(100),
                })
            } else {
                serde_json::json!({
                    "id": idx,
                    "status": "ok",
                    "latency_ms": idx,
                    "payload": "x".repeat(100),
                })
            }
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&items).expect("json")
}

fn cargo_log_eval_input() -> String {
    let mut lines = (0..600)
        .map(|idx| format!("running test noise_{idx}"))
        .collect::<Vec<_>>();
    lines.splice(
        250..250,
        [
            "error[E0425]: cannot find value `STACK_TRACE_NEEDLE` in this scope",
            "   --> src/lib.rs:10:5",
            "stack backtrace:",
            "   0: lha::STACK_TRACE_NEEDLE",
            "test result: FAILED. 1 passed; 1 failed",
            "exit code: 101",
        ]
        .into_iter()
        .map(str::to_string),
    );
    lines.join("\n")
}

fn search_eval_input() -> String {
    let mut lines = (0..260)
        .map(|idx| format!("src/bulk_{idx}.rs:1:ordinary match {}", "x".repeat(50)))
        .collect::<Vec<_>>();
    lines.push(r"C:\repo\pre-commit-config.yaml:42:error SEARCH_WINDOWS_NEEDLE".to_string());
    lines.push("src/pre-commit-config.yaml-7-warning SEARCH_DASH_NEEDLE".to_string());
    lines.join("\n")
}

fn diff_eval_input() -> String {
    let mut text = String::new();
    text.push_str("diff --git a/a.rs b/a.rs\nindex 1..2\n--- a/a.rs\n+++ b/a.rs\n");
    text.push_str("@@ -1,3000 +1,3000 @@\n");
    for idx in 0..3_000 {
        if idx == 1_500 {
            text.push_str("+fn DIFF_CRITICAL_NEEDLE() { unsafe { panic!(\"token\") } }\n");
        } else {
            text.push_str(&format!("+ordinary changed line {idx}\n"));
        }
    }
    text.push_str("diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n");
    text.push_str("@@ -1,2 +1,2 @@\n-old\n+new\n");
    text
}

fn omitted_needle_input() -> String {
    format!(
        "{}\nOMITTED_RETRIEVAL_NEEDLE\n{}",
        "head ".repeat(8_000),
        "tail ".repeat(8_000)
    )
}

#[tokio::test]
async fn compression_only_eval_json_log_search_diff_and_retrieval() {
    let cases = vec![
        SlimmingEvalCase {
            name: "json",
            tool_name: "json_search",
            input: json_eval_input(),
            required_compressed_needles: vec!["JSON_ERROR_NEEDLE", "JSON_RARE_NEEDLE", "50000"],
            required_retrieval_needles: vec!["JSON_ERROR_NEEDLE"],
        },
        SlimmingEvalCase {
            name: "cargo-log",
            tool_name: "cargo test",
            input: cargo_log_eval_input(),
            required_compressed_needles: vec!["STACK_TRACE_NEEDLE", "test result: FAILED"],
            required_retrieval_needles: vec!["STACK_TRACE_NEEDLE"],
        },
        SlimmingEvalCase {
            name: "search",
            tool_name: "rg",
            input: search_eval_input(),
            required_compressed_needles: vec!["SEARCH_WINDOWS_NEEDLE", "SEARCH_DASH_NEEDLE"],
            required_retrieval_needles: vec!["SEARCH_DASH_NEEDLE"],
        },
        SlimmingEvalCase {
            name: "diff",
            tool_name: "diff",
            input: diff_eval_input(),
            required_compressed_needles: vec!["DIFF_CRITICAL_NEEDLE", "diff --git a/b.rs b/b.rs"],
            required_retrieval_needles: vec!["DIFF_CRITICAL_NEEDLE"],
        },
        SlimmingEvalCase {
            name: "retrieval",
            tool_name: "shell",
            input: omitted_needle_input(),
            required_compressed_needles: vec!["plain_text_head_tail"],
            required_retrieval_needles: vec!["OMITTED_RETRIEVAL_NEEDLE"],
        },
    ];

    let mut strategies = Vec::new();
    for case in cases {
        let result = run_slimming_eval_case(case).await;
        assert!(result.tokens_saved > 0);
        assert!(result.retrieval_successes > 0);
        assert!(
            result
                .compressed_text
                .contains(INPUT_SLIMMING_MARKER_PREFIX)
        );
        strategies.push(result.strategy);
    }

    assert_eq!(
        strategies,
        vec![
            InputSlimmingStrategy::JsonArraySample,
            InputSlimmingStrategy::LogCompact,
            InputSlimmingStrategy::SearchResultCompact,
            InputSlimmingStrategy::DiffCompact,
            InputSlimmingStrategy::PlainTextHeadTail,
        ]
    );
}
