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
    expected_strategy: InputSlimmingStrategy,
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

enum HeadroomFixtureInput {
    SmartCrusher {
        content: String,
        query: &'static str,
        bias: f64,
    },
    Text(String),
}

struct HeadroomFixture {
    name: &'static str,
    transform: &'static str,
    tool_name: &'static str,
    input: HeadroomFixtureInput,
    output_compressed: &'static str,
    expected_strategy: InputSlimmingStrategy,
    semantic_needles: Vec<&'static str>,
    retrieval_needles: Vec<&'static str>,
}

impl HeadroomFixture {
    fn into_eval_case(self) -> SlimmingEvalCase {
        assert!(
            matches!(
                self.transform,
                "smart_crusher" | "log_compressor" | "diff_compressor"
            ),
            "{} fixture transform should be in the LHA-adapted Headroom subset",
            self.name
        );
        for needle in &self.semantic_needles {
            assert!(
                self.output_compressed.contains(needle),
                "{} fixture should derive `{needle}` from Headroom output.compressed",
                self.name
            );
        }

        let input = match self.input {
            HeadroomFixtureInput::SmartCrusher {
                content,
                query,
                bias,
            } => {
                assert_eq!(
                    query, "",
                    "{} fixture should not map query ranking",
                    self.name
                );
                assert_eq!(bias, 1.0, "{} fixture should not map bias knobs", self.name);
                content
            }
            HeadroomFixtureInput::Text(text) => text,
        };

        SlimmingEvalCase {
            name: self.name,
            tool_name: self.tool_name,
            input,
            expected_strategy: self.expected_strategy,
            required_compressed_needles: self.semantic_needles,
            required_retrieval_needles: self.retrieval_needles,
        }
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

fn headroom_smart_crusher_fixture() -> HeadroomFixture {
    let items = (0..180)
        .map(|idx| {
            if idx == 51 {
                serde_json::json!({
                    "id": idx,
                    "level": "warn",
                    "message": "SMART_WARN_NEEDLE",
                    "payload": "x".repeat(140),
                })
            } else if idx == 87 {
                serde_json::json!({
                    "id": idx,
                    "level": "info",
                    "rare_key": "SMART_RARE_NEEDLE",
                    "payload": "x".repeat(140),
                })
            } else if idx == 103 {
                serde_json::json!({
                    "id": idx,
                    "level": "info",
                    "message": "SMART_RETRIEVE_OMITTED_NEEDLE",
                    "payload": "x".repeat(140),
                })
            } else if idx == 129 {
                serde_json::json!({
                    "id": idx,
                    "level": "info",
                    "latency_ms": 50_000,
                    "payload": "x".repeat(140),
                })
            } else {
                serde_json::json!({
                    "id": idx,
                    "level": if idx % 17 == 0 { "warn" } else { "info" },
                    "message": format!("seq {idx}"),
                    "latency_ms": idx,
                    "payload": "x".repeat(140),
                })
            }
        })
        .collect::<Vec<_>>();

    HeadroomFixture {
        name: "headroom-smart-crusher-json",
        transform: "smart_crusher",
        tool_name: "json_search",
        input: HeadroomFixtureInput::SmartCrusher {
            content: serde_json::to_string(&items).expect("json"),
            query: "",
            bias: 1.0,
        },
        output_compressed: r#"[{"id":51,"level":"warn","message":"SMART_WARN_NEEDLE"},{"id":87,"rare_key":"SMART_RARE_NEEDLE"},{"id":129,"latency_ms":50000},{"_ccr_dropped":"<<ccr:fixture 177_rows_offloaded>>"}]"#,
        expected_strategy: InputSlimmingStrategy::JsonArraySample,
        semantic_needles: vec!["SMART_WARN_NEEDLE", "SMART_RARE_NEEDLE", "50000"],
        retrieval_needles: vec!["SMART_RETRIEVE_OMITTED_NEEDLE"],
    }
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

fn headroom_log_compressor_fixture() -> HeadroomFixture {
    let mut lines = (0..720)
        .map(|idx| format!("running test noise_{idx}"))
        .collect::<Vec<_>>();
    lines.splice(
        210..210,
        std::iter::repeat_n(
            "warning: duplicate warning at /tmp/build/output-123".to_string(),
            35,
        ),
    );
    lines[360] = "LOG_RETRIEVE_OMITTED_NEEDLE ordinary progress line".to_string();
    lines.splice(
        480..480,
        [
            "error[E0425]: cannot find value `HEADROOM_LOG_ERROR_NEEDLE` in this scope",
            "   --> src/lib.rs:10:5",
            "stack backtrace:",
            "   0: lha::HEADROOM_LOG_STACK_NEEDLE",
            "test result: FAILED. 1 passed; 1 failed",
            "exit code: 101",
        ]
        .into_iter()
        .map(str::to_string),
    );

    HeadroomFixture {
        name: "headroom-log-compressor-cargo",
        transform: "log_compressor",
        tool_name: "cargo test",
        input: HeadroomFixtureInput::Text(lines.join("\n")),
        output_compressed: "error[E0425]: cannot find value `HEADROOM_LOG_ERROR_NEEDLE` in this scope\nstack backtrace:\n   0: lha::HEADROOM_LOG_STACK_NEEDLE\ntest result: FAILED. 1 passed; 1 failed\nexit code: 101",
        expected_strategy: InputSlimmingStrategy::LogCompact,
        semantic_needles: vec![
            "HEADROOM_LOG_ERROR_NEEDLE",
            "HEADROOM_LOG_STACK_NEEDLE",
            "test result: FAILED",
            "exit code: 101",
        ],
        retrieval_needles: vec!["LOG_RETRIEVE_OMITTED_NEEDLE"],
    }
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

fn headroom_diff_compressor_fixture() -> HeadroomFixture {
    let mut text = String::new();
    text.push_str("diff --git a/src/auth.rs b/src/auth.rs\n");
    text.push_str("index 1..2 100644\n--- a/src/auth.rs\n+++ b/src/auth.rs\n");
    text.push_str("@@ -1,2600 +1,2600 @@\n");
    for idx in 0..2_600 {
        if idx == 1_050 {
            text.push_str("+ordinary changed line HEADROOM_DIFF_RETRIEVE_OMITTED_NEEDLE\n");
        } else if idx == 1_400 {
            text.push_str(
                "+fn headroom_diff_security_token_check() { panic!(\"HEADROOM_DIFF_CRITICAL_NEEDLE\") }\n",
            );
        } else {
            text.push_str(&format!("+ordinary changed line {idx}\n"));
        }
    }
    text.push_str("diff --git a/src/main.rs b/src/main.rs\n");
    text.push_str("--- a/src/main.rs\n+++ b/src/main.rs\n");
    text.push_str("@@ -1,3 +1,3 @@\n-old\n+new\n");

    HeadroomFixture {
        name: "headroom-diff-compressor-multifile",
        transform: "diff_compressor",
        tool_name: "diff",
        input: HeadroomFixtureInput::Text(text),
        output_compressed: "diff --git a/src/auth.rs b/src/auth.rs\n--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -1,2600 +1,2600 @@\n+fn headroom_diff_security_token_check() { panic!(\"HEADROOM_DIFF_CRITICAL_NEEDLE\") }\ndiff --git a/src/main.rs b/src/main.rs",
        expected_strategy: InputSlimmingStrategy::DiffCompact,
        semantic_needles: vec![
            "diff --git a/src/auth.rs b/src/auth.rs",
            "--- a/src/auth.rs",
            "+++ b/src/auth.rs",
            "@@ -1,2600 +1,2600 @@",
            "HEADROOM_DIFF_CRITICAL_NEEDLE",
            "diff --git a/src/main.rs b/src/main.rs",
        ],
        retrieval_needles: vec!["HEADROOM_DIFF_RETRIEVE_OMITTED_NEEDLE"],
    }
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
            expected_strategy: InputSlimmingStrategy::JsonArraySample,
            required_compressed_needles: vec!["JSON_ERROR_NEEDLE", "JSON_RARE_NEEDLE", "50000"],
            required_retrieval_needles: vec!["JSON_ERROR_NEEDLE"],
        },
        headroom_smart_crusher_fixture().into_eval_case(),
        SlimmingEvalCase {
            name: "cargo-log",
            tool_name: "cargo test",
            input: cargo_log_eval_input(),
            expected_strategy: InputSlimmingStrategy::LogCompact,
            required_compressed_needles: vec!["STACK_TRACE_NEEDLE", "test result: FAILED"],
            required_retrieval_needles: vec!["STACK_TRACE_NEEDLE"],
        },
        headroom_log_compressor_fixture().into_eval_case(),
        SlimmingEvalCase {
            name: "search",
            tool_name: "rg",
            input: search_eval_input(),
            expected_strategy: InputSlimmingStrategy::SearchResultCompact,
            required_compressed_needles: vec!["SEARCH_WINDOWS_NEEDLE", "SEARCH_DASH_NEEDLE"],
            required_retrieval_needles: vec!["SEARCH_DASH_NEEDLE"],
        },
        SlimmingEvalCase {
            name: "diff",
            tool_name: "diff",
            input: diff_eval_input(),
            expected_strategy: InputSlimmingStrategy::DiffCompact,
            required_compressed_needles: vec!["DIFF_CRITICAL_NEEDLE", "diff --git a/b.rs b/b.rs"],
            required_retrieval_needles: vec!["DIFF_CRITICAL_NEEDLE"],
        },
        headroom_diff_compressor_fixture().into_eval_case(),
        SlimmingEvalCase {
            name: "retrieval",
            tool_name: "shell",
            input: omitted_needle_input(),
            expected_strategy: InputSlimmingStrategy::PlainTextHeadTail,
            required_compressed_needles: vec!["plain_text_head_tail"],
            required_retrieval_needles: vec!["OMITTED_RETRIEVAL_NEEDLE"],
        },
    ];

    let mut strategies = Vec::new();
    for case in cases {
        let expected_strategy = case.expected_strategy;
        let result = run_slimming_eval_case(case).await;
        assert!(result.tokens_saved > 0);
        assert!(result.retrieval_successes > 0);
        assert!(
            result
                .compressed_text
                .contains(INPUT_SLIMMING_MARKER_PREFIX)
        );
        assert_eq!(result.strategy, expected_strategy);
        strategies.push(result.strategy);
    }

    assert_eq!(
        strategies,
        vec![
            InputSlimmingStrategy::JsonArraySample,
            InputSlimmingStrategy::JsonArraySample,
            InputSlimmingStrategy::LogCompact,
            InputSlimmingStrategy::LogCompact,
            InputSlimmingStrategy::SearchResultCompact,
            InputSlimmingStrategy::DiffCompact,
            InputSlimmingStrategy::DiffCompact,
            InputSlimmingStrategy::PlainTextHeadTail,
        ]
    );
}
