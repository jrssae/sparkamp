//! ISO 9660 + Joliet image writer, for macOS data burns.
//!
//! # Why this exists
//!
//! DiscRecording will not produce an ISO 9660 filesystem. Measured on
//! 2026-09-04 against a Slimtype DS8A5SH: a data burn yields an Apple
//! Partition Map holding an HFS+ volume and nothing else, whether the burn
//! root is a real folder or a virtual one, and under every filesystem mask
//! including one naming ISO 9660 explicitly. The mask reaches the engine (it
//! moved filesystem overhead from 547 blocks to 265) but only ever removes
//! filesystems; it cannot add one the engine is not generating. A byte scan of
//! every sector the drive reported written found no primary volume descriptor
//! at sector 16, which is where a hybrid disc puts one.
//!
//! `hdiutil makehybrid` would do this in one command and is unavailable: the
//! App Sandbox forbids subprocesses, which is already why eject moved into the
//! core.
//!
//! So Sparkamp builds the image itself and burns it as a raw data track. Linux
//! reaches the same result through `xorriso -joliet on`, and the point of both
//! is the same: a data CD of MP3s is most often played by a car stereo or a
//! DVD player, and those read ISO 9660 and Joliet, not HFS+.
//!
//! # Scope
//!
//! One flat directory, which is all a burn ever needs: files are staged into a
//! single temp directory so the disc root is flat and predictable. No
//! subdirectories, no Rock Ridge, no El Torito boot records. Restricting the
//! scope is what keeps this auditable.
//!
//! Two name spaces over one set of file extents. The primary descriptor names
//! files in ISO 9660 level 2 (uppercase, one dot, a `;1` version suffix); the
//! Joliet supplementary descriptor names the same extents in UCS-2, so a
//! reader that understands Joliet sees the real filename and one that does not
//! still finds the file.

use std::path::{Path, PathBuf};

/// One file to place in the image root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsoEntry {
    /// The name as it should appear to a Joliet reader.
    pub name: String,
    /// Where the bytes are read from.
    pub path: PathBuf,
    /// Size in bytes. Taken up front so the layout can be planned without
    /// reading any file twice.
    pub size: u64,
}

/// A logical block. ISO 9660 allows others; every CD and DVD uses 2048.
pub const SECTOR: u64 = 2048;

/// Sectors reserved before the volume descriptors. A hybrid disc puts an Apple
/// Partition Map here, which is exactly why the two can coexist.
const SYSTEM_AREA_SECTORS: u64 = 16;

fn sectors_for(bytes: u64) -> u64 {
    bytes.div_ceil(SECTOR)
}

/// A 32-bit value in both byte orders, which ISO 9660 requires for most
/// numeric fields so a reader of either endianness can use the near half.
fn both32(v: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&v.to_le_bytes());
    out[4..].copy_from_slice(&v.to_be_bytes());
    out
}

/// A 16-bit value in both byte orders.
fn both16(v: u16) -> [u8; 4] {
    let mut out = [0u8; 4];
    out[..2].copy_from_slice(&v.to_le_bytes());
    out[2..].copy_from_slice(&v.to_be_bytes());
    out
}

/// An ISO 9660 level 2 file name: up to 30 characters from A-Z, 0-9 and `_`,
/// at most one `.`, and a `;1` version suffix.
///
/// Anything outside the set becomes `_` rather than being dropped, so two
/// different names cannot silently collapse into one on the character rule
/// alone. Collisions that survive that are resolved by [`plan`].
pub fn iso_name(name: &str) -> String {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, e),
        _ => (name, ""),
    };
    let clean = |s: &str, max: usize| -> String {
        s.chars()
            .map(|c| {
                let u = c.to_ascii_uppercase();
                if u.is_ascii_uppercase() || u.is_ascii_digit() || u == '_' {
                    u
                } else {
                    '_'
                }
            })
            .take(max)
            .collect()
    };
    let ext = clean(ext, 3);
    // 30 characters total across stem and extension, leaving room for the dot.
    let stem_max = if ext.is_empty() { 30 } else { 29 - ext.len() };
    let stem = clean(stem, stem_max.max(1));
    let stem = if stem.is_empty() { "_".to_string() } else { stem };
    if ext.is_empty() {
        format!("{stem};1")
    } else {
        format!("{stem}.{ext};1")
    }
}

