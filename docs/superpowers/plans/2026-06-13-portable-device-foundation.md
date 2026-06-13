# Portable Device Support — Foundation (Schema + Diagnostics) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the non-UI, no-new-dependency foundation for external-device support — the SQLite schema for devices and per-file sync pairs, the persistence CRUD, and the pure udisks2-failure diagnostics classifier — all fully unit-tested.

**Architecture:** Two pieces. (1) Persistence lives in the existing media-library SQLite DB via additive migrations and a new `src/media_library/devices.rs` submodule holding CRUD on `MediaLibrary`, reusing the existing `temp_lib()` test harness. (2) A new top-level `src/devices/` module holds `diagnostics.rs`: pure functions that turn `/.flatpak-info`, `/etc/os-release`, and a D-Bus error kind into a friendly `Diagnosis`, plus thin IO wrappers. No GTK, no D-Bus client, no new crates — those arrive in the next plan (udisks2 detection + Devices UI).

**Tech Stack:** Rust, rusqlite 0.31 (bundled), anyhow, the existing distrobox `dev-box` for build/test.

---

## Scope

This plan covers spec chunks **1** (schema/CRUD) and the pure half of **2** (the diagnostics classifier from §3.3 of the design). It explicitly does NOT cover: the `zbus` udisks2 detection client, the GTK Devices nav section, transfers, or sync — those are later plans. Everything here is verifiable with `cargo test` in the distrobox; no hardware or Flatpak run is required.

Reference spec: `docs/superpowers/specs/2026-06-13-portable-device-support-design.md` (§3.3 diagnostics, §5 data model).

## File Structure

- **Modify** `src/media_library/mod.rs` — add `devices` + `device_sync_pairs` tables to `init_schema`, add the `rating` column to the migration list, and declare `mod devices;`.
- **Create** `src/media_library/devices.rs` — `DeviceRecord`, `SyncPair`, and CRUD methods on `MediaLibrary`. One responsibility: device/sync-pair persistence.
- **Modify** `src/media_library/tests.rs` — schema + CRUD tests (reuses `temp_lib()`).
- **Create** `src/devices/mod.rs` — module root; declares and re-exports `diagnostics`.
- **Create** `src/devices/diagnostics.rs` — pure classifier + IO wrappers, with inline `#[cfg(test)]` tests.
- **Modify** `src/lib.rs` and `src/main.rs` — declare the new `devices` module.

All build/test commands run inside the distrobox:
`distrobox enter dev-box -- sh -c '<cmd>'`.

---

## Task 1: Schema — rating column + devices and device_sync_pairs tables

**Files:**
- Modify: `src/media_library/mod.rs:347-414` (the `init_schema` `execute_batch` and the `new_cols` array)
- Test: `src/media_library/tests.rs` (append)

- [ ] **Step 1: Write the failing test**

Append to `src/media_library/tests.rs`:

```rust
// ── device schema ──────────────────────────────────────────────────────

fn table_exists(lib: &MediaLibrary, name: &str) -> bool {
    lib.conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |_| Ok(()),
        )
        .is_ok()
}

fn column_exists(lib: &MediaLibrary, table: &str, col: &str) -> bool {
    let mut stmt = lib
        .conn
        .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    cols.iter().any(|c| c == col)
}

#[test]
fn schema_has_device_tables_and_rating_column() {
    let (lib, _db) = temp_lib();
    assert!(table_exists(&lib, "devices"));
    assert!(table_exists(&lib, "device_sync_pairs"));
    assert!(column_exists(&lib, "tracks", "rating"));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `distrobox enter dev-box -- sh -c 'cargo test schema_has_device_tables -- --nocapture'`
Expected: FAIL — `assertion failed: table_exists(&lib, "devices")` (tables not created yet).

- [ ] **Step 3: Add the tables and column**

In `src/media_library/mod.rs`, inside the `init_schema` `execute_batch` string, add these two `CREATE TABLE` blocks immediately before the `CREATE INDEX idx_tracks_artist` line (around line 392):

```sql
            CREATE TABLE IF NOT EXISTS devices (
                id          TEXT PRIMARY KEY,
                label       TEXT NOT NULL DEFAULT '',
                last_seen   TEXT,
                smart_rules TEXT
            );

            CREATE TABLE IF NOT EXISTS device_sync_pairs (
                device_id          TEXT NOT NULL,
                device_relpath     TEXT NOT NULL,
                library_path       TEXT NOT NULL,
                baseline_tag_hash  TEXT NOT NULL DEFAULT '',
                baseline_rating    INTEGER NOT NULL DEFAULT 0,
                baseline_playcount INTEGER NOT NULL DEFAULT 0,
                last_sync_at       TEXT,
                PRIMARY KEY (device_id, device_relpath)
            );

            CREATE INDEX IF NOT EXISTS idx_pairs_library
                ON device_sync_pairs(library_path);
