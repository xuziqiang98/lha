use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use crate::product::protocol::models::ContentItem;
use crate::product::protocol::models::TranscriptItem;
use async_trait::async_trait;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use lha_llm::RuntimeEndpoint;
use lha_llm::ToolResultContentItem;
use lha_llm::ToolResultPayload;
use lha_llm::api::AuthProvider;
use lha_llm::api::ImageBackground;
use lha_llm::api::ImageEditRequest;
use lha_llm::api::ImageGenerationRequest;
use lha_llm::api::ImageQuality;
use lha_llm::api::ImageUrl;
use lha_llm::api::ImagesClient;
use lha_llm::api::Provider;
use lha_llm::api::ReqwestTransport;
use lha_llm::api::WireApi;
use serde::Deserialize;
use tokio::fs;

use crate::product::agent::default_client::build_reqwest_client;
use crate::product::agent::function_tool::FunctionCallError;
use crate::product::agent::tools::context::ToolInvocation;
use crate::product::agent::tools::context::ToolOutput;
use crate::product::agent::tools::context::ToolPayload;
use crate::product::agent::tools::handlers::parse_arguments;
use crate::product::agent::tools::registry::ToolHandler;
use crate::product::agent::tools::registry::ToolKind;

const IMAGE_MODEL: &str = "gpt-image-2";
const MAX_EDIT_IMAGES: usize = 5;

pub struct ImagegenHandler;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImagegenArgs {
    prompt: String,
    action: ImagegenAction,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ImagegenAction {
    Generate,
    Edit,
}

#[derive(Clone, Debug)]
struct ImagegenAuthProvider {
    token: Option<String>,
}

impl AuthProvider for ImagegenAuthProvider {
    fn bearer_token(&self) -> Option<String> {
        self.token.clone()
    }
}

#[derive(Clone, Debug, PartialEq)]
enum ImageRequest {
    Generate(ImageGenerationRequest),
    Edit(ImageEditRequest),
}

#[async_trait]
impl ToolHandler for ImagegenHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "imagegen handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: ImagegenArgs = parse_arguments(&arguments)?;
        let endpoint = turn.runtime.endpoint();
        let api_provider = image_api_provider(&endpoint)?;
        let api_auth = image_api_auth(&endpoint)?;
        let history = session.clone_history().await.for_prompt();
        let request = request_for_action(&args, &history)?;

        let client = ImagesClient::new(
            ReqwestTransport::new(build_reqwest_client()),
            api_provider,
            api_auth,
        );
        let response = match request {
            ImageRequest::Generate(request) => client.generate(&request, HeaderMap::new()).await,
            ImageRequest::Edit(request) => client.edit(&request, HeaderMap::new()).await,
        }
        .map_err(|err| FunctionCallError::RespondToModel(format!("image request failed: {err}")))?;

        let Some(result) = response.data.into_iter().next().map(|data| data.b64_json) else {
            return Err(FunctionCallError::RespondToModel(
                "image generation returned no image data".to_string(),
            ));
        };

        output_for_generated_image(
            &turn.runtime.config().lha_home,
            session.conversation_id.to_string().as_str(),
            &call_id,
            &result,
        )
        .await
    }
}

fn request_for_action(
    args: &ImagegenArgs,
    history: &[TranscriptItem],
) -> Result<ImageRequest, FunctionCallError> {
    match args.action {
        ImagegenAction::Generate => Ok(ImageRequest::Generate(ImageGenerationRequest {
            prompt: args.prompt.clone(),
            background: Some(ImageBackground::Auto),
            model: IMAGE_MODEL.to_string(),
            n: None,
            quality: Some(ImageQuality::Auto),
            size: Some("auto".to_string()),
        })),
        ImagegenAction::Edit => {
            let images = edit_images(history);
            if images.is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "image edit requested without any usable image in conversation history"
                        .to_string(),
                ));
            }
            Ok(ImageRequest::Edit(ImageEditRequest {
                images,
                prompt: args.prompt.clone(),
                background: Some(ImageBackground::Auto),
                model: IMAGE_MODEL.to_string(),
                n: None,
                quality: Some(ImageQuality::Auto),
                size: Some("auto".to_string()),
            }))
        }
    }
}

