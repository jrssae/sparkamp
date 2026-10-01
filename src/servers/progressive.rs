//! Playing a server song while it is still downloading.
//!
//! The playback cache downloads a song to a `.part` file and renames it when
//! it is complete. [`Partial`] is that download seen from the player's side:
//! where the bytes are going, how to stream the same song straight from the
//! server, and whether the download has finished.
//!
//! The two engines use it differently. GStreamer streams the server URL
//! itself and decodes whatever it gets. AVFoundation cannot read a stream or
//! a file that is still growing, so on macOS [`PcmStream`] decodes the growing
//! `.part` with symphonia on its own thread and hands the engine PCM, the way
//! the CD player hands it audio straight off the drive. A [`GrowingReader`]
//! under it waits for bytes that have not arrived yet, so the decoder never
//! mistakes "not downloaded yet" for the end of the file.
//!
//! Before any of that, a probe works out the format in the background, so the
//! UI thread never waits on the network: until it has, the song reads as
//! still downloading. A format symphonia cannot decode (Opus), or one whose
//! index sits at the end of the file, is simply played once the whole file is
//! there.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// How often a reader that has caught up with the download looks again.
const POLL: Duration = Duration::from_millis(20);

/// Decoded chunks kept ready ahead of the engine. Bounded so a fast decoder
/// does not hold a whole song in memory.
const CHUNKS_AHEAD: usize = 16;

/// Where a download stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadState {
    Running,
    Done,
    Failed(String),
}

/// What the probe found: enough to set up the engine before any audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamInfo {
    pub sample_rate: u32,
    pub channels: u16,
    /// Length in frames when the file says so (FLAC, WAV, MP3 with a Xing
    /// header); otherwise estimated from the server's duration, if any.
    pub frames: Option<u64>,
}

/// A download in progress, as the player sees it.
pub struct Partial {
    part: PathBuf,
    done_path: PathBuf,
    url: String,
    ext: Option<String>,
    duration_hint: Option<f64>,
    state: Mutex<DownloadState>,
    info: OnceLock<Option<StreamInfo>>,
}

impl Partial {
    /// A download of `url` into `part`, renamed to `done_path` when complete.
    /// `duration_hint` is the server's length in seconds, for formats that do
    /// not state their own.
    pub fn new(part: PathBuf, done_path: PathBuf, url: String, duration_hint: Option<f64>) -> Arc<Self> {
        let ext = done_path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        Arc::new(Partial {
            part,
            done_path,
            url,
            ext,
            duration_hint,
            state: Mutex::new(DownloadState::Running),
            info: OnceLock::new(),
        })
    }

    /// The server URL of the same song, for an engine that streams.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn state(&self) -> DownloadState {
        self.state.lock().unwrap().clone()
    }

    /// Record how the download ended.
    pub fn finish(&self, outcome: Result<(), String>) {
        *self.state.lock().unwrap() = match outcome {
            Ok(()) => DownloadState::Done,
            Err(why) => DownloadState::Failed(why),
        };
    }

    /// The format, once the probe has run: `Some(Some(info))` streamable,
    /// `Some(None)` not (wait for the whole file), `None` not known yet.
    pub fn info(&self) -> Option<Option<StreamInfo>> {
        self.info.get().copied()
    }

    /// Work out the format on a background thread. Safe to call more than
    /// once; only the first call probes.
    pub fn probe_in_background(self: &Arc<Self>) {
        static STARTED: Mutex<Vec<usize>> = Mutex::new(Vec::new());
        let key = Arc::as_ptr(self) as usize;
        {
            let mut started = STARTED.lock().unwrap();
            if started.contains(&key) || self.info.get().is_some() {
                return;
            }
            started.push(key);
        }
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            let found = Decoding::open(&me, Arc::new(AtomicBool::new(false))).ok().map(|d| d.info);
            let _ = me.info.set(found);
            STARTED.lock().unwrap().retain(|k| *k != key);
        });
    }

    /// Probe on the calling thread. Blocks until enough of the file is there.
    #[cfg(test)]
    pub(crate) fn probe_now(self: &Arc<Self>) -> Option<StreamInfo> {
        let found = Decoding::open(self, Arc::new(AtomicBool::new(false))).ok().map(|d| d.info);
        let _ = self.info.set(found);
        found
    }
}

/// Without the URL's query: it carries the sign-in token.
impl std::fmt::Debug for Partial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Partial")
            .field("part", &self.part)
            .field("url", &self.url.split('?').next().unwrap_or_default())
            .field("state", &self.state())
            .field("info", &self.info())
            .finish()
    }
}