```

Then, in the `new_cols` array (currently ending with `("deleted_at", "TEXT"),`), add one entry so existing databases gain the rating column:

```rust
            ("deleted_at", "TEXT"),
            ("rating", "INTEGER"),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `distrobox enter dev-box -- sh -c 'cargo test schema_has_device_tables -- --nocapture'`
Expected: PASS (1 passed).

- [ ] **Step 5: Commit**

```bash
git add src/media_library/mod.rs src/media_library/tests.rs
git commit -m "feat(devices): add rating column + devices/device_sync_pairs tables"
```

---

## Task 2: Device & sync-pair persistence CRUD

**Files:**
- Create: `src/media_library/devices.rs`
- Modify: `src/media_library/mod.rs:16-18` (add `mod devices;`)
- Test: `src/media_library/tests.rs` (append)

- [ ] **Step 1: Create the persistence module**

Create `src/media_library/devices.rs`:

```rust
//! Persistence for external-device records and the per-file sync pairs that
//! record which library files were explicitly copied to which device.
//!
//! A pair exists ONLY for a file the user transferred through Sparkamp (either
//! direction). Coincidental same-named files never get a pair, so the sync
//! engine never touches them.

use anyhow::{Context, Result};
use rusqlite::params;

use super::MediaLibrary;

/// A remembered external device. `id` is the volume UUID when available, else
/// a generated marker-file id assigned by the detection layer.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceRecord {
    pub id: String,
    pub label: String,
    /// ISO-8601 UTC of the last time the device was seen connected.
    pub last_seen: Option<String>,
    /// Serialized per-device smart-sync rules; `None` until any are set.
    pub smart_rules: Option<String>,
}

/// One file copied via Sparkamp between the library and a device.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncPair {
    pub device_id: String,
    /// Path on the device, relative to its mount root.
    pub device_relpath: String,
    pub library_path: String,
    /// Hash of the normalized tags at the last successful sync (written by the
    /// sync engine in a later phase; stored verbatim here).
    pub baseline_tag_hash: String,
    pub baseline_rating: i64,
    pub baseline_playcount: i64,
    pub last_sync_at: Option<String>,
}

// The macOS bin gates out the GTK/FFI callers of these methods; mirror the
// dead-code allow used on the other media_library impl blocks.
#[allow(dead_code)]
impl MediaLibrary {
    /// Insert a device, or update its label/last_seen/rules if `id` exists.
    pub fn upsert_device(&self, dev: &DeviceRecord) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO devices (id, label, last_seen, smart_rules)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET
                     label       = excluded.label,
                     last_seen   = excluded.last_seen,
                     smart_rules = excluded.smart_rules",
                params![dev.id, dev.label, dev.last_seen, dev.smart_rules],
            )
            .context("upsert_device")?;
        Ok(())
    }

    /// Fetch a device record by id, or `None` when it has never been seen.
    pub fn get_device(&self, id: &str) -> Result<Option<DeviceRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, label, last_seen, smart_rules FROM devices WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(DeviceRecord {
                id: row.get(0)?,
                label: row.get(1)?,
                last_seen: row.get(2)?,
                smart_rules: row.get(3)?,
            })),
            None => Ok(None),
        }
    }

    /// Create a sync pair, or replace it when `(device_id, device_relpath)`
    /// already exists (e.g. re-copying or refreshing the baseline after sync).
    pub fn upsert_sync_pair(&self, pair: &SyncPair) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO device_sync_pairs
                    (device_id, device_relpath, library_path,
                     baseline_tag_hash, baseline_rating, baseline_playcount, last_sync_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(device_id, device_relpath) DO UPDATE SET
                     library_path       = excluded.library_path,
                     baseline_tag_hash  = excluded.baseline_tag_hash,
                     baseline_rating    = excluded.baseline_rating,
                     baseline_playcount = excluded.baseline_playcount,
                     last_sync_at       = excluded.last_sync_at",
                params![
                    pair.device_id,
                    pair.device_relpath,
                    pair.library_path,
                    pair.baseline_tag_hash,
                    pair.baseline_rating,
                    pair.baseline_playcount,
                    pair.last_sync_at
                ],
            )
            .context("upsert_sync_pair")?;
        Ok(())
    }

    /// All pairs for a device, ordered by on-device path.
    pub fn sync_pairs_for_device(&self, device_id: &str) -> Result<Vec<SyncPair>> {
        let mut stmt = self.conn.prepare(
            "SELECT device_id, device_relpath, library_path, baseline_tag_hash,
                    baseline_rating, baseline_playcount, last_sync_at
             FROM device_sync_pairs WHERE device_id = ?1
             ORDER BY device_relpath",
        )?;
        let rows = stmt.query_map(params![device_id], Self::row_to_pair)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("sync_pairs_for_device")
    }

    /// All pairs whose library side is `library_path` (used when a library file
    /// is removed, to offer deleting the device copies).
    pub fn sync_pairs_for_library_path(&self, library_path: &str) -> Result<Vec<SyncPair>> {
        let mut stmt = self.conn.prepare(
            "SELECT device_id, device_relpath, library_path, baseline_tag_hash,
                    baseline_rating, baseline_playcount, last_sync_at
             FROM device_sync_pairs WHERE library_path = ?1",
        )?;
        let rows = stmt.query_map(params![library_path], Self::row_to_pair)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("sync_pairs_for_library_path")
    }

    /// Remove a single pair. Does nothing if it does not exist.
    pub fn delete_sync_pair(&self, device_id: &str, device_relpath: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM device_sync_pairs
                 WHERE device_id = ?1 AND device_relpath = ?2",
                params![device_id, device_relpath],
            )
            .context("delete_sync_pair")?;
        Ok(())
    }

    fn row_to_pair(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncPair> {
        Ok(SyncPair {
            device_id: row.get(0)?,
            device_relpath: row.get(1)?,
            library_path: row.get(2)?,
            baseline_tag_hash: row.get(3)?,
            baseline_rating: row.get(4)?,
            baseline_playcount: row.get(5)?,
            last_sync_at: row.get(6)?,
        })
    }
}
```

