//! freedb/CDDB disc identification math.
//!
//! Pure functions from a [`DiscToc`] to the 8-hex disc ID and the `cddb
//! query` argument string. Everything here assumes the TOC frames are
//! **CDDB-absolute** (LBA + 150 — see [`super::TocTrack`]); the detectors
//! guarantee that.

use super::DiscToc;

/// freedb/CDDB disc ID: 8 hex digits from the TOC.
///
/// `XXYYYYZZ` where `XX` = (sum of the digit-sums of each track's start
/// second) mod 255, `YYYY` = total playing seconds (leadout − first track),
/// `ZZ` = track count.
pub fn freedb_discid(toc: &DiscToc) -> String {
    fn digit_sum(mut secs: u32) -> u32 {
        let mut s = 0;
        while secs > 0 {
            s += secs % 10;
            secs /= 10;
        }
        s
    }
    let n: u32 = toc
        .tracks
        .iter()
        .map(|t| digit_sum(t.start_frame / 75))
        .sum();
    let first = toc.tracks.first().map(|t| t.start_frame / 75).unwrap_or(0);
    let last = toc.leadout_frame / 75;
    let total = last.saturating_sub(first);
    format!(
        "{:08x}",
        ((n % 0xff) << 24) | ((total & 0xffff) << 8) | toc.tracks.len() as u32
    )
}

/// The argument string for `cddb query`:
/// `<discid> <ntrks> <off1> … <offn> <nsecs>` — offsets are the CDDB-absolute
/// frame offsets, `nsecs` is the disc length in seconds measured from frame 0
/// (leadout / 75), per the CDDB protocol spec.
pub fn query_args(toc: &DiscToc) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(toc.tracks.len() + 3);
    parts.push(freedb_discid(toc));
    parts.push(toc.tracks.len().to_string());
    parts.extend(toc.tracks.iter().map(|t| t.start_frame.to_string()));
    parts.push((toc.leadout_frame / 75).to_string());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::TocTrack;

    fn toc(starts: &[u32], leadout: u32) -> DiscToc {
        DiscToc {
            tracks: starts
                .iter()
                .enumerate()
                .map(|(i, &s)| TocTrack {
                    number: (i + 1) as u8,
                    start_frame: s,
                    is_audio: true,
                })
                .collect(),
            leadout_frame: leadout,
        }
    }

    /// Hand-computed vector, worked from the published CDDB algorithm:
    /// starts 150 / 15000 / 30000 (seconds 2, 200, 400 → digit sums 2+2+4=8),
    /// leadout 45000 (600 s). total = 600−2 = 598 = 0x256; n%255 = 8.
    /// id = 0x08 << 24 | 0x256 << 8 | 3 = "08025603".
    #[test]
    fn discid_matches_hand_computed_vector() {
        let t = toc(&[150, 15000, 30000], 45000);
        assert_eq!(freedb_discid(&t), "08025603");
    }

    /// The real 8-track test disc (values live-read from its .TOC.plist).
    /// Seconds: 2,184,402,591,794,981,1294,1479 → digit sums
    /// 2+13+6+15+20+18+16+21 = 111; total 1663−2 = 1661 = 0x67d;
    /// id = 111<<24 | 1661<<8 | 8 = "6f067d08".
    #[test]
    fn discid_matches_real_disc() {
        let t = toc(
            &[150, 13834, 30216, 44337, 59560, 73612, 97120, 110977],
            124766,
        );
        assert_eq!(freedb_discid(&t), "6f067d08");
    }

    #[test]
    fn discid_empty_toc_is_zero_tracks() {
        let t = toc(&[], 4500);
        // No tracks: n = 0, first defaults to 0, total = 60.
        assert_eq!(freedb_discid(&t), "00003c00");
    }

    #[test]
    fn query_args_shape() {
        let t = toc(&[150, 15000, 30000], 45000);
        assert_eq!(query_args(&t), "08025603 3 150 15000 30000 600");
    }
}