/// Two handles are the same download only if they are the same object.
impl PartialEq for Partial {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for Partial {}

/// Reads a file that is still being written, as if it were complete.
///
/// A read past what has arrived waits for more instead of reporting the end;
/// the end is reported only once the download is done. A failed download
/// reads as an error once its bytes run out. `cancel` stops any wait.
pub struct GrowingReader {
    partial: Arc<Partial>,
    file: File,
    pos: u64,
    cancel: Arc<AtomicBool>,
}

impl GrowingReader {
    /// Open the download, waiting for its file to appear.
    pub fn open(partial: &Arc<Partial>, cancel: Arc<AtomicBool>) -> io::Result<Self> {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            // The state is read before trying the paths: a download that
            // finishes in between has already renamed its file.
            let state = partial.state();
            for path in [&partial.part, &partial.done_path] {
                if let Ok(file) = File::open(path) {
                    return Ok(GrowingReader { partial: Arc::clone(partial), file, pos: 0, cancel });
                }
            }
            if let DownloadState::Failed(why) = state {
                return Err(io::Error::other(why));
            }
            std::thread::sleep(POLL);
        }
    }

    fn arrived(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    /// Wait until byte `at` has arrived or no more will. Returns whether it
    /// is there.
    fn wait_for(&self, at: u64) -> io::Result<bool> {
        loop {
            // State first, then length: a download that is done has written
            // everything it ever will.
            let state = self.partial.state();
            if self.arrived()? > at {
                return Ok(true);
            }
            match state {
                DownloadState::Done => return Ok(false),
                DownloadState::Failed(why) => return Err(io::Error::other(why)),
                DownloadState::Running => {}
            }
            if self.cancel.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            std::thread::sleep(POLL);
        }
    }
}

impl Read for GrowingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || !self.wait_for(self.pos)? {
            return Ok(0);
        }
        let available = (self.arrived()? - self.pos).min(buf.len() as u64) as usize;
        self.file.seek(SeekFrom::Start(self.pos))?;
        let n = self.file.read(&mut buf[..available])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for GrowingReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.pos = match to {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(d) => self.pos.checked_add_signed(d).ok_or(io::ErrorKind::InvalidInput)?,
            SeekFrom::End(d) => {
                // The end is only known once everything has arrived.
                while self.partial.state() == DownloadState::Running {
                    if self.cancel.load(Ordering::Relaxed) {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                    }
                    std::thread::sleep(POLL);
                }
                self.arrived()?.checked_add_signed(d).ok_or(io::ErrorKind::InvalidInput)?
            }
        };
        Ok(self.pos)
    }
}

impl symphonia::core::io::MediaSource for GrowingReader {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        match self.partial.state() {
            DownloadState::Done => self.arrived().ok(),
            _ => None,
        }
    }
}

/// An open symphonia decoder over a download.
struct Decoding {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    info: StreamInfo,
}

