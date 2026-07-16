use crate::api::common::CompletedResponse;
use crate::api::common::ResponseEvent;
use crate::api::common::ResponseStream;
use crate::api::proposed_plan_parser::extract_proposed_plan_text;
use crate::types::ContentItem;
use crate::types::TranscriptItem;
use tokio::sync::mpsc;

pub(crate) fn completed_response_stream(
    completion: CompletedResponse,
    mut prefix_events: Vec<ResponseEvent>,
) -> ResponseStream {
    prefix_events.push(ResponseEvent::Created);

    for item in completion.output {
        let emits_lifecycle = is_semantic_output_item(&item);
        if emits_lifecycle {
            prefix_events.push(ResponseEvent::OutputItemAdded(item.clone()));
        }
        if let Some(plan_text) = proposed_plan_text(&item) {
            prefix_events.push(ResponseEvent::ProposedPlanDone(plan_text));
        }
        prefix_events.push(ResponseEvent::OutputItemDone(item));
    }

    prefix_events.push(ResponseEvent::Completed {
        response_id: completion.response_id,
        token_usage: completion.token_usage,
    });

    let (tx_event, rx_event) = mpsc::channel(16);
    tokio::spawn(async move {
        for event in prefix_events {
            if tx_event.send(Ok(event)).await.is_err() {
                return;
            }
        }
    });

    ResponseStream { rx_event }
}

fn is_semantic_output_item(item: &TranscriptItem) -> bool {
    match item {
        TranscriptItem::Message { role, .. } => role == "assistant",
        TranscriptItem::Reasoning { .. } | TranscriptItem::HostedActivity { .. } => true,
        TranscriptItem::ToolCall { .. }
        | TranscriptItem::ToolResult { .. }
        | TranscriptItem::Unknown { .. } => false,
    }
}

fn proposed_plan_text(item: &TranscriptItem) -> Option<String> {
    let TranscriptItem::Message { role, content, .. } = item else {
        return None;
    };
    if role != "assistant" {
        return None;
    }

    let text = content.iter().fold(String::new(), |mut text, item| {
        if let ContentItem::OutputText { text: chunk } = item {
            text.push_str(chunk);
        }
        text
    });
    extract_proposed_plan_text(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn completed_response_emits_only_terminal_message_events() {
        let response = CompletedResponse {
            response_id: "resp-1".to_string(),
            output: vec![TranscriptItem::Message {
                id: Some("msg-1".to_string()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Hello".to_string(),
                }],
                end_turn: None,
            }],
            token_usage: None,
        };

        let events = completed_response_stream(response, Vec::new())
            .collect::<Vec<_>>()
            .await;

        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], Ok(ResponseEvent::Created)));
        assert!(matches!(
            events[1],
            Ok(ResponseEvent::OutputItemAdded(
                TranscriptItem::Message { .. }
            ))
        ));
        assert!(matches!(
            events[2],
            Ok(ResponseEvent::OutputItemDone(
                TranscriptItem::Message { .. }
            ))
        ));
        assert!(matches!(
            &events[3],
            Ok(ResponseEvent::Completed {
                response_id,
                token_usage: None,
            }) if response_id == "resp-1"
        ));
    }

    #[tokio::test]
    async fn completed_response_emits_only_proposed_plan_done() {
        let response = CompletedResponse {
            response_id: "resp-1".to_string(),
            output: vec![TranscriptItem::Message {
                id: Some("msg-1".to_string()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "Intro\n<proposed_plan>\n- Step 1\n</proposed_plan>\nOutro".to_string(),
                }],
                end_turn: None,
            }],
            token_usage: None,
        };

        let events = completed_response_stream(response, Vec::new())
            .collect::<Vec<_>>()
            .await;

        assert!(events.iter().any(|event| {
            matches!(
                event,
                Ok(ResponseEvent::ProposedPlanDone(text)) if text == "- Step 1\n"
            )
        }));
        assert!(!events.iter().any(|event| {
            matches!(
                event,
                Ok(ResponseEvent::OutputTextDelta(_)
                    | ResponseEvent::ReasoningContentDelta { .. }
                    | ResponseEvent::ReasoningSummaryDelta { .. }
                    | ResponseEvent::ProposedPlanDelta(_))
            )
        }));
    }
}
