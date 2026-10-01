//! The network boundary. Everything above this trait is testable offline.

use super::error::ServerError;

/// A raw HTTP answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// What a download to a file produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub status: u16,
    /// The `Content-Type` header, lowercased, if any.
    pub content_type: Option<String>,
    pub bytes: u64,
}

/// Performs HTTP GETs. The real one is [`MinreqTransport`]; tests pass a
/// scripted fake.
pub trait Transport: Send + Sync {
    /// GET `url` with a whole-request timeout. Errors must already be
    /// redacted: implementations build them with [`ServerError::unreachable`].
    fn get(&self, url: &str, timeout_secs: u64) -> Result<HttpResponse, ServerError>;

    /// GET `url` and stream the body into a new file at `dest`, so a large
    /// FLAC never sits in memory whole.
    fn get_to_file(
        &self,
        url: &str,
        timeout_secs: u64,
        dest: &std::path::Path,
    ) -> Result<Download, ServerError>;
}

/// [`Transport`] over `minreq` with rustls, the same stack gnudb uses.
#[derive(Debug, Default, Clone, Copy)]
pub struct MinreqTransport;

/// The transport servers are reached through on this platform. macOS sends
/// HTTPS through URLSession, which Cloudflare lets through where it
/// challenges the Rust TLS stack; see [`super::transport_apple`].
#[cfg(target_os = "macos")]
pub type PlatformTransport = super::transport_apple::AppleTransport;
/// The transport servers are reached through on this platform.
#[cfg(not(target_os = "macos"))]
pub type PlatformTransport = MinreqTransport;

/// Sent with every request. minreq sends no `User-Agent` of its own, and
/// reverse proxies and Cloudflare turn such requests away with a 403 while
/// browsers and phone apps pass.
pub const USER_AGENT: &str = concat!("Sparkamp/", env!("CARGO_PKG_VERSION"), " (+https://github.com/jrssae/sparkamp)");

fn request(url: &str, timeout_secs: u64) -> minreq::Request {
    minreq::get(url)
        .with_timeout(timeout_secs)
        .with_header("User-Agent", USER_AGENT)
        .with_header("Accept", "*/*")
}

impl Transport for MinreqTransport {
    fn get(&self, url: &str, timeout_secs: u64) -> Result<HttpResponse, ServerError> {
        let resp = request(url, timeout_secs)
            .send()
            .map_err(|e| ServerError::unreachable(&e.to_string()))?;
        Ok(HttpResponse { status: resp.status_code as u16, body: resp.into_bytes() })
    }

    fn get_to_file(
        &self,
        url: &str,
        timeout_secs: u64,
        dest: &std::path::Path,
    ) -> Result<Download, ServerError> {
        let mut resp = request(url, timeout_secs)
            .send_lazy()
            .map_err(|e| ServerError::unreachable(&e.to_string()))?;
        let status = resp.status_code as u16;
        let content_type = resp
            .headers
            .get("content-type")
            .map(|v| v.to_ascii_lowercase());
        let mut file = std::fs::File::create(dest)
            .map_err(|e| ServerError::unreachable(&format!("cannot write cache file: {e}")))?;
        let bytes = std::io::copy(&mut resp, &mut file)
            .map_err(|e| ServerError::unreachable(&e.to_string()))?;
        Ok(Download { status, content_type, bytes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Serve `responses` (raw HTTP) to the next connections on a loopback
    /// port, one each, and return the base URL.
    fn serve(responses: Vec<Vec<u8>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                stream.write_all(&response).unwrap();
            }
        });
        format!("http://{addr}")
    }

    fn http(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
        let mut r = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        r.extend_from_slice(body);
        r
    }

    /// Serve one empty 200 and hand back the request as it arrived.
    fn capture_request() -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
                stream.write_all(&http("200 OK", "text/plain", b"")).unwrap();
            }
        });
        (format!("http://{addr}"), rx)
    }

    /// Proxies and Cloudflare turn away requests that do not say what sent
    /// them; every client people use with Navidrome does.
    #[test]
    fn every_request_says_it_comes_from_sparkamp() {
        let (base, requests) = capture_request();
        MinreqTransport.get(&format!("{base}/rest/ping"), 5).unwrap();
        let dir = tempfile::tempdir().unwrap();
        MinreqTransport.get_to_file(&format!("{base}/rest/stream?id=1"), 5, &dir.path().join("a")).unwrap();
        for _ in 0..2 {
            let req = requests.recv_timeout(std::time::Duration::from_secs(5)).unwrap().to_ascii_lowercase();
            assert!(req.contains(&format!("\r\nuser-agent: sparkamp/{}", env!("CARGO_PKG_VERSION"))), "{req}");
            assert!(req.contains("\r\naccept: */*"), "{req}");
        }
    }

    #[test]
    fn a_real_get_returns_status_and_body() {
        let base = serve(vec![http("200 OK", "application/json", b"{\"ok\":true}")]);
        let resp = MinreqTransport.get(&format!("{base}/rest/ping?u=me&t=x&s=y"), 5).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"{\"ok\":true}");
    }

    #[test]
    fn a_real_download_streams_to_the_file_and_reports_its_type() {
        let audio: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let base = serve(vec![http("200 OK", "audio/flac", &audio)]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("song.flac.part");
        let dl = MinreqTransport.get_to_file(&format!("{base}/rest/stream?id=1"), 5, &dest).unwrap();
        assert_eq!(dl, Download { status: 200, content_type: Some("audio/flac".into()), bytes: audio.len() as u64 });
        assert_eq!(std::fs::read(&dest).unwrap(), audio);
    }

    #[test]
    fn nothing_listening_is_unreachable_and_the_error_hides_the_query() {
        // Bind and drop to find a port nobody listens on.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let err = MinreqTransport
            .get(&format!("http://127.0.0.1:{port}/rest/ping?u=me&t=secret-token&s=salt"), 2)
            .unwrap_err();
        assert!(err.is_offline());
        assert!(!err.to_string().contains("secret-token"), "{err}");
    }
}