- [ ] **Step 2: Declare the submodule**

In `src/media_library/mod.rs`, with the other submodule declarations (lines 16-18), add:

```rust
mod devices;
mod playlists;
mod queries;
mod scan;
```

(Insert `mod devices;` before `mod playlists;`.)

- [ ] **Step 3: Write the failing CRUD tests**

Append to `src/media_library/tests.rs`:

```rust
#[test]
fn device_upsert_and_get_roundtrip() {
    let (lib, _db) = temp_lib();
    let dev = crate::media_library::DeviceRecord {
        id: "UUID-1234".into(),
        label: "MY STICK".into(),
        last_seen: Some("2026-06-13T00:00:00Z".into()),
        smart_rules: None,
    };
    lib.upsert_device(&dev).unwrap();
    assert_eq!(lib.get_device("UUID-1234").unwrap(), Some(dev.clone()));

    // Upsert updates rather than duplicating.
    let dev2 = crate::media_library::DeviceRecord { label: "RENAMED".into(), ..dev };
    lib.upsert_device(&dev2).unwrap();
    assert_eq!(lib.get_device("UUID-1234").unwrap().unwrap().label, "RENAMED");

    assert_eq!(lib.get_device("nope").unwrap(), None);
}

#[test]
fn sync_pair_crud_and_lookups() {
    let (lib, _db) = temp_lib();
    let pair = crate::media_library::SyncPair {
        device_id: "UUID-1234".into(),
        device_relpath: "Music/A/B/song.mp3".into(),
        library_path: "/home/u/Music/song.mp3".into(),
        baseline_tag_hash: "abc".into(),
        baseline_rating: 4,
        baseline_playcount: 7,
        last_sync_at: None,
    };
    lib.upsert_sync_pair(&pair).unwrap();

    assert_eq!(lib.sync_pairs_for_device("UUID-1234").unwrap(), vec![pair.clone()]);
    assert_eq!(
        lib.sync_pairs_for_library_path("/home/u/Music/song.mp3").unwrap(),
        vec![pair.clone()]
    );

    // Upsert on the same key replaces (baseline refresh after a sync).
    let refreshed = crate::media_library::SyncPair {
        baseline_tag_hash: "def".into(),
        baseline_playcount: 8,
        ..pair.clone()
    };
    lib.upsert_sync_pair(&refreshed).unwrap();
    let got = lib.sync_pairs_for_device("UUID-1234").unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].baseline_tag_hash, "def");
    assert_eq!(got[0].baseline_playcount, 8);

    lib.delete_sync_pair("UUID-1234", "Music/A/B/song.mp3").unwrap();
    assert!(lib.sync_pairs_for_device("UUID-1234").unwrap().is_empty());
}
```

