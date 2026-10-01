//! [`Transport`] over Apple's URLSession, for HTTPS on macOS.
//!
//! Servers behind Cloudflare can turn away anything whose connection does not
//! look like a browser or a phone app: tested against a real server, curl and
//! the Rust TLS stack got a "Just a moment..." challenge (HTTP 403) whatever
//! `User-Agent` they sent, while the same request through URLSession, the
//! stack Safari and iOS apps such as Shelv use, reached Navidrome. So HTTPS
//! goes through URLSession here.
//!
//! Plain `http://` (a home address, which never passes Cloudflare) stays on
//! [`MinreqTransport`]: App Transport Security would refuse most of it inside
//! the app, and there is nothing to gain.
//!
//! Downloads arrive through a delegate that appends each piece to the file as
//! it comes, so a song can start playing before it has finished downloading.

use super::error::ServerError;
use super::transport::{Download, HttpResponse, MinreqTransport, Transport, USER_AGENT};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSString, NSURLRequestCachePolicy,
    NSURLSession, NSURLSessionConfiguration, NSURLSessionDataDelegate, NSURLSessionDataTask,
    NSURLSessionDelegate, NSURLSessionTask, NSURLSessionTaskDelegate, NSURL,
};
use std::io::Write;
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

/// HTTPS through URLSession, plain HTTP through [`MinreqTransport`].
#[derive(Debug, Default, Clone, Copy)]
pub struct AppleTransport {
    /// Send plain HTTP through URLSession too; tests use it to reach a local
    /// server.
    all_through_url_session: bool,
}

impl AppleTransport {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn url_session_for_everything() -> Self {
        AppleTransport { all_through_url_session: true }
    }

    fn uses_url_session(&self, url: &str) -> bool {
        self.all_through_url_session || url.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://"))
    }
}

impl Transport for AppleTransport {
    fn get(&self, url: &str, timeout_secs: u64) -> Result<HttpResponse, ServerError> {
        if !self.uses_url_session(url) {
            return MinreqTransport.get(url, timeout_secs).map_err(|e| hinted(url, e));
        }
        let done = fetch(url, timeout_secs, Sink::Memory(Vec::new()))?;
        let body = match done.sink {
            Some(Sink::Memory(body)) => body,
            _ => Vec::new(),
        };
        Ok(HttpResponse { status: done.status, body })
    }

    fn get_to_file(&self, url: &str, timeout_secs: u64, dest: &std::path::Path) -> Result<Download, ServerError> {
        if !self.uses_url_session(url) {
            return MinreqTransport.get_to_file(url, timeout_secs, dest).map_err(|e| hinted(url, e));
        }
        let file = std::fs::File::create(dest)
            .map_err(|e| ServerError::unreachable(&format!("cannot write cache file: {e}")))?;
        let done = fetch(url, timeout_secs, Sink::File(file, 0))?;
        let bytes = match done.sink {
            Some(Sink::File(_, n)) => n,
            _ => 0,
        };
        Ok(Download { status: done.status, content_type: done.content_type, bytes })
    }
}

/// Where a response body goes.
enum Sink {
    Memory(Vec<u8>),
    /// The file and how many bytes are in it.
    File(std::fs::File, u64),
}

/// A finished request, as the delegate saw it.
struct Finished {
    status: u16,
    content_type: Option<String>,
    error: Option<String>,
    sink: Option<Sink>,
}

struct Ivars {
    sink: Mutex<Option<Sink>>,
    write_error: Mutex<Option<String>>,
    done: Mutex<Option<mpsc::Sender<Finished>>>,
}

define_class!(
    /// Receives one request's body piece by piece and reports when it ends.
    #[unsafe(super(NSObject))]
    #[name = "SparkampURLSessionReceiver"]
    #[ivars = Ivars]
    struct Receiver;

    unsafe impl NSObjectProtocol for Receiver {}

    unsafe impl NSURLSessionDelegate for Receiver {}

    unsafe impl NSURLSessionTaskDelegate for Receiver {
        #[unsafe(method(URLSession:task:didCompleteWithError:))]
        fn did_complete(&self, _session: &NSURLSession, task: &NSURLSessionTask, error: Option<&NSError>) {
            let http = task.response().and_then(|r| r.downcast::<NSHTTPURLResponse>().ok());
            let status = http.as_ref().map(|r| r.statusCode() as u16).unwrap_or(0);
            let content_type = http
                .as_ref()
                .and_then(|r| r.valueForHTTPHeaderField(&NSString::from_str("Content-Type")))
                .map(|v| v.to_string().to_ascii_lowercase());
            let error = error
                .map(|e| e.localizedDescription().to_string())
                .or_else(|| self.ivars().write_error.lock().unwrap().take());
            let sink = self.ivars().sink.lock().unwrap().take();
            if let Some(tx) = self.ivars().done.lock().unwrap().take() {
                let _ = tx.send(Finished { status, content_type, error, sink });
            }
        }
    }

    unsafe impl NSURLSessionDataDelegate for Receiver {
        #[unsafe(method(URLSession:dataTask:didReceiveData:))]
        fn did_receive_data(&self, _session: &NSURLSession, _task: &NSURLSessionDataTask, data: &NSData) {
            let piece = data.to_vec();
            let mut sink = self.ivars().sink.lock().unwrap();
            match sink.as_mut() {
                Some(Sink::Memory(body)) => body.extend_from_slice(&piece),
                Some(Sink::File(file, written)) => match file.write_all(&piece) {
                    Ok(()) => *written += piece.len() as u64,
                    Err(e) => {
                        *self.ivars().write_error.lock().unwrap() = Some(format!("cannot write cache file: {e}"));
                    }
                },
                None => {}
            }
        }
    }
);

