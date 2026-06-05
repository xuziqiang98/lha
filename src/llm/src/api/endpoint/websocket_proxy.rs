use base64::Engine;
use http::Uri;
use std::fmt;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::client_async_tls_with_config;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::handshake::client::Response;

const MAX_CONNECT_RESPONSE_SIZE: usize = 8192;

#[derive(Debug)]
pub(crate) enum ConnectError {
    Proxy(ProxyError),
    WebSocket(WsError),
}

impl From<ProxyError> for ConnectError {
    fn from(error: ProxyError) -> Self {
        Self::Proxy(error)
    }
}

impl From<WsError> for ConnectError {
    fn from(error: WsError) -> Self {
        Self::WebSocket(error)
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::Proxy(error) => error.fmt(f),
            ConnectError::WebSocket(error) => error.fmt(f),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProxyError {
    message: String,
}

impl ProxyError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProxyError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyScheme {
    Http,
    Socks5,
    Socks5h,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProxyAuth {
    username: String,
    password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProxyConfig {
    scheme: ProxyScheme,
    host: String,
    port: u16,
    auth: Option<ProxyAuth>,
}

impl ProxyConfig {
    fn authority(&self) -> (&str, u16) {
        (&self.host, self.port)
    }
}

pub(crate) async fn connect_async(
    request: Request,
) -> Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response), ConnectError> {
    let host = request_host(request.uri())?.to_string();
    let port = request_port(request.uri())?;
    let proxy = proxy_from_process_env(request.uri())?;
    let socket = connect_socket(proxy.as_ref(), &host, port).await?;

    client_async_tls_with_config(request, socket, None, None)
        .await
        .map_err(ConnectError::WebSocket)
}

async fn connect_socket(
    proxy: Option<&ProxyConfig>,
    host: &str,
    port: u16,
) -> Result<TcpStream, ProxyError> {
    match proxy {
        Some(proxy) => {
            let socket = TcpStream::connect(proxy.authority())
                .await
                .map_err(|err| ProxyError::new(format!("failed to connect to proxy: {err}")))?;
            connect_via_proxy(socket, proxy, host, port).await
        }
        None => TcpStream::connect((host, port))
            .await
            .map_err(|err| ProxyError::new(format!("failed to connect to websocket host: {err}"))),
    }
}

async fn connect_via_proxy<S>(
    mut stream: S,
    proxy: &ProxyConfig,
    host: &str,
    port: u16,
) -> Result<S, ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match proxy.scheme {
        ProxyScheme::Http => http_connect(&mut stream, host, port, proxy.auth.as_ref()).await?,
        ProxyScheme::Socks5 | ProxyScheme::Socks5h => {
            socks5_handshake(&mut stream, host, port, proxy.auth.as_ref()).await?;
        }
    }

    Ok(stream)
}

fn proxy_from_process_env(uri: &Uri) -> Result<Option<ProxyConfig>, ProxyError> {
    proxy_from_env_lookup(uri, |name| std::env::var(name).ok())
}

fn proxy_from_env_lookup(
    uri: &Uri,
    mut env: impl FnMut(&str) -> Option<String>,
) -> Result<Option<ProxyConfig>, ProxyError> {
    let host = request_host(uri)?;
    let port = request_port(uri)?;

    if should_bypass_proxy(
        host,
        port,
        env_first(&mut env, &["NO_PROXY", "no_proxy"]).as_deref(),
    ) {
        return Ok(None);
    }

    let proxy = match request_mode(uri)? {
        RequestMode::Plain => env_first(&mut env, &["HTTP_PROXY", "http_proxy"])
            .or_else(|| env_first(&mut env, &["ALL_PROXY", "all_proxy"])),
        RequestMode::Tls => env_first(&mut env, &["HTTPS_PROXY", "https_proxy"])
            .or_else(|| env_first(&mut env, &["HTTP_PROXY", "http_proxy"]))
            .or_else(|| env_first(&mut env, &["ALL_PROXY", "all_proxy"])),
    };

    proxy.as_deref().map(parse_proxy_config).transpose()
}

fn env_first(env: &mut impl FnMut(&str) -> Option<String>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| env(name))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestMode {
    Plain,
    Tls,
}

fn request_mode(uri: &Uri) -> Result<RequestMode, ProxyError> {
    match uri.scheme_str() {
        Some("ws") => Ok(RequestMode::Plain),
        Some("wss") => Ok(RequestMode::Tls),
        Some(scheme) => Err(ProxyError::new(format!(
            "unsupported websocket URL scheme for proxy resolution: {scheme}"
        ))),
        None => Err(ProxyError::new(
            "websocket URL is missing a scheme for proxy resolution",
        )),
    }
}

fn request_host(uri: &Uri) -> Result<&str, ProxyError> {
    uri.host()
        .map(strip_ipv6_brackets)
        .ok_or_else(|| ProxyError::new("websocket URL is missing a host"))
}

fn request_port(uri: &Uri) -> Result<u16, ProxyError> {
    uri.port_u16()
        .or_else(|| default_port(uri))
        .ok_or_else(|| ProxyError::new("websocket URL is missing a port and has no default port"))
}

fn default_port(uri: &Uri) -> Option<u16> {
    match uri.scheme_str() {
        Some("ws") => Some(80),
        Some("wss") => Some(443),
        Some(_) | None => None,
    }
}

fn parse_proxy_config(value: &str) -> Result<ProxyConfig, ProxyError> {
    let url = url::Url::parse(value)
        .map_err(|err| ProxyError::new(format!("invalid proxy URL '{value}': {err}")))?;

    let scheme = match url.scheme() {
        "http" => ProxyScheme::Http,
        "socks5" => ProxyScheme::Socks5,
        "socks5h" => ProxyScheme::Socks5h,
        scheme => {
            return Err(ProxyError::new(format!(
                "unsupported proxy URL scheme: {scheme}"
            )));
        }
    };

    let host = url
        .host_str()
        .map(strip_ipv6_brackets)
        .ok_or_else(|| ProxyError::new(format!("proxy URL is missing a host: {value}")))?
        .to_string();
    let port = url.port().unwrap_or(match scheme {
        ProxyScheme::Http => 80,
        ProxyScheme::Socks5 | ProxyScheme::Socks5h => 1080,
    });
    let auth = if url.username().is_empty() {
        None
    } else {
        Some(ProxyAuth {
            username: percent_decode(url.username())?,
            password: percent_decode(url.password().unwrap_or(""))?,
        })
    };

    Ok(ProxyConfig {
        scheme,
        host,
        port,
        auth,
    })
}

fn strip_ipv6_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|stripped| stripped.strip_suffix(']'))
        .unwrap_or(host)
}