fn edit_images(history: &[TranscriptItem]) -> Vec<ImageUrl> {
    let latest_uploaded_images = history.iter().enumerate().rev().find_map(|(index, item)| {
        let TranscriptItem::Message { role, content, .. } = item else {
            return None;
        };
        if role != "user" {
            return None;
        }
        let images = content
            .iter()
            .filter_map(|item| match item {
                ContentItem::InputImage { image_url } => Some(ImageUrl {
                    image_url: image_url.clone(),
                }),
                ContentItem::InputText { .. } | ContentItem::OutputText { .. } => None,
            })
            .collect::<Vec<_>>();
        (!images.is_empty()).then_some((index, images))
    });

    let (user_images, follow_up_start) = latest_uploaded_images
        .map_or_else(|| (Vec::new(), 0), |(index, images)| (images, index + 1));
    let mut generated_images = Vec::new();
    let mut imagegen_calls = HashMap::new();
    for item in &history[follow_up_start..] {
        match item {
            TranscriptItem::ToolCall {
                call_id, tool_name, ..
            } if tool_name == "imagegen" => {
                imagegen_calls.insert(call_id.clone(), ());
            }
            TranscriptItem::ToolResult {
                call_id, payload, ..
            } if imagegen_calls.contains_key(call_id) => {
                generated_images.extend(image_urls_from_tool_result(payload));
            }
            TranscriptItem::Message { .. }
            | TranscriptItem::Reasoning { .. }
            | TranscriptItem::HostedActivity { .. }
            | TranscriptItem::ToolCall { .. }
            | TranscriptItem::ToolResult { .. }
            | TranscriptItem::Unknown { .. } => {}
        }
    }

    truncate_images(user_images, generated_images)
}

fn image_urls_from_tool_result(payload: &ToolResultPayload) -> Vec<ImageUrl> {
    match payload {
        ToolResultPayload::Structured {
            content_items: Some(items),
            ..
        } => items
            .iter()
            .filter_map(|item| match item {
                ToolResultContentItem::InputImage { image_url } => Some(ImageUrl {
                    image_url: image_url.clone(),
                }),
                ToolResultContentItem::InputText { .. } => None,
            })
            .collect(),
        ToolResultPayload::Structured { .. } | ToolResultPayload::Text { .. } => Vec::new(),
    }
}

fn truncate_images(
    mut user_images: Vec<ImageUrl>,
    mut generated_images: Vec<ImageUrl>,
) -> Vec<ImageUrl> {
    let mut excess = (user_images.len() + generated_images.len()).saturating_sub(MAX_EDIT_IMAGES);
    let drop_generated = excess.min(generated_images.len().saturating_sub(1));
    generated_images.drain(..drop_generated);
    excess -= drop_generated;
    let drop_user = excess.min(user_images.len());
    user_images.drain(..drop_user);
    excess -= drop_user;
    generated_images.drain(..excess);

    user_images.extend(generated_images);
    user_images
}

fn image_api_provider(endpoint: &RuntimeEndpoint) -> Result<Provider, FunctionCallError> {
    if !endpoint.is_openai() || !matches_image_wire(endpoint) {
        return Err(FunctionCallError::RespondToModel(
            "image generation requires the OpenAI Responses or Chat provider".to_string(),
        ));
    }

    let default_base_url = "https://api.openai.com/v1";
    let base_url = endpoint
        .base_url
        .clone()
        .unwrap_or_else(|| default_base_url.to_string());

    Ok(Provider {
        name: endpoint.name.clone(),
        base_url,
        query_params: endpoint.query_params.clone(),
        wire: if endpoint.uses_responses_api() {
            WireApi::Responses
        } else {
            WireApi::Chat
        },
        headers: build_header_map(endpoint),
        retry: lha_llm::api::provider::RetryConfig {
            max_attempts: endpoint.request_max_retries(),
            base_delay: Duration::from_millis(200),
            retry_429: false,
            retry_5xx: true,
            retry_transport: true,
        },
        stream_idle_timeout: endpoint.stream_idle_timeout(),
    })
}