/// UCS-2 big endian, which is how Joliet stores a name.
fn ucs2(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    // Joliet allows 64 UCS-2 characters. Encoding to UTF-16 first means a
    // character outside the basic plane costs its two surrogates, which is
    // what a Joliet reader counts too.
    for unit in name.encode_utf16().take(64) {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    out
}

/// Where everything sits in the finished image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub volume_label: String,
    /// Entries in the order they appear in both directories, with the ISO
    /// 9660 name resolved and the extent assigned.
    pub files: Vec<PlacedFile>,
    pub path_table_l: u64,
    pub path_table_m: u64,
    pub joliet_path_table_l: u64,
    pub joliet_path_table_m: u64,
    pub root_dir: u64,
    pub root_dir_sectors: u64,
    pub joliet_root_dir: u64,
    pub joliet_root_dir_sectors: u64,
    /// Total image size in sectors.
    pub total_sectors: u64,
}

/// One file, placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedFile {
    pub entry: IsoEntry,
    /// The name a non-Joliet reader sees, `;1` suffix included.
    pub iso_name: String,
    /// First logical block of the file's data.
    pub extent: u64,
}

/// Assign names and extents. Pure, so the whole layout is testable without
/// touching a disc or writing a byte.
///
/// Ordering matters and is not cosmetic: ISO 9660 requires directory records
/// sorted by identifier, and a reader is entitled to binary-search them.
pub fn plan(entries: &[IsoEntry], volume_label: &str) -> Layout {
    // Resolve ISO name collisions before sorting, because the sort key is the
    // resolved name. Two files differing only in characters the level 2 set
    // cannot represent ("a-1.mp3" and "a_1.mp3") would otherwise become one.
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut named: Vec<(String, IsoEntry)> = entries
        .iter()
        .map(|e| (resolve_collision(&iso_name(&e.name), &mut taken), e.clone()))
        .collect();
    named.sort_by(|a, b| a.0.cmp(&b.0));

    let root_bytes = directory_bytes(&named, false);
    let joliet_root_bytes = directory_bytes(&named, true);
    let root_dir_sectors = sectors_for(root_bytes).max(1);
    let joliet_root_dir_sectors = sectors_for(joliet_root_bytes).max(1);

    // Fixed prologue: system area, PVD, Joliet SVD, terminator.
    let mut lba = SYSTEM_AREA_SECTORS + 3;
    let path_table_l = lba;
    lba += 1;
    let path_table_m = lba;
    lba += 1;
    let joliet_path_table_l = lba;
    lba += 1;
    let joliet_path_table_m = lba;
    lba += 1;
    let root_dir = lba;
    lba += root_dir_sectors;
    let joliet_root_dir = lba;
    lba += joliet_root_dir_sectors;

    let mut files = Vec::with_capacity(named.len());
    for (iso_name, entry) in named {
        let extent = lba;
        // A zero-length file still needs a valid extent, so it gets a sector.
        lba += sectors_for(entry.size).max(1);
        files.push(PlacedFile {
            entry,
            iso_name,
            extent,
        });
    }

    Layout {
        volume_label: volume_label.to_string(),
        files,
        path_table_l,
        path_table_m,
        joliet_path_table_l,
        joliet_path_table_m,
        root_dir,
        root_dir_sectors,
        joliet_root_dir,
        joliet_root_dir_sectors,
        total_sectors: lba,
    }
}

