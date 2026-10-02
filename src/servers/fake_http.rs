//! A Subsonic server on a local port, for tests that go through real HTTP:
//! its own catalog, playlists and password, a log of what it was asked, and
//! a switch that takes it down. Several run side by side for the
//! multi-server tests.

use crate::servers::auth;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

pub(crate) struct FakeSubsonic {
    pub base: String,
    state: Arc<Mutex<State>>,
}

struct State {
    username: String,
    password: String,
    songs: Vec<Value>,
    /// `(id, name, changed, song ids)`.
    playlists: Vec<(String, String, String, Vec<String>)>,
    last_scan: String,
    down: bool,
    /// `"<endpoint> <query>"` for every request, credentials left out.
    log: Vec<String>,
}

/// A song as `search3` and `getPlaylist` report it.
pub(crate) fn song(id: &str, title: &str, artist: &str, album: &str, path: &str) -> Value {
    json!({
        "id": id, "title": title, "artist": artist, "album": album, "albumArtist": artist,
        "albumId": format!("al-{album}"), "coverArt": format!("al-{album}"),
        "duration": 200, "suffix": "mp3", "size": 4000, "path": path, "track": 1,
    })
}

impl FakeSubsonic {
    /// Start a server that accepts `username` with `password`.
    pub fn start(username: &str, password: &str) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State {
            username: username.into(),
            password: password.into(),
            songs: Vec::new(),
            playlists: Vec::new(),
            last_scan: "2026-10-01T03:00:00Z".into(),
            down: false,
            log: Vec::new(),
        }));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let state = shared.clone();
                std::thread::spawn(move || answer(stream, &state));
            }
        });
        FakeSubsonic { base, state }
    }

    /// Replace the catalog; the server reports a new scan.
    pub fn set_songs(&self, songs: Vec<Value>) {
        let mut s = self.state.lock().unwrap();
        s.songs = songs;
        let hour: u32 = s.last_scan[11..13].parse().unwrap();
        s.last_scan = format!("2026-10-01T{:02}:00:00Z", hour + 1);
    }

    pub fn add_playlist(&self, id: &str, name: &str, changed: &str, song_ids: &[&str]) {
        self.state.lock().unwrap().playlists.push((
            id.into(),
            name.into(),
            changed.into(),
            song_ids.iter().map(|s| s.to_string()).collect(),
        ));
    }

    /// Answer every request with a proxy's 503, as a server that is down.
    pub fn set_down(&self, down: bool) {
        self.state.lock().unwrap().down = down;
    }

    /// The query strings of every request to `endpoint`, in order.
    pub fn calls(&self, endpoint: &str) -> Vec<String> {
        let prefix = format!("{endpoint} ");
        self.state
            .lock()
            .unwrap()
            .log
            .iter()
            .filter_map(|l| l.strip_prefix(&prefix).map(str::to_string))
            .collect()
    }
}

fn answer(mut stream: std::net::TcpStream, state: &Mutex<State>) {
    let mut req = Vec::new();
    let mut buf = [0u8; 4096];
    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => req.extend_from_slice(&buf[..n]),
        }
    }
    let line = String::from_utf8_lossy(&req).lines().next().unwrap_or("").to_string();
    let target = line.split(' ').nth(1).unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let endpoint = path.rsplit('/').next().unwrap_or("").trim_end_matches(".view").to_string();
    let params: Vec<(String, String)> = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), decode(v)))
        .collect();
    let param = |key: &str| params.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

    let (status, ctype, body) = {
        let mut s = state.lock().unwrap();
        let shown: Vec<String> = params
            .iter()
            .filter(|(k, _)| !["t", "s", "p", "v", "c", "f", "apiKey"].contains(&k.as_str()))
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        s.log.push(format!("{endpoint} {}", shown.join("&")));
        if s.down {
            ("503 Service Unavailable", "text/html", b"<html>maintenance</html>".to_vec())
        } else {
            let salt = param("s").unwrap_or_default();
            let signed_in = param("u").as_deref() == Some(s.username.as_str())
                && param("t") == Some(auth::token(&s.password, &salt));
            if !signed_in {
                ("200 OK", "application/json", envelope(Err((40, "Wrong username or password"))))
            } else {
                respond(&s, &endpoint, &param)
            }
        }
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
}

fn respond(
    s: &State,
    endpoint: &str,
    param: &dyn Fn(&str) -> Option<String>,
) -> (&'static str, &'static str, Vec<u8>) {
    let json_ok = |inner: Value| ("200 OK", "application/json", envelope(Ok(inner)));
    let head = |p: &(String, String, String, Vec<String>)| {
        json!({"id": p.0, "name": p.1, "owner": s.username, "public": false,
               "songCount": p.3.len(), "duration": 200 * p.3.len(), "changed": p.2})
    };
    match endpoint {
        "ping" => json_ok(json!({})),
        "getOpenSubsonicExtensions" => json_ok(json!({"openSubsonicExtensions": []})),
        "getScanStatus" => json_ok(json!({"scanStatus": {
            "scanning": false, "count": s.songs.len(), "lastScan": s.last_scan}})),
        "search3" => {
            let offset: usize = param("songOffset").and_then(|v| v.parse().ok()).unwrap_or(0);
            let count: usize = param("songCount").and_then(|v| v.parse().ok()).unwrap_or(20);
            let page: Vec<&Value> = s.songs.iter().skip(offset).take(count).collect();
            json_ok(json!({"searchResult3": {"song": page}}))
        }
        "getPlaylists" => {
            let heads: Vec<Value> = s.playlists.iter().map(head).collect();
            json_ok(json!({"playlists": {"playlist": heads}}))
        }
        "getPlaylist" => {
            let id = param("id").unwrap_or_default();
            let Some(p) = s.playlists.iter().find(|p| p.0 == id) else {
                return ("200 OK", "application/json", envelope(Err((70, "Playlist not found"))));
            };
            let mut h = head(p);
            h["entry"] = p
                .3
                .iter()
                .filter_map(|sid| s.songs.iter().find(|song| song["id"] == sid.as_str()).cloned())
                .collect();
            json_ok(json!({"playlist": h}))
        }
        "getCoverArt" => ("200 OK", "image/png", b"\x89PNG\r\n\x1a\nfake cover".to_vec()),
        "stream" | "download" => ("200 OK", "audio/mpeg", vec![0xffu8; 4000]),
        _ => json_ok(json!({})),
    }
}

fn envelope(result: Result<Value, (u32, &str)>) -> Vec<u8> {
    let mut r = json!({"status": "ok", "version": "1.16.1", "type": "navidrome",
                       "serverVersion": "0.58.0", "openSubsonic": true});
    match result {
        Ok(inner) => r.as_object_mut().unwrap().extend(inner.as_object().unwrap().clone()),
        Err((code, message)) => {
            r["status"] = json!("failed");
            r["error"] = json!({"code": code, "message": message});
        }
    }
    json!({ "subsonic-response": r }).to_string().into_bytes()
}

/// Undo `%XX` escapes and `+` in a query value.
fn decode(v: &str) -> String {
    let bytes = v.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