// SAFETY: the ivars are behind mutexes, and URLSession calls the delegate on
// its own queue; nothing else touches it.
unsafe impl Send for Receiver {}
unsafe impl Sync for Receiver {}

impl Receiver {
    fn new(sink: Sink, done: mpsc::Sender<Finished>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars {
            sink: Mutex::new(Some(sink)),
            write_error: Mutex::new(None),
            done: Mutex::new(Some(done)),
        });
        // SAFETY: NSObject's designated initialiser.
        unsafe { msg_send![super(this), init] }
    }
}

/// Run one GET to completion, the body going to `sink`.
fn fetch(url: &str, timeout_secs: u64, sink: Sink) -> Result<Finished, ServerError> {
    let address = NSURL::URLWithString(&NSString::from_str(url))
        .ok_or_else(|| ServerError::unreachable("not a valid address"))?;
    let request = NSMutableURLRequest::requestWithURL_cachePolicy_timeoutInterval(
        &address,
        NSURLRequestCachePolicy::ReloadIgnoringLocalCacheData,
        timeout_secs as f64,
    );
    request.setValue_forHTTPHeaderField(Some(&NSString::from_str(USER_AGENT)), &NSString::from_str("User-Agent"));
    request.setValue_forHTTPHeaderField(Some(&NSString::from_str("*/*")), &NSString::from_str("Accept"));

    // Ephemeral: no cookies, cache or credentials kept on disk.
    let config = NSURLSessionConfiguration::ephemeralSessionConfiguration();
    // The whole request, as minreq's timeout is; the request's own interval
    // is the longest silence allowed.
    config.setTimeoutIntervalForResource(timeout_secs as f64);
    let (tx, rx) = mpsc::channel();
    let receiver = Receiver::new(sink, tx);
    // SAFETY: the delegate is a thread-safe object that lives as long as the
    // session, which retains it until invalidated below.
    let session = unsafe {
        NSURLSession::sessionWithConfiguration_delegate_delegateQueue(
            &config,
            Some(ProtocolObject::from_ref(&*receiver)),
            None,
        )
    };
    let task = session.dataTaskWithRequest(&request);
    task.resume();
    // Let the task finish, then release the session and its delegate.
    session.finishTasksAndInvalidate();

    let done = rx.recv_timeout(Duration::from_secs(timeout_secs + 10)).map_err(|_| {
        task.cancel();
        ServerError::unreachable("timed out")
    })?;
    if let Some(why) = done.error {
        return Err(ServerError::unreachable(&with_local_network_hint(url, &why)));
    }
    if done.status == 0 {
        return Err(ServerError::unreachable("no answer"));
    }
    Ok(done)
}

/// An unreachable home address, with the Local Network hint.
fn hinted(url: &str, e: ServerError) -> ServerError {
    match e {
        ServerError::Unreachable(why) => ServerError::Unreachable(with_local_network_hint(url, &why)),
        other => other,
    }
}

/// `why`, plus a pointer to macOS's Local Network permission when `url` is
/// a home-network address: a Mac without that permission cannot reach the
/// network it is on, and the error says only that it is offline.
pub fn with_local_network_hint(url: &str, why: &str) -> String {
    if !is_home_network(url) {
        return why.to_string();
    }
    format!(
        "{why} If this Mac is on that network, check that Sparkamp is allowed under \
         System Settings › Privacy & Security › Local Network."
    )
}