fn should_bypass_proxy(host: &str, port: u16, no_proxy: Option<&str>) -> bool {
    let Some(no_proxy) = no_proxy else {
        return false;
    };

    let host = host.trim_matches(&['[', ']'][..]).to_ascii_lowercase();
    no_proxy.split(',').any(|token| {
        let token = token.trim();
        if token.is_empty() {
            return false;
        }
        if token == "*" {
            return true;
        }

        let (token_host, token_port) = split_no_proxy_token(token);
        if token_port.is_some_and(|token_port| token_port != port) {
            return false;
        }

        let token_host = token_host
            .trim_matches(&['[', ']'][..])
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if token_host.is_empty() {
            return false;
        }

        host == token_host || host.ends_with(&format!(".{token_host}"))
    })
}

fn split_no_proxy_token(token: &str) -> (&str, Option<u16>) {
    if token.starts_with('[')
        && let Some((host, rest)) = token.rsplit_once("]:")
    {
        return (host.trim_start_matches('['), rest.parse::<u16>().ok());
    }

    if token.matches(':').count() == 1
        && let Some((host, port)) = token.rsplit_once(':')
    {
        return (host, port.parse::<u16>().ok());
    }

    (token, None)
}

async fn http_connect<S>(
    stream: &mut S,
    host: &str,
    port: u16,
    auth: Option<&ProxyAuth>,
) -> Result<(), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let authority = authority(host, port);
    let request = build_http_connect_request(&authority, auth);
    stream.write_all(&request).await.map_err(proxy_io_error)?;
    stream.flush().await.map_err(proxy_io_error)?;

    let response = read_connect_response(stream).await?;
    let status = parse_http_connect_status(&response)?;
    if !(200..300).contains(&status) {
        return Err(ProxyError::new(format!(
            "HTTP CONNECT failed with status {status}"
        )));
    }

    Ok(())
}

