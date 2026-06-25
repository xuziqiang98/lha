use std::collections::BTreeMap;

use async_trait::async_trait;
use lha_llm::AdditionalProperties;
use lha_llm::FunctionToolDescriptor;
use lha_llm::ToolDescriptor;
use lha_llm::ToolInputSchema;
use serde::Deserialize;

use crate::product::agent::codex::Session;
use crate::product::agent::function_tool::FunctionCallError;
use crate::product::agent::input_slimming::RetrieveResult;
use crate::product::agent::tools::context::ToolInvocation;
use crate::product::agent::tools::context::ToolOutput;
use crate::product::agent::tools::context::ToolPayload;
use crate::product::agent::tools::registry::ToolHandler;
use crate::product::agent::tools::registry::ToolKind;
use crate::product::otel::OtelManager;

pub(crate) const INPUT_RETRIEVE_TOOL_NAME: &str = "lha_input_retrieve";

pub(crate) struct InputRetrieveHandler;

#[derive(Deserialize)]
struct InputRetrieveArgs {
    hash: String,
    query: Option<String>,
}

pub(crate) async fn retrieve_input_slimming_for_tool(
    session: &Session,
    hash: &str,
    query: Option<&str>,
) -> RetrieveResult {
    let mut result = session
        .services
        .input_slimming_store
        .retrieve(hash, query)
        .await;
    if !result.success
        && session
            .rehydrate_input_slimming_hash_from_rollout(hash)
            .await
    {
        result = session
            .services
            .input_slimming_store
            .retrieve(hash, query)
            .await;
    }
    result
}

pub(crate) fn record_input_slimming_retrieve_metrics(otel: &OtelManager, result: &RetrieveResult) {
    let strategy = result
        .strategy
        .map(super::InputSlimmingStrategy::as_str)
        .unwrap_or("unknown");
    let tool_name = result.tool_name.as_deref().unwrap_or("unknown");
    otel.counter(
        "lha.input_slimming.retrieve",
        1,
        &[
            ("success", if result.success { "true" } else { "false" }),
            ("strategy", strategy),
            ("tool_name", tool_name),
        ],
    );
    if !result.success {
        otel.counter("lha.input_slimming.retrieve_miss", 1, &[]);
    }
    if let Some(matched) = result.query_matched {
        otel.counter(
            "lha.input_slimming.retrieve_query",
            1,
            &[("matched", if matched { "true" } else { "false" })],
        );
    }
}

#[async_trait]
impl ToolHandler for InputRetrieveHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            ToolPayload::Custom { .. }
            | ToolPayload::LocalShell { .. }
            | ToolPayload::Mcp { .. } => {
                return Err(FunctionCallError::RespondToModel(
                    "lha_input_retrieve received unsupported payload".to_string(),
                ));
            }
        };
        let args: InputRetrieveArgs = serde_json::from_str(&arguments).map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to parse lha_input_retrieve arguments: {err}"
            ))
        })?;

        let result = retrieve_input_slimming_for_tool(
            invocation.session.as_ref(),
            args.hash.as_str(),
            args.query.as_deref(),
        )
        .await;

        let otel = invocation.turn.runtime.get_otel_manager();
        record_input_slimming_retrieve_metrics(&otel, &result);

        Ok(ToolOutput::Function {
            content: result.content,
            content_items: None,
            success: Some(result.success),
        })
    }
}