fn matches_image_wire(endpoint: &RuntimeEndpoint) -> bool {
    endpoint.uses_responses_api() || endpoint.uses_chat_completions_api()
}

fn build_header_map(endpoint: &RuntimeEndpoint) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(extra) = &endpoint.http_headers {
        for (key, value) in extra {
            if let (Ok(name), Ok(value)) = (HeaderName::try_from(key), HeaderValue::try_from(value))
            {
                headers.insert(name, value);
            }
        }
    }

    if let Some(env_headers) = &endpoint.env_http_headers {
        for (header, env_var) in env_headers {
            if let Ok(value) = std::env::var(env_var)
                && !value.trim().is_empty()
                && let (Ok(name), Ok(value)) =
                    (HeaderName::try_from(header), HeaderValue::try_from(value))
            {
                headers.insert(name, value);
            }
        }
    }

    headers
}

fn image_api_auth(endpoint: &RuntimeEndpoint) -> Result<ImagegenAuthProvider, FunctionCallError> {
    let token = endpoint
        .api_key()
        .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
        .or_else(|| endpoint.bearer_token.clone())
        .filter(|token| !token.trim().is_empty());

    if token.is_none() {
        return Err(FunctionCallError::RespondToModel(
            "image generation requires a provider API key".to_string(),
        ));
    }

    Ok(ImagegenAuthProvider { token })
}

async fn save_generated_image(
    lha_home: &Path,
    conversation_id: &str,
    call_id: &str,
    result: &str,
) -> Result<PathBuf, FunctionCallError> {
    let dir = lha_home
        .join("generated_images")
        .join(sanitize_path_component(conversation_id));
    fs::create_dir_all(&dir).await.map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "failed to create generated image directory `{}`: {err}",
            dir.display()
        ))
    })?;

    let path = dir.join(format!("{}.png", sanitize_path_component(call_id)));
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, result)
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to decode generated image payload: {err}"
            ))
        })?;
    fs::write(&path, bytes).await.map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "failed to save generated image `{}`: {err}",
            path.display()
        ))
    })?;
    Ok(path)
}

async fn output_for_generated_image(
    lha_home: &Path,
    conversation_id: &str,
    call_id: &str,
    result: &str,
) -> Result<ToolOutput, FunctionCallError> {
    let saved_path = save_generated_image(lha_home, conversation_id, call_id, result).await?;
    let content = format!("Generated image saved to {}", saved_path.display());
    let image_url = format!("data:image/png;base64,{result}");

    Ok(ToolOutput::Function {
        content: content.clone(),
        content_items: Some(vec![
            ToolResultContentItem::InputText { text: content },
            ToolResultContentItem::InputImage { image_url },
        ]),
        success: Some(true),
    })
}