fn authority(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn build_http_connect_request(authority: &str, auth: Option<&ProxyAuth>) -> Vec<u8> {
    let mut request = Vec::new();
    request.extend_from_slice(format!("CONNECT {authority} HTTP/1.1\r\n").as_bytes());
    request.extend_from_slice(format!("Host: {authority}\r\n").as_bytes());
    request.extend_from_slice(b"Proxy-Connection: Keep-Alive\r\n");
    if let Some(auth) = auth {
        let token = basic_auth_header(auth);
        request.extend_from_slice(format!("Proxy-Authorization: {token}\r\n").as_bytes());
    }
    request.extend_from_slice(b"\r\n");
    request
}

fn basic_auth_header(auth: &ProxyAuth) -> String {
    let token = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", auth.username, auth.password));
    format!("Basic {token}")
}

async fn read_connect_response<S>(stream: &mut S) -> Result<Vec<u8>, ProxyError>
where
    S: AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        if buf.len() >= MAX_CONNECT_RESPONSE_SIZE {
            return Err(ProxyError::new("HTTP CONNECT response too large"));
        }

        let read = stream.read(&mut chunk).await.map_err(proxy_io_error)?;
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    Ok(buf)
}

fn parse_http_connect_status(response: &[u8]) -> Result<u16, ProxyError> {
    let text = std::str::from_utf8(response)
        .map_err(|_| ProxyError::new("HTTP CONNECT response not valid UTF-8"))?;
    let status_line = text
        .lines()
        .next()
        .ok_or_else(|| ProxyError::new("HTTP CONNECT response missing status line"))?;
    let code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| ProxyError::new("HTTP CONNECT response missing status code"))?;

    code.parse::<u16>()
        .map_err(|_| ProxyError::new("HTTP CONNECT response invalid status code"))
}

async fn socks5_handshake<S>(
    stream: &mut S,
    host: &str,
    port: u16,
    auth: Option<&ProxyAuth>,
) -> Result<(), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut methods = vec![0x00];
    if auth.is_some() {
        methods.push(0x02);
    }

    stream
        .write_all(&[0x05, methods.len() as u8])
        .await
        .map_err(proxy_io_error)?;
    stream.write_all(&methods).await.map_err(proxy_io_error)?;
    stream.flush().await.map_err(proxy_io_error)?;

    let mut choice = [0u8; 2];
    stream
        .read_exact(&mut choice)
        .await
        .map_err(proxy_io_error)?;
    if choice[0] != 0x05 {
        return Err(ProxyError::new("SOCKS5: invalid response version"));
    }

    match choice[1] {
        0x00 => {}
        0x02 => {
            let auth = auth.ok_or_else(|| {
                ProxyError::new("SOCKS5: proxy requested auth, but none provided")
            })?;
            socks5_userpass_auth(stream, auth).await?;
        }
        0xFF => {
            return Err(ProxyError::new(
                "SOCKS5: no acceptable authentication method",
            ));
        }
        _ => return Err(ProxyError::new("SOCKS5: unsupported authentication method")),
    }

    send_socks5_connect(stream, host, port).await
}

async fn socks5_userpass_auth<S>(stream: &mut S, auth: &ProxyAuth) -> Result<(), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let username = auth.username.as_bytes();
    let password = auth.password.as_bytes();
    if username.len() > u8::MAX as usize || password.len() > u8::MAX as usize {
        return Err(ProxyError::new("SOCKS5 auth credentials too long"));
    }

    let mut request = Vec::with_capacity(3 + username.len() + password.len());
    request.push(0x01);
    request.push(username.len() as u8);
    request.extend_from_slice(username);
    request.push(password.len() as u8);
    request.extend_from_slice(password);

    stream.write_all(&request).await.map_err(proxy_io_error)?;
    stream.flush().await.map_err(proxy_io_error)?;

    let mut response = [0u8; 2];
    stream
        .read_exact(&mut response)
        .await
        .map_err(proxy_io_error)?;
    if response != [0x01, 0x00] {
        return Err(ProxyError::new("SOCKS5 authentication failed"));
    }

    Ok(())
}

async fn send_socks5_connect<S>(stream: &mut S, host: &str, port: u16) -> Result<(), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut request = Vec::new();
    request.push(0x05);
    request.push(0x01);
    request.push(0x00);

    if let Ok(addr) = host.parse::<std::net::Ipv4Addr>() {
        request.push(0x01);
        request.extend_from_slice(&addr.octets());
    } else if let Ok(addr) = host.parse::<std::net::Ipv6Addr>() {
        request.push(0x04);
        request.extend_from_slice(&addr.octets());
    } else {
        let host_bytes = host.as_bytes();
        if host_bytes.len() > u8::MAX as usize {
            return Err(ProxyError::new("SOCKS5 domain name too long"));
        }
        request.push(0x03);
        request.push(host_bytes.len() as u8);
        request.extend_from_slice(host_bytes);
    }

    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await.map_err(proxy_io_error)?;
    stream.flush().await.map_err(proxy_io_error)?;

    let mut header = [0u8; 4];
    stream
        .read_exact(&mut header)
        .await
        .map_err(proxy_io_error)?;
    if header[0] != 0x05 {
        return Err(ProxyError::new("SOCKS5: invalid response version"));
    }
    if header[1] != 0x00 {
        return Err(ProxyError::new(format!(
            "SOCKS5: connection failed with code {}",
            header[1]
        )));
    }

    let addr_len = match header[3] {
        0x01 => 4,
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await.map_err(proxy_io_error)?;
            len[0] as usize
        }
        0x04 => 16,
        _ => return Err(ProxyError::new("SOCKS5: invalid address type")),
    };

    let mut discard = vec![0u8; addr_len + 2];
    stream
        .read_exact(&mut discard)
        .await
        .map_err(proxy_io_error)?;
    Ok(())
}

