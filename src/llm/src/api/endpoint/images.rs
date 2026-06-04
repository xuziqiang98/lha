use crate::api::auth::AuthProvider;
use crate::api::auth::add_auth_headers;
use crate::api::error::ApiError;
use crate::api::provider::Provider;
use crate::api::telemetry::run_with_request_telemetry;
use crate::client::HttpTransport;
use crate::client::MultipartForm;
use crate::client::RequestTelemetry;
use base64::Engine;
use http::HeaderMap;
use http::Method;
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
        let multipart = image_edit_multipart(request)?;
        self.post_multipart_image_request("images/edits", multipart, extra_headers, "image edit")
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

    async fn post_multipart_image_request(
        &self,
        path: &str,
        multipart: MultipartForm,
        extra_headers: HeaderMap,
        operation: &str,
    ) -> Result<ImageResponse, ApiError> {
        let builder = || {
            let mut req = self.provider.build_request(Method::POST, path);
            req.headers.extend(extra_headers.clone());
            req = req.with_multipart(multipart.clone());
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

fn image_edit_multipart(request: &ImageEditRequest) -> Result<MultipartForm, ApiError> {
    let mut form = MultipartForm::new();
    for (index, image) in request.images.iter().enumerate() {
        let file = decode_image_data_url(&image.image_url, index + 1)?;
        form = form.bytes("image", file.file_name, file.mime, file.bytes);
    }

    form = form.text("prompt", request.prompt.clone());
    form = form.text("model", request.model.clone());
    if let Some(background) = request.background {
        form = form.text("background", image_background_wire(background));
    }
    if let Some(n) = request.n {
        form = form.text("n", n.to_string());
    }
    if let Some(quality) = request.quality {
        form = form.text("quality", image_quality_wire(quality));
    }
    if let Some(size) = &request.size {
        form = form.text("size", size.clone());
    }

    Ok(form)
}

struct ImageEditFile {
    file_name: String,
    mime: String,
    bytes: Vec<u8>,
}

fn decode_image_data_url(image_url: &str, index: usize) -> Result<ImageEditFile, ApiError> {
    let Some(data_url) = image_url.strip_prefix("data:") else {
        return Err(image_edit_input_error(format!(
            "image {index} must be a base64 data URL"
        )));
    };
    let Some((metadata, payload)) = data_url.split_once(',') else {
        return Err(image_edit_input_error(format!(
            "image {index} data URL is missing a comma separator"
        )));
    };

    let mut metadata_parts = metadata.split(';');
    let mime = metadata_parts.next().unwrap_or_default();
    if mime.is_empty() || !mime.to_ascii_lowercase().starts_with("image/") {
        return Err(image_edit_input_error(format!(
            "image {index} data URL MIME type must be image/*"
        )));
    }
    if !metadata_parts.any(|part| part.eq_ignore_ascii_case("base64")) {
        return Err(image_edit_input_error(format!(
            "image {index} data URL must be base64 encoded"
        )));
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|err| {
            image_edit_input_error(format!("image {index} base64 payload is invalid: {err}"))
        })?;

    Ok(ImageEditFile {
        file_name: image_file_name(index, mime),
        mime: mime.to_string(),
        bytes,
    })
}

fn image_edit_input_error(message: String) -> ApiError {
    ApiError::Stream(format!("failed to prepare image edit input: {message}"))
}

fn image_file_name(index: usize, mime: &str) -> String {
    let extension = match mime.to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpeg",
        "image/webp" => "webp",
        _ => "bin",
    };
    format!("image-{index}.{extension}")
}

fn image_background_wire(background: ImageBackground) -> &'static str {
    match background {
        ImageBackground::Auto => "auto",
        ImageBackground::Opaque => "opaque",
        ImageBackground::Transparent => "transparent",
    }
}

fn image_quality_wire(quality: ImageQuality) -> &'static str {
    match quality {
        ImageQuality::Auto => "auto",
        ImageQuality::Low => "low",
        ImageQuality::Medium => "medium",
        ImageQuality::High => "high",
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
    use crate::api::provider::RetryConfig;
    use crate::api::provider::WireApi;
    use crate::client::Request;
    use crate::client::Response;
    use crate::client::StreamResponse;
    use crate::client::TransportError;
    use async_trait::async_trait;
    use http::StatusCode;
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

    fn assert_image_edit_input_error(error: ApiError, expected: &str) {
        let ApiError::Stream(message) = error else {
            panic!("expected image edit input error");
        };
        assert!(
            message.starts_with("failed to prepare image edit input: "),
            "unexpected message: {message}"
        );
        assert!(
            message.contains(expected),
            "expected `{expected}` in `{message}`"
        );
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
        assert_eq!(request.multipart, None);
    }

    #[tokio::test]
    async fn edit_posts_multipart_request_and_omits_none_fields() {
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
        assert_eq!(request.body, None);
        assert_eq!(
            request.multipart,
            Some(
                MultipartForm::new()
                    .bytes("image", "image-1.png", "image/png", b"foo".to_vec())
                    .text("prompt", "add a red hat")
                    .text("model", "gpt-image-2")
            )
        );
    }

    #[tokio::test]
    async fn edit_rejects_non_data_url_image_input() {
        let transport = CapturingTransport::new(response_body());
        let client = ImagesClient::new(transport, provider(), DummyAuth);

        let error = client
            .edit(
                &ImageEditRequest {
                    images: vec![ImageUrl {
                        image_url: "https://example.test/image.png".to_string(),
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
            .expect_err("non-data URL should fail");

        assert_image_edit_input_error(error, "image 1 must be a base64 data URL");
    }

    #[tokio::test]
    async fn edit_rejects_invalid_base64_image_input() {
        let transport = CapturingTransport::new(response_body());
        let client = ImagesClient::new(transport, provider(), DummyAuth);

        let error = client
            .edit(
                &ImageEditRequest {
                    images: vec![ImageUrl {
                        image_url: "data:image/png;base64,not base64".to_string(),
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
            .expect_err("invalid base64 should fail");

        assert_image_edit_input_error(error, "image 1 base64 payload is invalid");
    }

    #[tokio::test]
    async fn edit_rejects_non_image_data_url_input() {
        let transport = CapturingTransport::new(response_body());
        let client = ImagesClient::new(transport, provider(), DummyAuth);

        let error = client
            .edit(
                &ImageEditRequest {
                    images: vec![ImageUrl {
                        image_url: "data:text/plain;base64,Zm9v".to_string(),
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
            .expect_err("non-image data URL should fail");

        assert_image_edit_input_error(error, "image 1 data URL MIME type must be image/*");
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