fn sanitize_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim_matches('.');
    if sanitized.is_empty() {
        "image".to_string()
    } else {
        sanitized.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn user_images(images: &[&str]) -> TranscriptItem {
        TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content: images
                .iter()
                .map(|image_url| ContentItem::InputImage {
                    image_url: (*image_url).to_string(),
                })
                .collect(),
            end_turn: None,
        }
    }

    fn imagegen_tool_call(call_id: &str) -> TranscriptItem {
        TranscriptItem::ToolCall {
            id: None,
            call_id: call_id.to_string(),
            tool_name: "imagegen".to_string(),
            payload: lha_llm::ToolCallPayload::JsonArguments {
                arguments: "{}".to_string(),
            },
        }
    }

    fn imagegen_tool_result(call_id: &str, images: &[&str]) -> TranscriptItem {
        TranscriptItem::ToolResult {
            call_id: call_id.to_string(),
            tool_name: "imagegen".to_string(),
            payload: ToolResultPayload::Structured {
                content: "ok".to_string(),
                content_items: Some(
                    std::iter::once(ToolResultContentItem::InputText {
                        text: "Generated image saved to /tmp/image.png".to_string(),
                    })
                    .chain(
                        images
                            .iter()
                            .map(|image_url| ToolResultContentItem::InputImage {
                                image_url: (*image_url).to_string(),
                            }),
                    )
                    .collect(),
                ),
                success: Some(true),
            },
        }
    }

    #[test]
    fn generate_request_uses_expected_defaults() {
        let args = ImagegenArgs {
            prompt: "a red fox in a field".to_string(),
            action: ImagegenAction::Generate,
        };

        let request = request_for_action(&args, &[]).expect("generate request");

        assert_eq!(
            request,
            ImageRequest::Generate(ImageGenerationRequest {
                prompt: "a red fox in a field".to_string(),
                background: Some(ImageBackground::Auto),
                model: IMAGE_MODEL.to_string(),
                n: None,
                quality: Some(ImageQuality::Auto),
                size: Some("auto".to_string()),
            })
        );
    }

    #[test]
    fn edit_request_fails_without_image_context() {
        let args = ImagegenArgs {
            prompt: "add a hat".to_string(),
            action: ImagegenAction::Edit,
        };
        let err = request_for_action(&args, &[]).expect_err("edit should require images");
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "image edit requested without any usable image in conversation history".to_string()
            )
        );
    }

    #[test]
    fn edit_images_uses_latest_user_images_and_following_generated_images() {
        let history = vec![
            user_images(&["old-user"]),
            imagegen_tool_call("old"),
            imagegen_tool_result("old", &["old-gen"]),
            user_images(&["new-user-1", "new-user-2"]),
            imagegen_tool_call("new"),
            imagegen_tool_result("new", &["new-gen"]),
        ];

        let images = edit_images(&history)
            .into_iter()
            .map(|image| image.image_url)
            .collect::<Vec<_>>();

        assert_eq!(images, vec!["new-user-1", "new-user-2", "new-gen"]);
    }

    #[test]
    fn edit_images_truncates_to_five_and_preserves_newest_generated_image() {
        let history = vec![
            user_images(&["user-1", "user-2", "user-3", "user-4"]),
            imagegen_tool_call("gen"),
            imagegen_tool_result("gen", &["gen-1", "gen-2", "gen-3"]),
        ];

        let images = edit_images(&history)
            .into_iter()
            .map(|image| image.image_url)
            .collect::<Vec<_>>();

        assert_eq!(
            images,
            vec!["user-1", "user-2", "user-3", "user-4", "gen-3"]
        );
    }

    #[tokio::test]
    async fn generated_image_is_saved_under_lha_home_and_returned_as_input_image() {
        let lha_home = tempfile::tempdir().expect("tempdir");
        let output = output_for_generated_image(lha_home.path(), "thread/1", "../call/..", "Zm9v")
            .await
            .expect("build output");
        let expected_path = lha_home
            .path()
            .join("generated_images")
            .join("thread_1")
            .join("_call_.png");

        assert_eq!(
            std::fs::read(&expected_path).expect("read saved image"),
            b"foo"
        );
        let ToolOutput::Function {
            content,
            content_items,
            success,
        } = output;
        assert_eq!(
            content,
            format!("Generated image saved to {}", expected_path.display())
        );
        assert_eq!(
            content_items,
            Some(vec![
                ToolResultContentItem::InputText {
                    text: format!("Generated image saved to {}", expected_path.display()),
                },
                ToolResultContentItem::InputImage {
                    image_url: "data:image/png;base64,Zm9v".to_string(),
                },
            ])
        );
        assert_eq!(success, Some(true));
    }
}