fn proxy_io_error(error: std::io::Error) -> ProxyError {
    ProxyError::new(format!("proxy connection failed: {error}"))
}

fn percent_decode(value: &str) -> Result<String, ProxyError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(ProxyError::new(format!(
                    "invalid percent escape in proxy credentials: {value}"
                )));
            }
            let hi = hex_value(bytes[index + 1])?;
            let lo = hex_value(bytes[index + 2])?;
            decoded.push((hi << 4) | lo);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }

    String::from_utf8(decoded).map_err(|_| {
        ProxyError::new(format!(
            "proxy credentials are not valid UTF-8 after decoding: {value}"
        ))
    })
}

fn hex_value(byte: u8) -> Result<u8, ProxyError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(ProxyError::new("invalid hex digit in proxy credentials")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;

    fn proxy_for_uri_with_env(
        uri: &str,
        env: &[(&str, &str)],
    ) -> Result<Option<ProxyConfig>, ProxyError> {
        let uri = uri
            .parse::<Uri>()
            .map_err(|err| ProxyError::new(format!("failed to parse uri: {err}")))?;
        proxy_from_env_lookup(&uri, |name| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        })
    }

    #[test]
    fn websocket_proxy_env_prefers_https_for_wss() -> Result<(), Box<dyn std::error::Error>> {
        let proxy = proxy_for_uri_with_env(
            "wss://api.example.com/v1/responses",
            &[
                ("HTTP_PROXY", "http://proxy-http.local:8080"),
                ("HTTPS_PROXY", "http://proxy-https.local:8443"),
            ],
        )?;

        assert_eq!(
            proxy,
            Some(ProxyConfig {
                scheme: ProxyScheme::Http,
                host: "proxy-https.local".to_string(),
                port: 8443,
                auth: None,
            })
        );
        Ok(())
    }

    #[test]
    fn websocket_proxy_env_uses_all_proxy_as_fallback() -> Result<(), Box<dyn std::error::Error>> {
        let proxy = proxy_for_uri_with_env(
            "ws://api.example.com/v1/responses",
            &[("ALL_PROXY", "socks5://user:pass@proxy.local")],
        )?;

        assert_eq!(
            proxy,
            Some(ProxyConfig {
                scheme: ProxyScheme::Socks5,
                host: "proxy.local".to_string(),
                port: 1080,
                auth: Some(ProxyAuth {
                    username: "user".to_string(),
                    password: "pass".to_string(),
                }),
            })
        );
        Ok(())
    }

    #[test]
    fn websocket_proxy_no_proxy_bypasses_domain_suffix_and_port()
    -> Result<(), Box<dyn std::error::Error>> {
        let proxy = proxy_for_uri_with_env(
            "wss://api.example.com:443/v1/responses",
            &[
                ("HTTPS_PROXY", "http://proxy.local:8443"),
                ("NO_PROXY", ".example.com:443"),
            ],
        )?;

        assert_eq!(proxy, None);
        Ok(())
    }

    #[test]
    fn websocket_proxy_no_proxy_wrong_port_does_not_bypass()
    -> Result<(), Box<dyn std::error::Error>> {
        let proxy = proxy_for_uri_with_env(
            "wss://api.example.com:443/v1/responses",
            &[
                ("HTTPS_PROXY", "http://proxy.local:8443"),
                ("NO_PROXY", ".example.com:444"),
            ],
        )?;

        assert_eq!(
            proxy,
            Some(ProxyConfig {
                scheme: ProxyScheme::Http,
                host: "proxy.local".to_string(),
                port: 8443,
                auth: None,
            })
        );
        Ok(())
    }

    #[test]
    fn websocket_proxy_no_env_uses_direct_path() -> Result<(), Box<dyn std::error::Error>> {
        let proxy = proxy_for_uri_with_env("wss://api.example.com/v1/responses", &[])?;

        assert_eq!(proxy, None);
        Ok(())
    }

    #[test]
    fn websocket_proxy_parses_percent_encoded_auth() -> Result<(), Box<dyn std::error::Error>> {
        let proxy = proxy_for_uri_with_env(
            "wss://api.example.com/v1/responses",
            &[("HTTPS_PROXY", "http://user%20name:pass%21@proxy.local")],
        )?;

        assert_eq!(
            proxy.and_then(|proxy| proxy.auth),
            Some(ProxyAuth {
                username: "user name".to_string(),
                password: "pass!".to_string(),
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn websocket_proxy_http_connect_request_includes_auth()
    -> Result<(), Box<dyn std::error::Error>> {
        let (client, mut server) = tokio::io::duplex(1024);
        let proxy = ProxyConfig {
            scheme: ProxyScheme::Http,
            host: "proxy.local".to_string(),
            port: 3128,
            auth: Some(ProxyAuth {
                username: "user".to_string(),
                password: "pass".to_string(),
            }),
        };

        let client_task =
            tokio::spawn(
                async move { connect_via_proxy(client, &proxy, "example.com", 443).await },
            );

        let mut buf = vec![0u8; 256];
        let n = server.read(&mut buf).await?;
        let request = std::str::from_utf8(&buf[..n])?;
        assert!(request.contains("CONNECT example.com:443 HTTP/1.1"));
        assert!(request.contains("Proxy-Authorization: Basic dXNlcjpwYXNz"));

        server
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        client_task.await??;
        Ok(())
    }

    #[tokio::test]
    async fn websocket_proxy_http_connect_non_2xx_reports_status()
    -> Result<(), Box<dyn std::error::Error>> {
        let (client, mut server) = tokio::io::duplex(1024);
        let proxy = ProxyConfig {
            scheme: ProxyScheme::Http,
            host: "proxy.local".to_string(),
            port: 3128,
            auth: None,
        };

        let client_task =
            tokio::spawn(
                async move { connect_via_proxy(client, &proxy, "example.com", 443).await },
            );

        let mut buf = vec![0u8; 256];
        let _n = server.read(&mut buf).await?;
        server.write_all(b"HTTP/1.1 407 Forbidden\r\n\r\n").await?;
        let result = client_task.await?;
        let error = result.err().ok_or_else(|| {
            ProxyError::new("expected HTTP CONNECT error but proxy connection succeeded")
        })?;

        assert_eq!(error.to_string(), "HTTP CONNECT failed with status 407");
        Ok(())
    }

    #[tokio::test]
    async fn websocket_proxy_socks5_handshake() -> Result<(), Box<dyn std::error::Error>> {
        let (client, mut server) = tokio::io::duplex(1024);
        let proxy = ProxyConfig {
            scheme: ProxyScheme::Socks5,
            host: "proxy.local".to_string(),
            port: 1080,
            auth: None,
        };

        let client_task =
            tokio::spawn(
                async move { connect_via_proxy(client, &proxy, "example.com", 443).await },
            );

        let mut greeting = [0u8; 3];
        server.read_exact(&mut greeting).await?;
        assert_eq!(greeting, [0x05, 0x01, 0x00]);
        server.write_all(&[0x05, 0x00]).await?;

        let mut connect = [0u8; 18];
        let n = server.read(&mut connect).await?;
        assert_eq!(&connect[..n], b"\x05\x01\x00\x03\x0bexample.com\x01\xbb");
        server
            .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;

        client_task.await??;
        Ok(())
    }

    #[tokio::test]
    async fn websocket_proxy_socks5_no_acceptable_auth_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let (client, mut server) = tokio::io::duplex(1024);
        let proxy = ProxyConfig {
            scheme: ProxyScheme::Socks5,
            host: "proxy.local".to_string(),
            port: 1080,
            auth: None,
        };

        let client_task =
            tokio::spawn(
                async move { connect_via_proxy(client, &proxy, "example.com", 443).await },
            );

        let mut greeting = [0u8; 3];
        server.read_exact(&mut greeting).await?;
        server.write_all(&[0x05, 0xFF]).await?;
        let result = client_task.await?;
        let error = result.err().ok_or_else(|| {
            ProxyError::new("expected SOCKS5 error but proxy connection succeeded")
        })?;

        assert_eq!(
            error.to_string(),
            "SOCKS5: no acceptable authentication method"
        );
        Ok(())
    }
}
