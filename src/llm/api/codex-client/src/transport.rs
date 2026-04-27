use crate::default_client::CodexHttpClient;
use crate::default_client::CodexRequestBuilder;
use crate::error::TransportError;
use crate::request::Request;
use crate::request::RequestCompression;
use crate::request::Response;
use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use http::HeaderMap;
use http::Method;
use http::StatusCode;
use std::time::Instant;
use tracing::Level;
use tracing::enabled;
use tracing::info;
use tracing::trace;
use tracing::warn;

pub type ByteStream = BoxStream<'static, Result<Bytes, TransportError>>;

pub struct StreamResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub bytes: ByteStream,
}

#[derive(Clone, Copy, Debug)]
enum ReqwestErrorPhase {
    ExecuteRequest,
    ExecuteBody,
    StreamRequest,
    StreamBody,
}

impl ReqwestErrorPhase {
    fn as_str(self) -> &'static str {
        match self {
            ReqwestErrorPhase::ExecuteRequest => "request send",
            ReqwestErrorPhase::ExecuteBody => "response body",
            ReqwestErrorPhase::StreamRequest => "stream request send",
            ReqwestErrorPhase::StreamBody => "stream response body",
        }
    }
}

#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn execute(&self, req: Request) -> Result<Response, TransportError>;
    async fn stream(&self, req: Request) -> Result<StreamResponse, TransportError>;
}

#[derive(Clone, Debug)]
pub struct ReqwestTransport {
    client: CodexHttpClient,
}

impl ReqwestTransport {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client: CodexHttpClient::new(client),
        }
    }

    fn build(&self, req: Request) -> Result<CodexRequestBuilder, TransportError> {
        let Request {
            method,
            url,
            mut headers,
            body,
            compression,
            timeout,
        } = req;

        let mut builder = self.client.request(
            Method::from_bytes(method.as_str().as_bytes()).unwrap_or(Method::GET),
            &url,
        );

        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }

        if let Some(body) = body {
            if compression != RequestCompression::None {
                if headers.contains_key(http::header::CONTENT_ENCODING) {
                    return Err(TransportError::Build(
                        "request compression was requested but content-encoding is already set"
                            .to_string(),
                    ));
                }

                let json = serde_json::to_vec(&body)
                    .map_err(|err| TransportError::Build(err.to_string()))?;
                let pre_compression_bytes = json.len();
                let compression_start = std::time::Instant::now();
                let (compressed, content_encoding) = match compression {
                    RequestCompression::None => unreachable!("guarded by compression != None"),
                    RequestCompression::Zstd => (
                        zstd::stream::encode_all(std::io::Cursor::new(json), 3)
                            .map_err(|err| TransportError::Build(err.to_string()))?,
                        http::HeaderValue::from_static("zstd"),
                    ),
                };
                let post_compression_bytes = compressed.len();
                let compression_duration = compression_start.elapsed();

                // Ensure the server knows to unpack the request body.
                headers.insert(http::header::CONTENT_ENCODING, content_encoding);
                if !headers.contains_key(http::header::CONTENT_TYPE) {
                    headers.insert(
                        http::header::CONTENT_TYPE,
                        http::HeaderValue::from_static("application/json"),
                    );
                }

                tracing::info!(
                    pre_compression_bytes,
                    post_compression_bytes,
                    compression_duration_ms = compression_duration.as_millis(),
                    "Compressed request body with zstd"
                );

                builder = builder.headers(headers).body(compressed);
            } else {
                builder = builder.headers(headers).json(&body);
            }
        } else {
            builder = builder.headers(headers);
        }
        Ok(builder)
    }

    fn map_error(err: reqwest::Error, phase: ReqwestErrorPhase) -> TransportError {
        if err.is_timeout() {
            TransportError::Timeout
        } else {
            TransportError::Network(describe_reqwest_error(&err, phase))
        }
    }
}

fn describe_reqwest_error(err: &reqwest::Error, phase: ReqwestErrorPhase) -> String {
    let kind = classify_reqwest_error(err);
    let url = err.url().map(sanitized_url);
    let sources = error_sources(err);

    format_reqwest_error(phase, &kind, url.as_deref(), &err.to_string(), sources)
}

fn classify_reqwest_error(err: &reqwest::Error) -> String {
    let mut kinds = Vec::new();
    if err.is_connect() {
        kinds.push("connect");
    }
    if err.is_request() {
        kinds.push("request");
    }
    if err.is_body() {
        kinds.push("body");
    }
    if err.is_decode() {
        kinds.push("decode");
    }
    if err.is_redirect() {
        kinds.push("redirect");
    }
    if err.is_status() {
        kinds.push("status");
    }

    if kinds.is_empty() {
        "other".to_string()
    } else {
        kinds.join(",")
    }
}