/// Give a folded name one nobody else has, by appending `_1`, `_2` and so on
/// to the stem.
///
/// The first file to claim a name keeps it unsuffixed, so three files called
/// `a-1.mp3` become `A_1.MP3`, `A_1_1.MP3` and `A_1_2.MP3`. The suffix goes on
/// the stem rather than the end, because the extension is what a reader
/// dispatches on and `A_1.MP3_1` would not play anywhere.
///
/// The loop re-checks each candidate rather than trusting a counter, since a
/// generated name can collide with a real one: a burn list holding `a-1.mp3`,
/// `a+1.mp3` and a genuine `a_1_1.mp3` would otherwise produce two
/// `A_1_1.MP3;1` records.
fn resolve_collision(base: &str, taken: &mut std::collections::HashSet<String>) -> String {
    if taken.insert(base.to_string()) {
        return base.to_string();
    }
    let body = base.strip_suffix(";1").unwrap_or(base);
    let (stem, ext) = match body.rsplit_once('.') {
        Some((s, x)) => (s, format!(".{x}")),
        None => (body, String::new()),
    };
    for n in 1u32.. {
        let tag = format!("_{n}");
        // 30 characters is the level 2 ceiling for stem plus extension, so
        // the stem yields room to the tag rather than the name overflowing.
        let keep = stem.len().min(30usize.saturating_sub(tag.len() + ext.len()));
        let candidate = format!("{}{}{}{};1", &stem[..keep], tag, ext, "");
        if taken.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("u32 exhausted resolving a filename collision")
}

/// A directory record's on-disc length for a given identifier length. The
/// fixed part is 33 bytes and the whole record is padded to even.
fn record_len(id_len: usize) -> usize {
    let n = 33 + id_len;
    n + (n & 1)
}

/// Total bytes the root directory occupies, including its `.` and `..`
/// records. Used by [`plan`] before any extent is known, so it takes names
/// rather than placed files.
fn directory_bytes(named: &[(String, IsoEntry)], joliet: bool) -> u64 {
    // "." and ".." both have a one-byte identifier.
    let mut total = (record_len(1) * 2) as u64;
    for (iso, entry) in named {
        let id_len = if joliet {
            ucs2(&entry.name).len()
        } else {
            iso.len()
        };
        total += record_len(id_len) as u64;
    }
    total
}

/// Civil date from a Unix timestamp, days-from-epoch algorithm.
///
/// Written out rather than pulled in: the only thing the image needs a clock
/// for is a volume timestamp, and a date library is a poor trade for that.
fn civil(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (
        y as i32,
        m,
        d,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    )
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The 7-byte form used inside a directory record.
fn dir_datetime(secs: i64) -> [u8; 7] {
    let (y, m, d, hh, mm, ss) = civil(secs);
    [
        (y - 1900).clamp(0, 255) as u8,
        m as u8,
        d as u8,
        hh as u8,
        mm as u8,
        ss as u8,
        0, // UTC
    ]
}

/// The 17-byte form used in a volume descriptor.
fn vol_datetime(secs: i64) -> [u8; 17] {
    let (y, m, d, hh, mm, ss) = civil(secs);
    let s = format!("{y:04}{m:02}{d:02}{hh:02}{mm:02}{ss:02}00");
    let mut out = [0u8; 17];
    out[..16].copy_from_slice(&s.as_bytes()[..16]);
    out[16] = 0; // UTC
    out
}

/// One directory record.
///
/// `flags` is 0x02 for a directory and 0 for a file.
fn dir_record(id: &[u8], extent: u64, length: u64, flags: u8, secs: i64) -> Vec<u8> {
    let len = record_len(id.len());
    let mut r = vec![0u8; len];
    r[0] = len as u8;
    r[2..10].copy_from_slice(&both32(extent as u32));
    r[10..18].copy_from_slice(&both32(length as u32));
    r[18..25].copy_from_slice(&dir_datetime(secs));
    r[25] = flags;
    r[28..32].copy_from_slice(&both16(1)); // volume sequence number
    r[32] = id.len() as u8;
    r[33..33 + id.len()].copy_from_slice(id);
    r
}

/// The root directory's contents: `.`, `..`, then every file in name order.
fn directory(layout: &Layout, joliet: bool, secs: i64) -> Vec<u8> {
    let (self_lba, self_len) = if joliet {
        (
            layout.joliet_root_dir,
            layout.joliet_root_dir_sectors * SECTOR,
        )
    } else {
        (layout.root_dir, layout.root_dir_sectors * SECTOR)
    };
    let mut out = Vec::new();
    // "." and ".." both point at the root: there is no parent above it.
    out.extend_from_slice(&dir_record(&[0x00], self_lba, self_len, 0x02, secs));
    out.extend_from_slice(&dir_record(&[0x01], self_lba, self_len, 0x02, secs));
    for f in &layout.files {
        let id = if joliet {
            ucs2(&f.entry.name)
        } else {
            f.iso_name.as_bytes().to_vec()
        };
        out.extend_from_slice(&dir_record(&id, f.extent, f.entry.size, 0x00, secs));
    }
    out.resize((self_len) as usize, 0);
    out
}

/// A path table holding the single root entry. `msb` selects the byte order
/// of the extent field, which is the only difference between the L and M
/// tables.
fn path_table(root_lba: u64, msb: bool) -> Vec<u8> {
    let mut t = vec![0u8; 10];
    t[0] = 1; // identifier length
    t[1] = 0; // extended attribute length
    let lba = root_lba as u32;
    t[2..6].copy_from_slice(&if msb {
        lba.to_be_bytes()
    } else {
        lba.to_le_bytes()
    });
    let parent: u16 = 1;
    t[6..8].copy_from_slice(&if msb {
        parent.to_be_bytes()
    } else {
        parent.to_le_bytes()
    });
    t[8] = 0; // the root's identifier is a single zero byte
    t
}

/// The size a path table occupies, which both descriptors must agree on.
const PATH_TABLE_BYTES: u32 = 10;

/// Write an `a`-space-padded field.
fn strfield(dst: &mut [u8], s: &str) {
    dst.fill(b' ');
    for (d, b) in dst.iter_mut().zip(s.bytes()) {
        *d = b;
    }
}

/// Write a UCS-2 field, padded with UCS-2 spaces, as Joliet requires.
fn ucs2field(dst: &mut [u8], s: &str) {
    for pair in dst.chunks_exact_mut(2) {
        pair.copy_from_slice(&[0x00, 0x20]);
    }
    let enc = ucs2(s);
    let n = enc.len().min(dst.len());
    dst[..n].copy_from_slice(&enc[..n]);
}

/// A primary (`joliet == false`) or supplementary (`joliet == true`) volume
/// descriptor, one sector wide.
fn volume_descriptor(layout: &Layout, joliet: bool, secs: i64) -> Vec<u8> {
    let mut s = vec![0u8; SECTOR as usize];
    s[0] = if joliet { 2 } else { 1 };
    s[1..6].copy_from_slice(b"CD001");
    s[6] = 1; // descriptor version

    if joliet {
        // Escape sequence for UCS-2 level 3, which is what marks this
        // supplementary descriptor as Joliet rather than something else.
        s[88..91].copy_from_slice(b"%/E");
        ucs2field(&mut s[8..40], "");
        ucs2field(&mut s[40..72], &layout.volume_label);
    } else {
        strfield(&mut s[8..40], "");
        strfield(&mut s[40..72], &layout.volume_label);
    }

    s[80..88].copy_from_slice(&both32(layout.total_sectors as u32));
    s[120..124].copy_from_slice(&both16(1)); // volume set size
    s[124..128].copy_from_slice(&both16(1)); // volume sequence number
    s[128..132].copy_from_slice(&both16(SECTOR as u16));
    s[132..140].copy_from_slice(&both32(PATH_TABLE_BYTES));

    let (l, m, root, root_len) = if joliet {
        (
            layout.joliet_path_table_l,
            layout.joliet_path_table_m,
            layout.joliet_root_dir,
            layout.joliet_root_dir_sectors * SECTOR,
        )
    } else {
        (
            layout.path_table_l,
            layout.path_table_m,
            layout.root_dir,
            layout.root_dir_sectors * SECTOR,
        )
    };
    s[140..144].copy_from_slice(&(l as u32).to_le_bytes());
    s[148..152].copy_from_slice(&(m as u32).to_be_bytes());

    // The root directory record lives inside the descriptor, fixed at 34
    // bytes: a one-byte identifier padded to even.
    let root_rec = dir_record(&[0x00], root, root_len, 0x02, secs);
    s[156..156 + root_rec.len()].copy_from_slice(&root_rec);

    let text = |dst: &mut [u8], v: &str| {
        if joliet {
            ucs2field(dst, v)
        } else {
            strfield(dst, v)
        }
    };
    text(&mut s[190..318], ""); // volume set
    text(&mut s[318..446], "SPARKAMP");
    text(&mut s[446..574], "SPARKAMP");
    text(&mut s[574..702], "SPARKAMP");
    text(&mut s[702..739], "");
    text(&mut s[739..776], "");
    text(&mut s[776..813], "");

    let stamp = vol_datetime(secs);
    s[813..830].copy_from_slice(&stamp);
    s[830..847].copy_from_slice(&stamp);
    // Expiration and effective dates stay all-zero, which means "unset"
    // rather than "already expired": a field of ASCII '0' would be a date.
    s[847..864].fill(b'0');
    s[864..881].fill(b'0');
    s[881] = 1; // file structure version
    s
}

/// The volume descriptor set terminator.
fn terminator() -> Vec<u8> {
    let mut s = vec![0u8; SECTOR as usize];
    s[0] = 255;
    s[1..6].copy_from_slice(b"CD001");
    s[6] = 1;
    s
}

/// Build the image at `out`.
///
/// Streams each file rather than buffering the image, so a full disc costs a
/// sector of memory rather than 700 MB.
pub fn write_iso(entries: &[IsoEntry], volume_label: &str, out: &Path) -> Result<Layout, String> {
    use std::io::{Seek, SeekFrom, Write};

    let layout = plan(entries, volume_label);
    let secs = now_secs();
    let mut f = std::fs::File::create(out)
        .map_err(|e| format!("couldn't create {}: {e}", out.display()))?;
    let put = |lba: u64, bytes: &[u8], f: &mut std::fs::File| -> Result<(), String> {
        f.seek(SeekFrom::Start(lba * SECTOR))
            .and_then(|_| f.write_all(bytes))
            .map_err(|e| format!("writing {}: {e}", out.display()))
    };

    put(16, &volume_descriptor(&layout, false, secs), &mut f)?;
    put(17, &volume_descriptor(&layout, true, secs), &mut f)?;
    put(18, &terminator(), &mut f)?;
    put(layout.path_table_l, &path_table(layout.root_dir, false), &mut f)?;
    put(layout.path_table_m, &path_table(layout.root_dir, true), &mut f)?;
    put(
        layout.joliet_path_table_l,
        &path_table(layout.joliet_root_dir, false),
        &mut f,
    )?;
    put(
        layout.joliet_path_table_m,
        &path_table(layout.joliet_root_dir, true),
        &mut f,
    )?;
    put(layout.root_dir, &directory(&layout, false, secs), &mut f)?;
    put(
        layout.joliet_root_dir,
        &directory(&layout, true, secs),
        &mut f,
    )?;

    for placed in &layout.files {
        let data = std::fs::read(&placed.entry.path)
            .map_err(|e| format!("reading {}: {e}", placed.entry.path.display()))?;
        if data.len() as u64 != placed.entry.size {
            return Err(format!(
                "{} changed size between planning and writing ({} then {})",
                placed.entry.path.display(),
                placed.entry.size,
                data.len()
            ));
        }
        put(placed.extent, &data, &mut f)?;
    }

    // Pad to the declared volume size. A reader that trusts the descriptor
    // over the file length would otherwise walk off the end.
    f.set_len(layout.total_sectors * SECTOR)
        .map_err(|e| format!("sizing {}: {e}", out.display()))?;
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, size: u64) -> IsoEntry {
        IsoEntry {
            name: name.to_string(),
            path: PathBuf::from("/dev/null"),
            size,
        }
    }

    #[test]
    fn a_name_is_folded_to_the_level_2_character_set() {
        assert_eq!(iso_name("tone_440.mp3"), "TONE_440.MP3;1");
        assert_eq!(iso_name("My Song!.mp3"), "MY_SONG_.MP3;1");
        // A leading dot is not an extension separator, so the whole thing is
        // the stem.
        assert_eq!(iso_name(".hidden"), "_HIDDEN;1");
        assert_eq!(iso_name("noext"), "NOEXT;1");
    }

    #[test]
    fn names_that_fold_together_are_still_distinct_on_the_disc() {
        // All three differ only in characters the level 2 set cannot
        // represent, so all three fold to A_1.MP3 and two would be lost.
        let l = plan(
            &[entry("a-1.mp3", 10), entry("a+1.mp3", 10), entry("a 1.mp3", 10)],
            "T",
        );
        let names: Vec<&str> = l.files.iter().map(|f| f.iso_name.as_str()).collect();
        assert_eq!(names, vec!["A_1.MP3;1", "A_1_1.MP3;1", "A_1_2.MP3;1"]);
    }

    #[test]
    fn a_generated_name_never_steals_a_real_one() {
        // The obvious counter implementation hands A_1_1.MP3 to the second
        // a-1.mp3 without noticing a file already answers to it.
        let l = plan(
            &[entry("a-1.mp3", 1), entry("a+1.mp3", 1), entry("a_1_1.mp3", 1)],
            "T",
        );
        let mut names: Vec<&str> = l.files.iter().map(|f| f.iso_name.as_str()).collect();
        let count = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), count, "every record needs its own identifier");
    }

    #[test]
    fn the_suffix_goes_on_the_stem_so_the_extension_still_dispatches() {
        let l = plan(&[entry("a-1.mp3", 1), entry("a+1.mp3", 1)], "T");
        for f in &l.files {
            assert!(
                f.iso_name.ends_with(".MP3;1"),
                "{} lost its extension",
                f.iso_name
            );
        }
    }

    #[test]
    fn records_are_sorted_because_a_reader_may_binary_search_them() {
        let l = plan(
            &[entry("zulu.mp3", 1), entry("alpha.mp3", 1), entry("mike.mp3", 1)],
            "T",
        );
        let names: Vec<String> = l.files.iter().map(|f| f.iso_name.clone()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn every_file_gets_its_own_non_overlapping_extent() {
        let l = plan(
            &[entry("a.mp3", 5000), entry("b.mp3", 1), entry("c.mp3", 0)],
            "T",
        );
        let mut prev_end = l.joliet_root_dir + l.joliet_root_dir_sectors;
        for f in &l.files {
            assert_eq!(f.extent, prev_end, "extents must be contiguous");
            // A zero-length file still occupies one sector, so its extent is
            // a real address rather than pointing at the next file's data.
            prev_end += sectors_for(f.entry.size).max(1);
        }
        assert_eq!(l.total_sectors, prev_end);
    }

    #[test]
    fn the_descriptors_carry_the_signature_a_reader_looks_for() {
        let l = plan(&[entry("a.mp3", 1)], "SPARKAMP");
        let pvd = volume_descriptor(&l, false, 0);
        assert_eq!(pvd[0], 1, "primary descriptor type");
        assert_eq!(&pvd[1..6], b"CD001");
        let svd = volume_descriptor(&l, true, 0);
        assert_eq!(svd[0], 2, "supplementary descriptor type");
        assert_eq!(&svd[1..6], b"CD001");
        assert_eq!(&svd[88..91], b"%/E", "the escape sequence is what makes it Joliet");
        let end = terminator();
        assert_eq!(end[0], 255);
        assert_eq!(&end[1..6], b"CD001");
    }

    #[test]
    fn both_endian_fields_agree_with_themselves() {
        let b = both32(0x0102_0304);
        assert_eq!(&b[..4], &0x0102_0304u32.to_le_bytes());
        assert_eq!(&b[4..], &0x0102_0304u32.to_be_bytes());
        let h = both16(0x0102);
        assert_eq!(&h[..2], &0x0102u16.to_le_bytes());
        assert_eq!(&h[2..], &0x0102u16.to_be_bytes());
    }

    #[test]
    fn the_civil_date_matches_known_timestamps() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        // 2026-09-04T12:00:00Z, cross-checked against the platform's own
        // date arithmetic rather than counted by hand.
        assert_eq!(civil(1_788_523_200), (2026, 9, 4, 12, 0, 0));
        // A leap day, which is where a days-from-epoch algorithm goes wrong
        // if the era arithmetic is off.
        assert_eq!(civil(1_709_208_000), (2024, 2, 29, 12, 0, 0));
    }

    /// The one test that proves the format rather than our own arithmetic.
    ///
    /// Every other test here checks the image against the same code that wrote
    /// it, which cannot catch a misread of the specification. This hands the
    /// image to a reader nobody here wrote and asks whether the files come
    /// back. `hdiutil` is a subprocess, which the shipped app may not spawn,
    /// but a test is not the shipped app and this needs no disc.
    #[test]
    fn a_real_iso_reader_can_mount_the_image_and_read_every_file() {
        let dir = std::env::temp_dir().join(format!("sparkamp-isotest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        // Names chosen to exercise the parts that go wrong: a space and a
        // hyphen are outside the level 2 set, and the two long names differ
        // only past the point ISO 9660 truncates.
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("tone_440.mp3", vec![0xAAu8; 3000]),
            ("My Track - 02.mp3", vec![0xBB; 1]),
            ("playlist.m3u8", b"#EXTM3U\n".to_vec()),
        ];
        let mut entries = Vec::new();
        for (name, data) in &cases {
            let p = dir.join(name);
            std::fs::write(&p, data).expect("write source");
            entries.push(IsoEntry {
                name: (*name).to_string(),
                path: p,
                size: data.len() as u64,
            });
        }

        let iso = dir.join("out.iso");
        let layout = write_iso(&entries, "SPARKAMP TEST", &iso).expect("write iso");
        assert!(layout.total_sectors > 0);

        let out = std::process::Command::new("/usr/bin/hdiutil")
            .args(["attach", "-nobrowse", "-readonly", "-mountrandom", "/tmp"])
            .arg(&iso)
            .output()
            .expect("run hdiutil");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            out.status.success(),
            "hdiutil refused the image, so it is not a valid ISO 9660:\n{}\n{}",
            stdout,
            String::from_utf8_lossy(&out.stderr)
        );
        let mount = stdout
            .lines()
            .filter_map(|l| l.split('\t').next_back())
            .map(str::trim)
            .find(|s| s.starts_with('/'))
            .expect("hdiutil printed no mount point")
            .to_string();

        // Detach before asserting, so a failure does not leave a volume
        // mounted for the next run to trip over.
        let read_back: Vec<(String, Vec<u8>)> = std::fs::read_dir(&mount)
            .expect("read mount")
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(e.path()).unwrap_or_default(),
                )
            })
            .collect();
        let _ = std::process::Command::new("/usr/bin/hdiutil")
            .args(["detach", "-force"])
            .arg(&mount)
            .output();
        let _ = std::fs::remove_dir_all(&dir);

        for (name, data) in &cases {
            let got = read_back
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| {
                    panic!(
                        "{name} is missing from the mounted image; Joliet names present: {:?}",
                        read_back.iter().map(|(n, _)| n).collect::<Vec<_>>()
                    )
                });
            assert_eq!(&got.1, data, "{name} read back with different bytes");
        }
    }

    #[test]
    fn joliet_names_are_ucs2_big_endian() {
        assert_eq!(ucs2("AB"), vec![0x00, b'A', 0x00, b'B']);
    }
}
