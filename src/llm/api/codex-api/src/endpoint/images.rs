use crate::auth::AuthProvider;
use crate::auth::add_auth_headers;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::telemetry::run_with_request_telemetry;
use http::HeaderMap;
use http::Method;
use lha_client::HttpTransport;
use lha_client::RequestTelemetry;
use serde::Deserialize;
use serde::Serialize;
use std::sync::Arc;

pub struct ImagesClient<T: HttpTransport, A: AuthProvider> {
    transport: T,
    provider: Provider,
    auth: A,
    request_telemetry: Option<Arc<dyn RequestTelemetry>>,
}

impl<T: HttpTransport, A: AuthProvider> ImagesClient<T, A> {
    pub fn new(transport: T, provider: Provider, auth: A) -> Self {
        Self {
            transport,
            provider,
            auth,
            request_telemetry: None,
        }
    }

    pub fn with_telemetry(mut self, request: Option<Arc<dyn RequestTelemetry>>) -> Self {
        self.request_telemetry = request;
        self
    }

    pub async fn generate(
        &self,
        request: &ImageGenerationRequest,
        extra_headers: HeaderMap,
    ) -> Result<ImageResponse, ApiError> {
        self.post_image_request(
            "images/generations",
            request,
            extra_headers,
            "image generation",
        )
        .await
    }

    pub async fn edit(
        &self,
        request: &ImageEditRequest,
        extra_headers: HeaderMap,
    ) -> Result<ImageResponse, ApiError> {
        self.post_image_request("images/edits", request, extra_headers, "image edit")
            .await
    }