impl Decoding {
    fn open(partial: &Arc<Partial>, cancel: Arc<AtomicBool>) -> Result<Self, String> {
        use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::probe::Hint;

        let reader = GrowingReader::open(partial, cancel).map_err(|e| e.to_string())?;
        let stream = MediaSourceStream::new(Box::new(reader), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = &partial.ext {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
            .map_err(|e| e.to_string())?;
        let format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or("no audio track")?;
        let params = track.codec_params.clone();
        let sample_rate = params.sample_rate.ok_or("no sample rate")?;
        let channels = params.channels.map(|c| c.count() as u16).ok_or("no channel layout")?;
        let decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .map_err(|e| e.to_string())?;
        let frames = params
            .n_frames
            .or_else(|| partial.duration_hint.map(|secs| (secs * sample_rate as f64).round() as u64));
        Ok(Decoding {
            track_id: track.id,
            format,
            decoder,
            info: StreamInfo { sample_rate, channels, frames },
        })
    }
}

/// Decoded audio from a download, produced on its own thread.
///
/// Chunks are interleaved `f32` samples at the stream's sample rate and
/// channel count. Dropping it stops the thread.
pub struct PcmStream {
    info: StreamInfo,
    chunks: Receiver<Result<Vec<f32>, String>>,
    finished: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

impl PcmStream {
    /// Start decoding `partial` from frame `from`. Needs the probe to have
    /// found the format; `None` otherwise.
    pub fn start(partial: &Arc<Partial>, from: u64) -> Option<Self> {
        let info = partial.info().flatten()?;
        let (tx, rx) = std::sync::mpsc::sync_channel(CHUNKS_AHEAD);
        let finished = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        let (p, f, c) = (Arc::clone(partial), Arc::clone(&finished), Arc::clone(&cancel));
        std::thread::spawn(move || {
            decode_into(&p, from, &tx, &c);
            f.store(true, Ordering::Release);
        });
        Some(PcmStream { info, chunks: rx, finished, cancel })
    }

    pub fn info(&self) -> StreamInfo {
        self.info
    }

    /// The next decoded chunk if one is ready: `None` when nothing is ready
    /// yet or nothing more will come (see [`Self::is_finished`]).
    pub fn try_next(&self) -> Option<Result<Vec<f32>, String>> {
        match self.chunks.try_recv() {
            Ok(chunk) => Some(chunk),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    /// Whether the decoder has stopped and every chunk has been taken.
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
            && matches!(self.chunks.try_recv(), Err(TryRecvError::Disconnected))
    }

    /// Wait for the next chunk, for tests and offline use.
    #[cfg(test)]
    pub fn next_blocking(&self, timeout: Duration) -> Option<Result<Vec<f32>, String>> {
        self.chunks.recv_timeout(timeout).ok()
    }
}

impl Drop for PcmStream {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The decoder thread: open, seek to `from`, decode until the end, a fatal
/// error, or cancellation.
fn decode_into(partial: &Arc<Partial>, from: u64, tx: &SyncSender<Result<Vec<f32>, String>>, cancel: &Arc<AtomicBool>) {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::{SeekMode, SeekTo};
    use symphonia::core::units::Time;

    let mut d = match Decoding::open(partial, Arc::clone(cancel)) {
        Ok(d) => d,
        Err(why) => {
            let _ = tx.send(Err(why));
            return;
        }
    };
    let channels = d.info.channels as usize;
    let rate = d.info.sample_rate as f64;
    // Frames still to drop before `from`: a seek lands on the packet that
    // holds it, not on the frame itself.
    let mut skip: u64 = 0;
    if from > 0 {
        let to = SeekTo::Time { time: Time::from(from as f64 / rate), track_id: Some(d.track_id) };
        match d.format.seek(SeekMode::Accurate, to) {
            Ok(seeked) => skip = seeked.required_ts.saturating_sub(seeked.actual_ts),
            Err(e) => {
                let _ = tx.send(Err(e.to_string()));
                return;
            }
        }
    }
    let mut samples: Option<SampleBuffer<f32>> = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let packet = match d.format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => return,
            Err(e) => {
                if !cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(Err(e.to_string()));
                }
                return;
            }
        };
        if packet.track_id() != d.track_id {
            continue;
        }
        let decoded = match d.decoder.decode(&packet) {
            Ok(buf) => buf,
            // A damaged packet is skipped, as players do.
            Err(Error::DecodeError(_)) => continue,
            Err(e) => {
                let _ = tx.send(Err(e.to_string()));
                return;
            }
        };
        let spec = *decoded.spec();
        let buf = samples.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
        if buf.capacity() < decoded.capacity() * channels {
            *buf = SampleBuffer::new(decoded.capacity() as u64, spec);
        }
        buf.copy_interleaved_ref(decoded);
        let mut chunk = buf.samples();
        if skip > 0 {
            let frames = (chunk.len() / channels) as u64;
            let dropped = skip.min(frames);
            skip -= dropped;
            chunk = &chunk[dropped as usize * channels..];
        }
        if chunk.is_empty() {
            continue;
        }
        if tx.send(Ok(chunk.to_vec())).is_err() {
            return;
        }
    }
}

/// The `.part` file a download into `dest` writes first.
pub fn part_path(dest: &Path) -> PathBuf {
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    PathBuf::from(part)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A mono 16-bit WAV of `frames` frames of a ramp, so every sample is
    /// different and a skipped or repeated frame would show.
    fn wav(frames: u32, rate: u32) -> Vec<u8> {
        let data = frames * 2;
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * 2).to_le_bytes());
        w.extend_from_slice(&2u16.to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data.to_le_bytes());
        for i in 0..frames {
            w.extend_from_slice(&((i % 30_000) as i16).to_le_bytes());
        }
        w
    }

    /// A download in `dir` and a writer that appends to its `.part`.
    fn download(dir: &Path, name: &str) -> (Arc<Partial>, PathBuf) {
        let done = dir.join(name);
        let part = part_path(&done);
        (Partial::new(part.clone(), done, "http://server/rest/stream?id=1".into(), None), part)
    }

    fn append(path: &Path, bytes: &[u8]) {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        f.write_all(bytes).unwrap();
    }

    #[test]
    fn debug_output_leaves_out_the_sign_in_token() {
        let p = Partial::new("a.part".into(), "a".into(), "http://s/rest/stream?u=me&t=secret&s=salt".into(), None);
        let shown = format!("{p:?}");
        assert!(!shown.contains("secret") && shown.contains("http://s/rest/stream"), "{shown}");
    }