fn sanitized_url(url: &reqwest::Url) -> String {
    let mut url = url.clone();
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn sanitized_url_str(url: &str) -> String {
    reqwest::Url::parse(url).map_or_else(
        |_| url.split(['?', '#']).next().unwrap_or(url).to_string(),
        |url| sanitized_url(&url),
    )
}

fn error_sources(err: &reqwest::Error) -> Vec<String> {
    let mut sources = Vec::new();
    let mut source = std::error::Error::source(err);
    while let Some(err) = source {
        sources.push(err.to_string());
        source = err.source();
    }
    sources
}

fn format_reqwest_error(
    phase: ReqwestErrorPhase,
    kind: &str,
    url: Option<&str>,
    error: &str,
    sources: Vec<String>,
) -> String {
    let url = url.map_or_else(String::new, |url| format!(", url={url}"));
    let sources = if sources.is_empty() {
        String::new()
    } else {
        format!("; source: {}", sources.join(" -> "))
    };

    format!(
        "{} failed (kind={kind}{url}): {error}{sources}",
        phase.as_str()
    )
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn execute(&self, req: Request) -> Result<Response, TransportError> {
        if enabled!(Level::TRACE) {
            trace!(
                "{} to {}: {}",
                req.method,
                req.url,
                req.body.as_ref().unwrap_or_default()
            );
        }

        let url = req.url.clone();
        let builder = self.build(req)?;
        let resp = builder
            .send()
            .await
            .map_err(|err| Self::map_error(err, ReqwestErrorPhase::ExecuteRequest))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp
            .bytes()
            .await
            .map_err(|err| Self::map_error(err, ReqwestErrorPhase::ExecuteBody))?;
        if !status.is_success() {
            let body = String::from_utf8(bytes.to_vec()).ok();
            return Err(TransportError::Http {
                status,
                url: Some(url),
                headers: Some(headers),
                body,
            });
        }
        Ok(Response {
            status,
            headers,
            body: bytes,
        })
    }

    async fn stream(&self, req: Request) -> Result<StreamResponse, TransportError> {
        if enabled!(Level::TRACE) {
            trace!(
                "{} to {}: {}",
                req.method,
                req.url,
                req.body.as_ref().unwrap_or_default()
            );
        }

        let url = req.url.clone();
        let sanitized_url = sanitized_url_str(&url);
        let method = req.method.clone();
        let builder = self.build(req)?;
        info!(method = %method, url = %sanitized_url, "Starting stream request");
        let start = Instant::now();
        let resp = match builder.send().await {
            Ok(resp) => resp,
            Err(err) => {
                warn!(
                    method = %method,
                    url = %sanitized_url,
                    elapsed_ms = start.elapsed().as_millis(),
                    error = %err,
                    "Stream request failed"
                );
                return Err(Self::map_error(err, ReqwestErrorPhase::StreamRequest));
            }
        };
        let status = resp.status();
        let headers = resp.headers().clone();
        info!(
            method = %method,
            url = %sanitized_url,
            status = %status,
            version = ?resp.version(),
            elapsed_ms = start.elapsed().as_millis(),
            "Stream response headers received"
        );
        if !status.is_success() {
            let body = resp.text().await.ok();
            return Err(TransportError::Http {
                status,
                url: Some(url),
                headers: Some(headers),
                body,
            });
        }
        let stream = resp.bytes_stream().map(|result| {
            result.map_err(|err| Self::map_error(err, ReqwestErrorPhase::StreamBody))
        });
        Ok(StreamResponse {
            status,
            headers,
            bytes: Box::pin(stream),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn format_reqwest_error_includes_phase_kind_url_and_source_chain() {
        let message = format_reqwest_error(
            ReqwestErrorPhase::StreamBody,
            "body",
            Some("https://api.i9vc.com/v1/responses"),
            "connection closed before message completed",
            vec![
                "tcp stream reset".to_string(),
                "proxy tunnel closed".to_string(),
            ],
        );

        assert_eq!(
            message,
            "stream response body failed (kind=body, url=https://api.i9vc.com/v1/responses): connection closed before message completed; source: tcp stream reset -> proxy tunnel closed"
        );
    }

    #[test]
    fn format_reqwest_error_omits_absent_url_and_sources() {
        let message = format_reqwest_error(
            ReqwestErrorPhase::StreamRequest,
            "connect,request",
            None,
            "error sending request",
            Vec::new(),
        );

        assert_eq!(
            message,
            "stream request send failed (kind=connect,request): error sending request"
        );
    }

    #[test]
    fn sanitized_url_drops_query_and_fragment() {
        let url = reqwest::Url::parse("https://api.example.test/v1/responses?api_key=secret#frag")
            .expect("valid url");

        assert_eq!(sanitized_url(&url), "https://api.example.test/v1/responses");
    }

    #[test]
    fn sanitized_url_str_drops_query_from_unparseable_url() {
        assert_eq!(
            sanitized_url_str("not a url?api_key=secret#frag"),
            "not a url"
        );
    }
}