/// Whether `url`'s host is on a private network: 10/8, 172.16/12,
/// 192.168/16, 169.254/16, or a `.local` name.
fn is_home_network(url: &str) -> bool {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', ':', '?']).next().unwrap_or("").to_ascii_lowercase();
    if host.ends_with(".local") {
        return true;
    }
    let Ok(ip) = host.parse::<std::net::Ipv4Addr>() else { return false };
    let [a, b, ..] = ip.octets();
    a == 10 || (a == 172 && (16..32).contains(&b)) || (a == 192 && b == 168) || (a == 169 && b == 254)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    /// One connection: hand back the request, answer with `head`, then send
    /// `body` in two halves, the second only once `release` fires.
    fn serve_once(
        head: String,
        body: Vec<u8>,
        release: Option<mpsc::Receiver<()>>,
    ) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
            stream.write_all(head.as_bytes()).unwrap();
            let half = body.len() / 2;
            stream.write_all(&body[..half]).unwrap();
            stream.flush().unwrap();
            if let Some(release) = release {
                let _ = release.recv_timeout(Duration::from_secs(10));
            }
            stream.write_all(&body[half..]).unwrap();
        });
        (format!("http://{addr}"), rx)
    }

    fn head(status: &str, content_type: &str, len: usize) -> String {
        format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n")
    }

    #[test]
    fn a_get_returns_status_and_body_and_says_it_is_sparkamp() {
        let body = b"{\"ok\":true}".to_vec();
        let (base, requests) = serve_once(head("200 OK", "application/json", body.len()), body.clone(), None);
        let resp = AppleTransport::url_session_for_everything().get(&format!("{base}/rest/ping?u=me"), 5).unwrap();
        assert_eq!(resp, HttpResponse { status: 200, body });
        let req = requests.recv().unwrap().to_ascii_lowercase();
        assert!(req.contains(&format!("user-agent: sparkamp/{}", env!("CARGO_PKG_VERSION"))), "{req}");
        assert!(req.contains("accept: */*"), "{req}");
    }

    #[test]
    fn a_refusal_comes_back_as_its_status_for_the_client_to_explain() {
        let body = b"error code: 1010".to_vec();
        let (base, _) = serve_once(head("403 Forbidden", "text/plain", body.len()), body.clone(), None);
        let resp = AppleTransport::url_session_for_everything().get(&format!("{base}/rest/ping"), 5).unwrap();
        assert_eq!(resp, HttpResponse { status: 403, body });
    }

    #[test]
    fn a_download_lands_in_the_file_while_it_is_still_arriving() {
        let audio: Vec<u8> = (0..400_000u32).map(|i| (i % 251) as u8).collect();
        let (release_tx, release_rx) = mpsc::channel();
        let (base, _) = serve_once(head("200 OK", "audio/flac", audio.len()), audio.clone(), Some(release_rx));
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("song.flac.part");
        let watched = dest.clone();
        let watcher = std::thread::spawn(move || {
            // The first half must reach the file before the rest is sent.
            for _ in 0..500 {
                if std::fs::metadata(&watched).map(|m| m.len()).unwrap_or(0) > 0 {
                    let _ = release_tx.send(());
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = release_tx.send(());
            false
        });
        let dl = AppleTransport::url_session_for_everything()
            .get_to_file(&format!("{base}/rest/stream?id=1"), 10, &dest)
            .unwrap();
        assert!(watcher.join().unwrap(), "bytes reached the file before the download finished");
        assert_eq!(dl, Download { status: 200, content_type: Some("audio/flac".into()), bytes: audio.len() as u64 });
        assert_eq!(std::fs::read(&dest).unwrap(), audio);
    }

    #[test]
    fn nothing_listening_is_unreachable_and_the_error_hides_the_query() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let err = AppleTransport::url_session_for_everything()
            .get(&format!("http://127.0.0.1:{port}/rest/ping?u=me&t=secret-token&s=salt"), 3)
            .unwrap_err();
        assert!(err.is_offline(), "{err}");
        assert!(!err.to_string().contains("secret-token"), "{err}");
    }

    /// Reaches a real server behind Cloudflare with no credentials, so only
    /// when asked: `SPARKAMP_LIVE_URL=https://… cargo test --lib live_ -- --ignored`.
    /// Navidrome answering at all, even "missing parameter", means the
    /// request got past Cloudflare.
    #[test]
    #[ignore]
    fn live_https_reaches_navidrome_past_cloudflare() {
        let base = std::env::var("SPARKAMP_LIVE_URL").expect("SPARKAMP_LIVE_URL");
        let resp = AppleTransport::new().get(&format!("{base}/rest/ping.view?v=1.16.1&c=sparkamp&f=json"), 10).unwrap();
        let body = String::from_utf8_lossy(&resp.body);
        assert_eq!(resp.status, 200, "{body:.200}");
        assert!(body.contains("subsonic-response"), "{body:.200}");
    }

    #[test]
    fn only_https_goes_through_url_session_by_default() {
        let t = AppleTransport::new();
        assert!(t.uses_url_session("https://navidrome.example.com/rest/ping"));
        assert!(t.uses_url_session("HTTPS://navidrome.example.com/rest/ping"));
        assert!(!t.uses_url_session("http://192.168.1.137:4533/rest/ping"));
    }

    #[test]
    fn a_home_address_that_fails_points_at_the_local_network_permission() {
        let why = "The Internet connection appears to be offline.";
        for home in ["http://192.168.1.137:4533/rest/ping", "http://10.0.0.5/rest", "http://oscar.local:4533", "http://172.20.1.1"] {
            assert!(with_local_network_hint(home, why).contains("Local Network"), "{home}");
        }
        for away in ["https://navidrome.example.com/rest", "http://8.8.8.8/rest", "http://172.32.0.1"] {
            assert_eq!(with_local_network_hint(away, why), why, "{away}");
        }
    }
}
