use bytes::Bytes;
use http::Method;
use reqwest::header::HeaderMap;
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RequestCompression {
    #[default]
    None,
    Zstd,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MultipartForm {
    pub parts: Vec<MultipartPart>,
}

impl MultipartForm {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.parts.push(MultipartPart::Text {
            name: name.into(),
            value: value.into(),
        });
        self
    }

    pub fn bytes(
        mut self,
        name: impl Into<String>,
        file_name: impl Into<String>,
        mime: impl Into<String>,
        bytes: impl Into<Bytes>,
    ) -> Self {
        self.parts.push(MultipartPart::Bytes {
            name: name.into(),
            file_name: file_name.into(),
            mime: mime.into(),
            bytes: bytes.into(),
        });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MultipartPart {
    Text {
        name: String,
        value: String,
    },
    Bytes {
        name: String,
        file_name: String,
        mime: String,
        bytes: Bytes,
    },
}

#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Option<Value>,
    pub multipart: Option<MultipartForm>,
    pub compression: RequestCompression,
    pub timeout: Option<Duration>,
}

impl Request {
    pub fn new(method: Method, url: String) -> Self {
        Self {
            method,
            url,
            headers: HeaderMap::new(),
            body: None,
            multipart: None,
            compression: RequestCompression::None,
            timeout: None,
        }
    }

    pub fn with_json<T: Serialize>(mut self, body: &T) -> Self {
        self.body = serde_json::to_value(body).ok();
        self.multipart = None;
        self
    }

    pub fn with_multipart(mut self, multipart: MultipartForm) -> Self {
        self.body = None;
        self.multipart = Some(multipart);
        self
    }

    pub fn with_compression(mut self, compression: RequestCompression) -> Self {
        self.compression = compression;
        self
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: http::StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}