    async fn post_image_request<R: Serialize>(
        &self,
        path: &str,
        request: &R,
        extra_headers: HeaderMap,
        operation: &str,
    ) -> Result<ImageResponse, ApiError> {
        let builder = || {
            let mut req = self.provider.build_request(Method::POST, path);
            req.headers.extend(extra_headers.clone());
            req = req.with_json(request);
            add_auth_headers(&self.auth, self.provider.wire.clone(), req)
        };

        let resp = run_with_request_telemetry(
            self.provider.retry.to_policy(),
            self.request_telemetry.clone(),
            builder,
            |req| self.transport.execute(req),
        )
        .await?;

        serde_json::from_slice(&resp.body).map_err(|err| {
            ApiError::Stream(format!("failed to decode {operation} response: {err}"))
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ImageGenerationRequest {
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<ImageBackground>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<ImageQuality>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ImageEditRequest {
    pub images: Vec<ImageUrl>,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<ImageBackground>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<ImageQuality>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ImageUrl {
    pub image_url: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageBackground {
    Auto,
    Opaque,
    Transparent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageQuality {
    Auto,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ImageResponse {
    pub created: u64,
    #[serde(default)]
    pub background: Option<ImageBackground>,
    pub data: Vec<ImageData>,
    #[serde(default)]
    pub quality: Option<ImageQuality>,
    #[serde(default)]
    pub size: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ImageData {
    pub b64_json: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::RetryConfig;
    use crate::provider::WireApi;
    use async_trait::async_trait;
    use http::StatusCode;
    use lha_client::Request;
    use lha_client::Response;
    use lha_client::StreamResponse;
    use lha_client::TransportError;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Clone, Default)]
    struct DummyAuth;

    impl AuthProvider for DummyAuth {
        fn bearer_token(&self) -> Option<String> {
            None
        }
    }

    #[derive(Clone)]
    struct CapturingTransport {
        last_request: Arc<Mutex<Option<Request>>>,
        response_body: Arc<Vec<u8>>,
    }

    impl CapturingTransport {
        fn new(response_body: Vec<u8>) -> Self {
            Self {
                last_request: Arc::new(Mutex::new(None)),
                response_body: Arc::new(response_body),
            }
        }
    }

    #[async_trait]
    impl HttpTransport for CapturingTransport {
        async fn execute(&self, req: Request) -> Result<Response, TransportError> {
            *self.last_request.lock().expect("lock request store") = Some(req);
            Ok(Response {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                body: self.response_body.as_ref().clone().into(),
            })
        }

        async fn stream(&self, _req: Request) -> Result<StreamResponse, TransportError> {
            Err(TransportError::Build("stream should not run".to_string()))
        }
    }

    fn provider() -> Provider {
        Provider {
            name: "test".to_string(),
            base_url: "https://example.com/api/codex".to_string(),
            query_params: None,
            wire: WireApi::Responses,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: false,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_idle_timeout: Duration::from_secs(1),
        }
    }

    fn response_body() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "created": 1778832973u64,
            "background": "opaque",
            "data": [{"b64_json": "REDACT"}],
            "quality": "medium",
            "size": "1024x1536"
        }))
        .expect("serialize response")
    }

    fn expected_response() -> ImageResponse {
        ImageResponse {
            created: 1778832973,
            background: Some(ImageBackground::Opaque),
            data: vec![ImageData {
                b64_json: "REDACT".to_string(),
            }],
            quality: Some(ImageQuality::Medium),
            size: Some("1024x1536".to_string()),
        }
    }

    fn captured_request(transport: &CapturingTransport) -> Request {
        transport
            .last_request
            .lock()
            .expect("lock request store")
            .clone()
            .expect("request should be captured")
    }

    #[tokio::test]
    async fn generate_posts_typed_request_and_parses_image_response() {
        let transport = CapturingTransport::new(response_body());
        let client = ImagesClient::new(transport.clone(), provider(), DummyAuth);

        let response = client
            .generate(
                &ImageGenerationRequest {
                    prompt: "a red fox in a field".to_string(),
                    background: Some(ImageBackground::Opaque),
                    model: "gpt-image-2".to_string(),
                    n: None,
                    quality: Some(ImageQuality::Medium),
                    size: Some("1024x1536".to_string()),
                },
                HeaderMap::new(),
            )
            .await
            .expect("image generation request should succeed");

        assert_eq!(response, expected_response());

        let request = captured_request(&transport);
        assert_eq!(
            request.url,
            "https://example.com/api/codex/images/generations"
        );
        assert_eq!(
            request.body,
            Some(json!({
                "prompt": "a red fox in a field",
                "background": "opaque",
                "model": "gpt-image-2",
                "quality": "medium",
                "size": "1024x1536",
            }))
        );
    }

    #[tokio::test]
    async fn edit_posts_typed_request_and_omits_none_fields() {
        let transport = CapturingTransport::new(response_body());
        let client = ImagesClient::new(transport.clone(), provider(), DummyAuth);

        let response = client
            .edit(
                &ImageEditRequest {
                    images: vec![ImageUrl {
                        image_url: "data:image/png;base64,Zm9v".to_string(),
                    }],
                    prompt: "add a red hat".to_string(),
                    background: None,
                    model: "gpt-image-2".to_string(),
                    n: None,
                    quality: None,
                    size: None,
                },
                HeaderMap::new(),
            )
            .await
            .expect("image edit request should succeed");

        assert_eq!(response, expected_response());

        let request = captured_request(&transport);
        assert_eq!(request.url, "https://example.com/api/codex/images/edits");
        assert_eq!(
            request.body,
            Some(json!({
                "images": [{"image_url": "data:image/png;base64,Zm9v"}],
                "prompt": "add a red hat",
                "model": "gpt-image-2",
            }))
        );
    }

    #[tokio::test]
    async fn image_response_requires_image_data() {
        let transport = CapturingTransport::new(
            serde_json::to_vec(&json!({"created": 1778832973u64})).expect("serialize response"),
        );
        let client = ImagesClient::new(transport, provider(), DummyAuth);

        let error = client
            .generate(
                &ImageGenerationRequest {
                    prompt: "a red fox in a field".to_string(),
                    background: None,
                    model: "gpt-image-2".to_string(),
                    n: None,
                    quality: None,
                    size: None,
                },
                HeaderMap::new(),
            )
            .await
            .expect_err("image response without data should fail");

        let ApiError::Stream(message) = error else {
            panic!("expected image response decode error");
        };
        assert!(
            message.starts_with("failed to decode image generation response: missing field `data`"),
            "{message}"
        );
    }
}