- [ ] **Step 4: Re-export the types so tests and callers can name them**

In `src/media_library/mod.rs`, near the existing `pub use` re-exports of library types (search for `pub use` of `LibTrack`/`LibPlaylist`), add:

```rust
pub use devices::{DeviceRecord, SyncPair};
```

If there is no existing `pub use` block, add this line directly after the `mod devices;` declaration.

- [ ] **Step 5: Run the tests (expect compile error first, then pass)**

Run: `distrobox enter dev-box -- sh -c 'cargo test device_upsert sync_pair_crud 2>&1 | tail -20'`
Expected: PASS — `device_upsert_and_get_roundtrip` and `sync_pair_crud_and_lookups` both pass. If the first run shows an unused-import or dead-code warning, the zero-warnings rule applies — re-run a full `cargo build` (Step 6 of Task 4) resolves module wiring; warnings about these CRUD methods are suppressed by the `#[allow(dead_code)]` on the impl block.

- [ ] **Step 6: Commit**

```bash
git add src/media_library/devices.rs src/media_library/mod.rs src/media_library/tests.rs
git commit -m "feat(devices): DeviceRecord/SyncPair persistence CRUD"
```

---

## Task 3: udisks2-failure diagnostics classifier (pure)

**Files:**
- Create: `src/devices/mod.rs`
- Create: `src/devices/diagnostics.rs`

- [ ] **Step 1: Create the module root**

Create `src/devices/mod.rs`:

```rust
//! External-device support. This phase ships only the failure-diagnostics
//! classifier; the udisks2 detection client and transfer/sync engines arrive
//! in later phases.

pub mod diagnostics;
```

- [ ] **Step 2: Create the diagnostics module with its tests**

Create `src/devices/diagnostics.rs`:

