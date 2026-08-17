use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use reqwest::Proxy;

/// Creates a direct client. It deliberately ignores ambient HTTP(S)_PROXY
/// settings so local Transmission RPC traffic is never sent through a proxy.
pub fn build_http_client(insecure_tls: bool) -> Result<Client> {
    build_client(insecure_tls, None)
}

/// Creates the client used only for remote discovery and rating lookups. An
/// explicit proxy can be HTTP(S) or SOCKS; Transmission does not receive this
/// client and continues to use a direct connection.
pub fn build_remote_http_client(insecure_tls: bool, proxy_url: Option<&str>) -> Result<Client> {
    build_client(insecure_tls, proxy_url)
}

fn build_client(insecure_tls: bool, proxy_url: Option<&str>) -> Result<Client> {
    let builder = Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(45))
        .user_agent("hd-movies/3.0")
        .danger_accept_invalid_certs(insecure_tls)
        // Do not silently inherit a global proxy. Remote requests use only the
        // explicit HD_MOVIES_PROXY/--proxy value; Transmission is always direct.
        .no_proxy();
    let builder = if let Some(proxy_url) = proxy_url {
        builder.proxy(Proxy::all(proxy_url).context("parse --proxy URL")?)
    } else {
        builder
    };
    builder.build().context("build HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn accepts_http_and_socks_remote_proxy_urls() {
        assert!(build_remote_http_client(false, Some("http://127.0.0.1:7890")).is_ok());
        assert!(build_remote_http_client(false, Some("socks5://127.0.0.1:1080")).is_ok());
    }

    #[test]
    fn rejects_an_invalid_proxy_url() {
        assert!(build_remote_http_client(false, Some("not a proxy URL")).is_err());
    }

    #[test]
    fn sends_remote_requests_through_the_configured_http_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_url = format!("http://{}", listener.local_addr().unwrap());
        let proxy = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0, "proxy client closed before completing headers");
                request.extend_from_slice(&buffer[..read]);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET http://example.invalid/through-proxy HTTP/1.1\r\n"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nproxied",
                )
                .unwrap();
        });

        let body = build_remote_http_client(false, Some(&proxy_url))
            .unwrap()
            .get("http://example.invalid/through-proxy")
            .send()
            .unwrap()
            .text()
            .unwrap();
        assert_eq!(body, "proxied");
        proxy.join().unwrap();
    }
}