pub(crate) fn create_lha_input_retrieve_tool() -> ToolDescriptor {
    let mut properties = BTreeMap::new();
    properties.insert(
        "hash".to_string(),
        ToolInputSchema::String {
            description: Some(
                "The 24-character hash from an <<lha-input:...>> marker.".to_string(),
            ),
            enum_values: None,
        },
    );
    properties.insert(
        "query".to_string(),
        ToolInputSchema::String {
            description: Some(
                "Optional text to search for within the stored original payload.".to_string(),
            ),
            enum_values: None,
        },
    );

    ToolDescriptor::Function(FunctionToolDescriptor {
        name: INPUT_RETRIEVE_TOOL_NAME.to_string(),
        description: "Retrieve the original tool output behind an Input Slimming marker. Use this when a compressed snippet with <<lha-input:...>> does not contain enough detail; pass an optional query to retrieve only relevant lines.".to_string(),
        strict: false,
        parameters: ToolInputSchema::Object {
            properties,
            required: Some(vec!["hash".to_string()]),
            additional_properties: Some(AdditionalProperties::Boolean(false)),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::agent::codex::make_session_and_context;
    use crate::product::agent::input_slimming::InputSlimmingStrategy;
    use crate::product::agent::input_slimming::StoredInputMetadata;
    use crate::product::agent::tools::context::ToolPayload;
    use crate::product::agent::turn_diff_tracker::TurnDiffTracker;
    use crate::product::otel::OtelManager;
    use crate::product::otel::metrics::MetricsClient;
    use crate::product::otel::metrics::MetricsConfig;
    use crate::product::otel::metrics::Result as MetricsResult;
    use crate::product::protocol::ThreadId;
    use crate::product::protocol::protocol::SessionSource;
    use opentelemetry_sdk::metrics::InMemoryMetricExporter;
    use opentelemetry_sdk::metrics::data::AggregatedMetrics;
    use opentelemetry_sdk::metrics::data::Metric;
    use opentelemetry_sdk::metrics::data::MetricData;
    use opentelemetry_sdk::metrics::data::ResourceMetrics;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[test]
    fn tool_spec_shape_is_pinned() {
        let ToolDescriptor::Function(tool) = create_lha_input_retrieve_tool() else {
            panic!("expected function tool");
        };

        assert_eq!(tool.name, INPUT_RETRIEVE_TOOL_NAME);
        assert!(!tool.strict);
        let ToolInputSchema::Object {
            properties,
            required,
            additional_properties,
        } = tool.parameters
        else {
            panic!("expected object schema");
        };
        assert!(properties.contains_key("hash"));
        assert!(properties.contains_key("query"));
        assert_eq!(required, Some(vec!["hash".to_string()]));
        assert_eq!(
            additional_properties,
            Some(AdditionalProperties::Boolean(false))
        );
    }

    #[test]
    fn record_retrieve_metrics_records_query_miss_label() -> MetricsResult<()> {
        let exporter = InMemoryMetricExporter::default();
        let config =
            MetricsConfig::in_memory("test", "lha-cli", env!("CARGO_PKG_VERSION"), exporter)
                .with_runtime_reader();
        let metrics = MetricsClient::new(config)?;
        let manager = OtelManager::new(
            ThreadId::new(),
            "gpt-5.1",
            "gpt-5.1",
            None,
            None,
            None,
            false,
            "test".to_string(),
            SessionSource::Cli,
        )
        .with_metrics_without_metadata_tags(metrics);
        let result = RetrieveResult {
            content: "no matching excerpt".to_string(),
            success: true,
            strategy: Some(InputSlimmingStrategy::PlainTextHeadTail),
            tool_name: Some("shell".to_string()),
            query_matched: Some(false),
        };

        record_input_slimming_retrieve_metrics(&manager, &result);

        let snapshot = manager.snapshot_metrics()?;
        assert_eq!(
            counter_attributes(&snapshot, "lha.input_slimming.retrieve"),
            BTreeMap::from([
                ("strategy".to_string(), "plain_text_head_tail".to_string()),
                ("success".to_string(), "true".to_string()),
                ("tool_name".to_string(), "shell".to_string()),
            ])
        );
        assert_eq!(
            counter_attributes(&snapshot, "lha.input_slimming.retrieve_query"),
            BTreeMap::from([("matched".to_string(), "false".to_string())])
        );
        assert!(find_metric(&snapshot, "lha.input_slimming.retrieve_miss").is_none());

        Ok(())
    }

    #[tokio::test]
    async fn handler_retrieves_original_payload() {
        let (session, turn_context) = make_session_and_context().await;
        let hash = session
            .services
            .input_slimming_store
            .put(
                "original payload".to_string(),
                StoredInputMetadata {
                    scope:
                        crate::product::agent::protocol::InputSlimmingScope::HistoricalToolOutputs,
                    strategy: InputSlimmingStrategy::PlainTextHeadTail,
                    tool_name: "shell".to_string(),
                    original_tokens: 3,
                    compressed_tokens: 1,
                    created_turn_id: "turn-1".to_string(),
                },
            )
            .await;
        let invocation = ToolInvocation {
            session: Arc::new(session),
            turn: Arc::new(turn_context),
            tracker: Arc::new(Mutex::new(TurnDiffTracker::new())),
            call_id: "call".to_string(),
            tool_name: INPUT_RETRIEVE_TOOL_NAME.to_string(),
            payload: ToolPayload::Function {
                arguments: format!(r#"{{"hash":"{hash}"}}"#),
            },
        };

        let output = InputRetrieveHandler
            .handle(invocation)
            .await
            .expect("tool succeeds");

        match output {
            ToolOutput::Function {
                content,
                content_items,
                success,
            } => {
                assert!(content.contains("original payload"));
                assert_eq!(content_items, None);
                assert_eq!(success, Some(true));
            }
        }
    }

    #[tokio::test]
    async fn handler_reports_missing_hash_as_failure() {
        let (session, turn_context) = make_session_and_context().await;
        let invocation = ToolInvocation {
            session: Arc::new(session),
            turn: Arc::new(turn_context),
            tracker: Arc::new(Mutex::new(TurnDiffTracker::new())),
            call_id: "call".to_string(),
            tool_name: INPUT_RETRIEVE_TOOL_NAME.to_string(),
            payload: ToolPayload::Function {
                arguments: r#"{"hash":"missing"}"#.to_string(),
            },
        };

        let output = InputRetrieveHandler
            .handle(invocation)
            .await
            .expect("tool succeeds");

        match output {
            ToolOutput::Function {
                content,
                content_items,
                success,
            } => {
                assert!(content.contains("store miss"));
                assert_eq!(content_items, None);
                assert_eq!(success, Some(false));
            }
        }
    }

    fn find_metric<'a>(snapshot: &'a ResourceMetrics, name: &str) -> Option<&'a Metric> {
        snapshot
            .scope_metrics()
            .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
            .find(|metric| metric.name() == name)
    }

    fn counter_attributes(snapshot: &ResourceMetrics, name: &str) -> BTreeMap<String, String> {
        let metric = find_metric(snapshot, name).expect("counter metric missing");
        match metric.data() {
            AggregatedMetrics::U64(data) => match data {
                MetricData::Sum(sum) => {
                    let points: Vec<_> = sum.data_points().collect();
                    assert_eq!(points.len(), 1);
                    points[0]
                        .attributes()
                        .map(|kv| (kv.key.as_str().to_string(), kv.value.as_str().to_string()))
                        .collect()
                }
                _ => panic!("unexpected counter aggregation for {name}"),
            },
            _ => panic!("unexpected counter data type for {name}"),
        }
    }
}