```rust
//! Friendly diagnosis of why the system disk service (udisks2) is unreachable.
//!
//! Turns three local, sandbox-readable signals — our own `/.flatpak-info`
//! grants, `/etc/os-release` (+ `/run/ostree-booted`), and the D-Bus error
//! kind — into a single [`Diagnosis`] the UI renders as a one-line message
//! with one action button. Pure classification: callers read the input
//! strings and pass them in, so every branch is unit-testable without a live
//! bus or a specific host.

// The GTK frontend wires these in a later phase; until then the whole module
// is unreferenced in non-test builds.
#![allow(dead_code)]

/// The kind of D-Bus failure observed when talking to udisks2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbusErrorKind {
    /// The udisks2 name could not be reached or activated.
    ServiceUnknown,
    /// The bus or call was refused (e.g. filtered by the Flatpak proxy).
    AccessDenied,
    /// udisks2 answered but refused the action (polkit).
    NotAuthorized,
    /// Any other failure.
    Other,
}

/// Distro identity and whether the OS is image-based (Bazzite, Silverblue,
/// Fedora Atomic, SteamOS), where udisks2 ships in the base image so
/// "install the package" advice would be wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistroInfo {
    pub id: String,
    pub immutable: bool,
}

/// The classified outcome the UI renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Diagnosis {
    /// We weren't granted the udisks2 talk-name, or it was removed. Fix via
    /// the System-Bus permission (Flatseal / KDE settings).
    PermissionOff,
    /// udisks2 genuinely isn't installed — only possible on a traditional
    /// (non-image-based) distro.
    NotInstalled,
    /// Reached udisks2 but eject was refused, or eject isn't available; the
    /// user should eject via their file browser.
    EjectUnavailable,
}

/// Extract the system-bus talk-names this app was granted, from the contents
/// of `/.flatpak-info`. Empty when not sandboxed or none were granted.
pub fn parse_flatpak_info_talk_names(flatpak_info: &str) -> Vec<String> {
    for line in flatpak_info.lines() {
        if let Some(rest) = line.trim().strip_prefix("system-talk-name=") {
            return rest
                .split(';')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
        }
    }
    Vec::new()
}

/// Whether `/.flatpak-info` shows the udisks2 system talk-name as granted.
pub fn has_udisks_grant(flatpak_info: &str) -> bool {
    parse_flatpak_info_talk_names(flatpak_info)
        .iter()
        .any(|n| n == "org.freedesktop.UDisks2")
}

/// Parse `/etc/os-release` (and whether `/run/ostree-booted` exists) into a
/// [`DistroInfo`].
pub fn parse_os_release(os_release: &str, ostree_booted: bool) -> DistroInfo {
    let mut id = String::new();
    let mut variant = String::new();
    for line in os_release.lines() {
        if let Some(v) = line.strip_prefix("ID=") {
            id = unquote(v);
        } else if let Some(v) = line.strip_prefix("VARIANT_ID=") {
            variant = unquote(v);
        }
    }
    let immutable = ostree_booted
        || id == "steamos"
        || id == "bazzite"
        || variant.contains("silverblue")
        || variant.contains("kinoite")
        || variant.contains("atomic");
    DistroInfo { id, immutable }
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

/// Decide which message to show.
///
/// - Talk-name missing from our grant, or the bus refused us → `PermissionOff`
///   (a packaging / override issue), regardless of distro.
/// - Granted but the service is unknown → `NotInstalled` on a traditional
///   distro, but `PermissionOff` on an immutable one (the daemon is in the base
///   image, so the real cause is almost always the permission / proxy).
/// - The action was refused by policy → `EjectUnavailable`.
pub fn classify(granted: bool, distro: &DistroInfo, err: DbusErrorKind) -> Diagnosis {
    match err {
        DbusErrorKind::NotAuthorized => Diagnosis::EjectUnavailable,
        DbusErrorKind::AccessDenied => Diagnosis::PermissionOff,
        DbusErrorKind::ServiceUnknown => {
            if !granted || distro.immutable {
                Diagnosis::PermissionOff
            } else {
                Diagnosis::NotInstalled
            }
        }
        DbusErrorKind::Other => {
            if granted {
                Diagnosis::EjectUnavailable
            } else {
                Diagnosis::PermissionOff
            }
        }
    }
}

// ── runtime IO wrappers (thin; not unit-tested) ────────────────────────────

/// Read `/.flatpak-info`; empty string when not sandboxed.
pub fn read_flatpak_info() -> String {
    std::fs::read_to_string("/.flatpak-info").unwrap_or_default()
}

/// Read the host distro identity from `/etc/os-release` + `/run/ostree-booted`.
pub fn read_distro_info() -> DistroInfo {
    let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let ostree = std::path::Path::new("/run/ostree-booted").exists();
    parse_os_release(&os, ostree)
}

/// Compose the live signals with an observed error kind into a [`Diagnosis`].
pub fn diagnose(err: DbusErrorKind) -> Diagnosis {
    let info = read_flatpak_info();
    let distro = read_distro_info();
    classify(has_udisks_grant(&info), &distro, err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_granted_talk_names() {
        let info = "[Application]\nname=dev.sparkamp.Sparkamp\n[Context]\n\
                    system-talk-name=org.freedesktop.UDisks2;org.freedesktop.Other;\n";
        assert!(has_udisks_grant(info));
        assert_eq!(
            parse_flatpak_info_talk_names(info),
            vec![
                "org.freedesktop.UDisks2".to_string(),
                "org.freedesktop.Other".to_string()
            ]
        );
    }

    #[test]
    fn no_grant_when_absent_or_unsandboxed() {
        assert!(!has_udisks_grant(""));
        assert!(!has_udisks_grant("[Context]\nsystem-talk-name=org.freedesktop.Other;\n"));
    }

    #[test]
    fn detects_immutable_distros() {
        assert!(parse_os_release("ID=bazzite\n", false).immutable);
        assert!(parse_os_release("ID=steamos\n", false).immutable);
        assert!(parse_os_release("ID=fedora\nVARIANT_ID=silverblue\n", false).immutable);
        assert!(parse_os_release("ID=fedora\n", true).immutable); // ostree-booted
        assert!(!parse_os_release("ID=fedora\nVARIANT_ID=workstation\n", false).immutable);
        assert!(!parse_os_release("ID=arch\n", false).immutable);
    }

    #[test]
    fn classify_permission_off_when_not_granted() {
        let arch = DistroInfo { id: "arch".into(), immutable: false };
        assert_eq!(
            classify(false, &arch, DbusErrorKind::ServiceUnknown),
            Diagnosis::PermissionOff
        );
        assert_eq!(
            classify(true, &arch, DbusErrorKind::AccessDenied),
            Diagnosis::PermissionOff
        );
    }

    #[test]
    fn classify_not_installed_only_on_traditional_distro() {
        let arch = DistroInfo { id: "arch".into(), immutable: false };
        let bazzite = DistroInfo { id: "bazzite".into(), immutable: true };
        // Granted + service unknown + traditional → genuinely missing package.
        assert_eq!(
            classify(true, &arch, DbusErrorKind::ServiceUnknown),
            Diagnosis::NotInstalled
        );
        // Same on an immutable distro → it's in the base image, so it's the
        // permission, never a missing package.
        assert_eq!(
            classify(true, &bazzite, DbusErrorKind::ServiceUnknown),
            Diagnosis::PermissionOff
        );
    }

    #[test]
    fn classify_eject_unavailable_on_polkit_denial() {
        let arch = DistroInfo { id: "arch".into(), immutable: false };
        assert_eq!(
            classify(true, &arch, DbusErrorKind::NotAuthorized),
            Diagnosis::EjectUnavailable
        );
    }
}
```

