# Path canonicalization — one file, one row

**Status:** DONE `ee80091`, 2026-08-11. Found while diagnosing a UI freeze.

Run against the live library the same day: **27,961 rows moved, 8,417
duplicates merged**. Every track row is now under `/var/mnt`, none under
`/mnt`, all 42 playlist rows migrated, and the total held flat at 36,328
across a minute — the runaway ingest that caused the freezes is gone. No
`.m3u` was rewritten, and a spot-checked playlist still resolves its entries
(the file says `/mnt/…`, the lookup canonicalizes and finds the row).

Two things the live run turned up that the plan did not predict:

- Some `.m3u` files hold a **third** spelling — Flatpak portal paths like
  `/run/user/1000/doc/4f1f2acb/Music/…`, left by an earlier sandboxed run.
  Those resolve only while that mount exists, which is a pre-existing gap this
  work neither fixes nor worsens.
- `dedup_folders` (not `normalize_folder_paths`, which does not exist) is the
  folder-side migration, and it runs on every `MediaLibrary::open`. That is
  what silently moved the folder row and started the duplication.

## The problem in three sentences

`/mnt` is a symlink to `var/mnt` on this machine (Fedora ostree; macOS has the
same shape with `/var → /private/var`). `add_folder` canonicalizes the folder
it stores, but `scan.rs:632` deliberately does not canonicalize the **track**
paths underneath it — "Use paths as-is for fast insert. Skipping
canonicalize() removes a stat call per file." So when a folder's stored
spelling changes, every track under it is rediscovered under a name that no
longer matches its existing row, the exact-string existence check at
`scan.rs:646` misses, and the file is inserted a second time.

Measured 2026-08-11: 36,311 track rows under `/mnt`, 8,417 under `/var/mnt`,
**all 8,417 exact duplicates**, zero `/var/mnt`-only rows. Still climbing —
the re-add was ~10 files/s, which is what starved the GTK main loop and forced
two hard quits (see `f367324`).

## How it starts, including from Settings

Four entry points write a track path. Only the last is safe today.

| Entry point | Canonicalizes? |
|---|---|
| `add_folder` (Settings ▸ Add Folder…) — the *folder* row | yes, `scan.rs:142` |
| `scan_folder` — the *track* rows under it | **no**, `scan.rs:632` |
| `apply_watch_action` → `upsert_path` — watcher events | **no** |
| `Track::path` (playback, `last_playlist.toml`, duration cache) | yes, `model.rs:133` |

`normalize_folder_paths` (`scan.rs:168–215`) already repairs *folder*
duplicates and re-homes tracks by `folder_id` — but it never rewrites
`tracks.path`. That is the exact hole this plan fills.

**The Settings case Josef called out:** picking `/mnt/Blackbeard/Music` in the
file chooser stores `/var/mnt/Blackbeard/Music`. If the library already holds
tracks under `/mnt/...`, adding that folder immediately starts duplicating
them. Whatever normalization we add must run *when a folder is added*, not
only at startup.

## Normal form

**The canonical (symlink-resolved) path wins** — `/var/mnt/...`.

Not a coin flip: `Track::path` already resolves to it, so playback, the
duration cache and `last_playlist.toml` are all on that side today. `scan.rs:310`
documents the resulting split and works around it by skipping auto-add-played
for inside-folder paths. Choosing the other direction would mean walking that
back.

## Changes

### 1. Canonicalize on insert

Resolve before any track path reaches the DB, in `scan_folder` and in
`upsert_path`. Use `pathutil::canonicalize_lenient`, which the folder side
already uses.

The "saves a stat per file" argument no longer holds: `upsert_track` stats the
file anyway via `ProbedTrackMetadata::probe`. For the bulk scan path, resolve
the *folder* once and join relative names onto it rather than calling
`canonicalize` per file — same result, no extra syscall.

### 2. Canonicalize on lookup, and leave user files alone

`metadata_by_path`, `track_by_path` and the playlist resolvers compare paths
by exact string. Canonicalize the incoming path in those lookups.

This is what keeps the migration off the user's disk: a `.m3u` containing
`/mnt/...` keeps resolving after the rows move, so **no `.m3u` file is
rewritten**. Their 42 saved playlists are not touched.

### 3. Migrate the rows

One pass over `tracks`, `playlists.path` and `device_sync_pairs.library_path`:
canonicalize each stored path; if it differs from what is stored, that row
needs to move.

- No row at the target → `UPDATE` the path in place.
- Row already at the target → merge `play_count` and `last_played` (keep the
  larger count, the later timestamp), then delete the alias row.

Here the merge is nearly free: exactly one `/mnt` row has plays and no
`/var/mnt` row does. The code still has to be right in general.

Cost is one `stat` per row — ~44k on a spinning disk. Run it off the main
thread through the existing scan-progress machinery (`start_ml_scan` /
`update_ml_scan_progress` / `complete_ml_scan`), never inline in a tick.

### 4. Run it at the two moments that matter

- **At startup**, beside `normalize_folder_paths` — repairs libraries already
  in this state.
- **From `add_folder`**, when the canonicalized folder differs from what the
  caller passed, or when any existing row sits under the pre-canonical prefix.
  This is Josef's Settings case, and skipping it means the bug returns the
  next time a folder is added through a symlink.

## Verification

- Unit: a temp dir plus a symlink to it; add the symlinked path as a folder,
  scan, assert one row per file. Add the real path afterwards, assert still one
  row and one folder.
- Unit: migration with a colliding pair where the alias row holds the play
  count — assert the survivor keeps it.
- On Josef's library: `select count(*) from tracks` drops by ~8,417 and then
  holds steady across a restart; all 42 playlists still resolve their tracks.
- The freeze repro (`scratchpad/scrolljump.py`) should show no ingest at all
  once the library is normalized, since there is nothing left to re-add.

## Risks

- **The `.m3u` files stay as they are** by design (change 2). If the lookup
  canonicalization is missed anywhere, a playlist silently loses its metadata
  rather than erroring. Grep for `WHERE path = ` before calling this done —
  12 sites outside the tests: `mod.rs` 1, `playlists.rs` 4, `queries.rs` 1,
  `scan.rs` 6.
- `device_sync_pairs` has rows on both sides already (24 `/mnt`, 4
  `/var/mnt`), so its merge is the one most likely to collide.
- Bind mounts resolve to themselves and `canonicalize` will not unify them.
  `playlists::file_identity` (dev+inode) is the tool if that ever comes up;
  out of scope here.