    #[test]
    fn a_reader_waits_for_bytes_that_have_not_arrived_and_ends_only_when_the_download_does() {
        let dir = tempfile::tempdir().unwrap();
        let (p, part) = download(dir.path(), "a.mp3");
        append(&part, b"hello ");
        let writer = {
            let (p, part) = (Arc::clone(&p), part.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                append(&part, b"world");
                p.finish(Ok(()));
            })
        };
        let mut r = GrowingReader::open(&p, Arc::new(AtomicBool::new(false))).unwrap();
        let mut all = Vec::new();
        r.read_to_end(&mut all).unwrap();
        writer.join().unwrap();
        assert_eq!(all, b"hello world");
    }

    #[test]
    fn a_failed_download_reads_as_an_error_once_its_bytes_run_out() {
        let dir = tempfile::tempdir().unwrap();
        let (p, part) = download(dir.path(), "a.mp3");
        append(&part, b"abc");
        p.finish(Err("server went away".into()));
        let mut r = GrowingReader::open(&p, Arc::new(AtomicBool::new(false))).unwrap();
        let mut buf = [0u8; 3];
        r.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"abc");
        let err = r.read(&mut buf).unwrap_err();
        assert!(err.to_string().contains("server went away"), "{err}");
    }

    #[test]
    fn a_reader_waits_for_the_file_and_finds_it_after_the_rename() {
        let dir = tempfile::tempdir().unwrap();
        let (p, _part) = download(dir.path(), "a.mp3");
        std::fs::write(dir.path().join("a.mp3"), b"complete").unwrap();
        p.finish(Ok(()));
        let mut r = GrowingReader::open(&p, Arc::new(AtomicBool::new(false))).unwrap();
        let mut all = String::new();
        r.read_to_string(&mut all).unwrap();
        assert_eq!(all, "complete");
    }

    #[test]
    fn cancelling_stops_a_waiting_reader() {
        let dir = tempfile::tempdir().unwrap();
        let (p, part) = download(dir.path(), "a.mp3");
        append(&part, b"x");
        let cancel = Arc::new(AtomicBool::new(false));
        let mut r = GrowingReader::open(&p, Arc::clone(&cancel)).unwrap();
        let mut one = [0u8; 1];
        r.read_exact(&mut one).unwrap();
        let c = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            c.store(true, Ordering::Relaxed);
        });
        let started = std::time::Instant::now();
        assert_eq!(r.read(&mut one).unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn the_probe_finds_the_format_from_the_first_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let (p, part) = download(dir.path(), "a.wav");
        let bytes = wav(44_100, 44_100);
        append(&part, &bytes[..4096]);
        // Not finished: the probe must not need the whole file.
        let info = p.probe_now().expect("a WAV is streamable");
        assert_eq!(info, StreamInfo { sample_rate: 44_100, channels: 1, frames: Some(44_100) });
        assert_eq!(p.info(), Some(Some(info)));
    }

    #[test]
    fn data_symphonia_cannot_read_is_not_streamable() {
        let dir = tempfile::tempdir().unwrap();
        let (p, part) = download(dir.path(), "a.opus");
        append(&part, &[0x55u8; 8192]);
        p.finish(Ok(()));
        assert_eq!(p.probe_now(), None);
        assert_eq!(p.info(), Some(None));
    }

    #[test]
    fn audio_arrives_before_the_download_is_complete() {
        let dir = tempfile::tempdir().unwrap();
        let (p, part) = download(dir.path(), "a.wav");
        let bytes = wav(88_200, 44_100);
        let half = bytes.len() / 2;
        append(&part, &bytes[..half]);
        p.probe_now().unwrap();
        let stream = PcmStream::start(&p, 0).unwrap();
        let first = stream.next_blocking(Duration::from_secs(5)).expect("audio before the rest").unwrap();
        assert_eq!(p.state(), DownloadState::Running, "still downloading");
        assert_eq!(first[0], 0.0);
        append(&part, &bytes[half..]);
        p.finish(Ok(()));
        let mut frames = first.len();
        while let Some(chunk) = stream.next_blocking(Duration::from_secs(5)) {
            frames += chunk.unwrap().len();
        }
        assert_eq!(frames, 88_200, "every frame, once");
        assert!(stream.is_finished());
    }

    #[test]
    fn decoding_can_start_part_way_through() {
        let dir = tempfile::tempdir().unwrap();
        let (p, part) = download(dir.path(), "a.wav");
        append(&part, &wav(44_100, 44_100));
        p.finish(Ok(()));
        p.probe_now().unwrap();
        let stream = PcmStream::start(&p, 22_050).unwrap();
        let mut samples = Vec::new();
        while let Some(chunk) = stream.next_blocking(Duration::from_secs(5)) {
            samples.extend(chunk.unwrap());
        }
        assert_eq!(samples.len(), 22_050);
        // The ramp's value at frame 22_050, as the decoder scales 16-bit PCM.
        assert!((samples[0] - 22_050.0 / 32_768.0).abs() < 1e-6, "{}", samples[0]);
    }
}