- [ ] **Step 3: Run the diagnostics tests (expect failure — module not yet wired into the crate)**

Run: `distrobox enter dev-box -- sh -c 'cargo test devices::diagnostics 2>&1 | tail -20'`
Expected: FAIL to compile — `file not found for module \`devices\`` or the tests don't run, because `mod devices;` isn't declared at the crate root yet. Task 4 wires it.

- [ ] **Step 4: Commit the module (compiles after Task 4)**

Defer the commit to Task 4 Step 4, where the module is wired in and the build is green — committing a non-compiling crate would break bisection. Proceed directly to Task 4.

---

## Task 4: Wire the `devices` module into the crate and go green

**Files:**
- Modify: `src/lib.rs:12-16` (module declarations)
- Modify: `src/main.rs:31-35` (module declarations)

- [ ] **Step 1: Declare the module in the library crate**

In `src/lib.rs`, alongside the existing `pub mod` declarations (e.g. after `pub mod granite;`), add:

```rust
pub mod devices;
```

- [ ] **Step 2: Declare the module in the binary crate**

In `src/main.rs`, alongside the existing `mod` declarations (e.g. after `mod granite;`), add:

```rust
mod devices;
```

- [ ] **Step 3: Build and test everything (zero warnings required)**

Run: `distrobox enter dev-box -- sh -c 'cargo build 2>&1 | grep -E "warning|error" ; echo "build done"; cargo test devices 2>&1 | tail -25'`
Expected:
- `cargo build` prints only `build done` (no `warning:` / `error:` lines).
- The `devices::diagnostics::tests::*` tests and the `device_*` / `sync_pair_*` / `schema_has_device_tables` media-library tests all pass.

