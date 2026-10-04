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

/// The most a [`Transport::get`] answer may hold. A catalog page is a few
/// hundred kilobytes; anything near this is not a Subsonic answer, and is
/// read into memory whole.
pub const MAX_ANSWER_BYTES: u64 = 64 << 20;

/// The most a [`Transport::get_to_file`] download may hold: room for an
/// hour-long 24-bit/192 kHz FLAC, and a stop to a server that would fill
/// the disk.
pub const MAX_DOWNLOAD_BYTES: u64 = 4 << 30;

/// Sent with every request. minreq sends no `User-Agent` of its own, and
/// reverse proxies and Cloudflare turn such requests away with a 403 while
/// browsers and phone apps pass.
pub const USER_AGENT: &str = concat!("Sparkamp/", env!("CARGO_PKG_VERSION"), " (+https://github.com/jrssae/sparkamp)");

fn request(url: &str, timeout_secs: u64) -> minreq::Request {
    minreq::get(url)
        .with_timeout(timeout_secs)
        .with_header("User-Agent", USER_AGENT)
        .with_header("Accept", "*/*")
        // The credentials ride in the query string, and minreq would carry
        // them on to wherever a redirect points: another host, or plain HTTP.
        .with_follow_redirects(false)
}

/// Refuse an answer that says up front it is larger than `limit`.
fn refuse_declared_size(headers: &std::collections::HashMap<String, String>, limit: u64) -> Result<(), ServerError> {
    match headers.get("content-length").and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(len) if len > limit => Err(ServerError::TooLarge { limit }),
        _ => Ok(()),
    }
}

/// The redirect a response is, if it is one: refused, naming its target.
fn refuse_redirect(status: i32, headers: &std::collections::HashMap<String, String>) -> Result<(), ServerError> {
    if (300..400).contains(&status) {
        if let Some(to) = headers.get("location") {
            return Err(ServerError::redirected(to));
        }
    }
    Ok(())
}

impl Transport for MinreqTransport {
    fn get(&self, url: &str, timeout_secs: u64) -> Result<HttpResponse, ServerError> {
        use std::io::Read;
        let resp = request(url, timeout_secs)
            .send_lazy()
            .map_err(|e| ServerError::unreachable(&e.to_string()))?;
        refuse_redirect(resp.status_code, &resp.headers)?;
        refuse_declared_size(&resp.headers, MAX_ANSWER_BYTES)?;
        let status = resp.status_code as u16;
        let mut body = Vec::new();
        Read::take(resp, MAX_ANSWER_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|e| ServerError::unreachable(&e.to_string()))?;
        if body.len() as u64 > MAX_ANSWER_BYTES {
            return Err(ServerError::TooLarge { limit: MAX_ANSWER_BYTES });
        }
        Ok(HttpResponse { status, body })
    }

    fn get_to_file(
        &self,
        url: &str,
        timeout_secs: u64,
        dest: &std::path::Path,
    ) -> Result<Download, ServerError> {
        use std::io::Read;
        let resp = request(url, timeout_secs)
            .send_lazy()
            .map_err(|e| ServerError::unreachable(&e.to_string()))?;
        refuse_redirect(resp.status_code, &resp.headers)?;
        refuse_declared_size(&resp.headers, MAX_DOWNLOAD_BYTES)?;
        let status = resp.status_code as u16;
        let content_type = resp
            .headers
            .get("content-type")
            .map(|v| v.to_ascii_lowercase());
        let mut file = std::fs::File::create(dest)
            .map_err(|e| ServerError::unreachable(&format!("cannot write cache file: {e}")))?;
        let bytes = std::io::copy(&mut Read::take(resp, MAX_DOWNLOAD_BYTES + 1), &mut file)
            .map_err(|e| ServerError::unreachable(&e.to_string()))?;
        if bytes > MAX_DOWNLOAD_BYTES {
            return Err(ServerError::TooLarge { limit: MAX_DOWNLOAD_BYTES });
        }
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

    fn redirect(status: &str, location: &str) -> Vec<u8> {
        format!("HTTP/1.1 {status}\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .into_bytes()
    }

    /// A redirect would carry the credentials in the query string on to
    /// wherever it points, plain HTTP or another host included. It is
    /// refused, and the error names where it pointed, minus the query.
    #[test]
    fn a_redirect_is_not_followed_and_names_where_it_pointed() {
        let base = serve(vec![
            redirect("302 Found", "http://elsewhere.example/rest/ping?u=me&t=tok&s=salt"),
            redirect("301 Moved Permanently", "https://elsewhere.example/rest/stream?id=1&t=tok"),
        ]);
        let err = MinreqTransport.get(&format!("{base}/rest/ping?u=me&t=tok&s=salt"), 5).unwrap_err();
        assert_eq!(err, ServerError::Redirected { to: "http://elsewhere.example/rest/ping".into() });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("song.part");
        let err = MinreqTransport.get_to_file(&format!("{base}/rest/stream?id=1"), 5, &dest).unwrap_err();
        assert_eq!(err, ServerError::Redirected { to: "https://elsewhere.example/rest/stream".into() });
        assert!(!dest.exists(), "nothing is written for a redirect");
    }

    /// A catalog page is a few hundred kilobytes. An answer past the cap is
    /// not one, and reading it whole could exhaust memory, whether its size
    /// is declared up front or only shows as it arrives.
    #[test]
    fn an_answer_larger_than_the_cap_is_refused() {
        let big = MAX_ANSWER_BYTES + 1;
        let declared = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {big}\r\nConnection: close\r\n\r\n{{}}"
        )
        .into_bytes();
        let mut streamed = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n".to_vec();
        streamed.resize(streamed.len() + big as usize, b' ');
        let base = serve(vec![declared, streamed]);
        for _ in 0..2 {
            let err = MinreqTransport.get(&format!("{base}/rest/search3"), 10).unwrap_err();
            assert_eq!(err, ServerError::TooLarge { limit: MAX_ANSWER_BYTES });
        }
    }

    /// A download is capped too, so a broken or hostile server cannot fill
    /// the disk. One that declares too much is refused before a byte lands.
    #[test]
    fn a_download_declared_larger_than_the_cap_is_refused_unwritten() {
        let big = MAX_DOWNLOAD_BYTES + 1;
        let base = serve(vec![format!(
            "HTTP/1.1 200 OK\r\nContent-Type: audio/flac\r\nContent-Length: {big}\r\nConnection: close\r\n\r\nfLaC"
        )
        .into_bytes()]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("song.part");
        let err = MinreqTransport.get_to_file(&format!("{base}/rest/stream?id=1"), 5, &dest).unwrap_err();
        assert_eq!(err, ServerError::TooLarge { limit: MAX_DOWNLOAD_BYTES });
        assert!(!dest.exists(), "nothing is written for a refused download");
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