If a `warning: unused` appears for any diagnostics item, confirm the `#![allow(dead_code)]` at the top of `src/devices/diagnostics.rs` is present; if a media-library CRUD method warns, confirm the `#[allow(dead_code)]` on its `impl MediaLibrary` block is present.

- [ ] **Step 4: Full suite + commit**

Run: `distrobox enter dev-box -- sh -c 'cargo build && cargo test 2>&1 | grep -E "test result|warning|error"'`
Expected: every `test result:` line shows `0 failed`; no `warning`/`error` lines.

```bash
git add src/devices/mod.rs src/devices/diagnostics.rs src/lib.rs src/main.rs
git commit -m "feat(devices): udisks2 failure-diagnostics classifier"
```

---

## Self-Review

**Spec coverage (against the design doc):**
- §5 data model — `rating` column, `devices`, `device_sync_pairs` → Task 1; CRUD → Task 2. ✅
- §3.3 diagnostics — `/.flatpak-info` grant detection, `os-release`/immutable detection, D-Bus-error classification into the three message variants (PermissionOff / NotInstalled / EjectUnavailable) → Task 3. ✅
- Explicitly deferred (documented in Scope): udisks2 `zbus` client, Devices UI, transfers, sync engine, marker file, manifest change. These are the next plan(s). ✅

**Placeholder scan:** No TBD/TODO; every code step shows complete code; every test step shows full assertions; every run step shows the exact command and expected result.

**Type consistency:** `DeviceRecord`/`SyncPair` field names and types match between `src/media_library/devices.rs`, the re-export, and the tests. `Diagnosis`, `DbusErrorKind`, `DistroInfo` names and variants are identical across `classify`, `diagnose`, and the tests. CRUD method names (`upsert_device`, `get_device`, `upsert_sync_pair`, `sync_pairs_for_device`, `sync_pairs_for_library_path`, `delete_sync_pair`) are used consistently in module and tests.

## Next plan (not in scope here)

udisks2 detection via `zbus` (add the dependency; enumerate removable mounted filesystems; connect/disconnect signal stream; free space; volume UUID; eject = Unmount + PowerOff) and the marker-file identity fallback, followed by the GTK Devices nav section (live add/remove, free-space bar, eject button, diagnostics banner wired to Task 3's `diagnose`). The manifest gains `--system-talk-name=org.freedesktop.UDisks2` in that plan.
