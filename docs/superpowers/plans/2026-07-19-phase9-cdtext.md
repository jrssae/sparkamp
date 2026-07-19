# Phase 9 — F5 CD-TEXT on Unknown Discs (design-level plan)

> Read `2026-07-19-opus-handoff.md` first. Disc subsystem; the libburn
> `cdtext_to_v07t` path was PROVEN in the phase-"Disc UX 2" live tests —
> reuse that exact mechanism, don't invent a reader.

**Goal:** When gnudb has no match for an audio CD (or leaves gaps), read
CD-TEXT off the disc and use album / artist / per-track titles in the disc
view.

## Architecture

- **Read mechanism:** the existing disc code already shells/links the
  libburn path that can emit Sony v07t CD-TEXT dumps
  (`cdtext_to_v07t` — see `src/disc/` and the P2 ledger notes; the v07t
  format was verified against `man cdrskin` during P2 task 4). Extract a
  `read_cdtext(drive) -> Option<CdText>` in `src/disc/` beside the
  existing probe utilities.
- **Parser:** `CdText { album_title, album_performer, tracks:
  Vec<{title, performer}> }` from the v07t text — PURE parser fn over the
  dump string; fixtures from the documented format (P2's task-4 work has
  verified real-world shapes; sanitize NULs per gtk_safe rules).
- **Precedence (decided):** gnudb wins where it has data; CD-TEXT fills
  what gnudb left blank; whole-miss → CD-TEXT alone; neither → today's
  "Track N" fallback. One pure merge fn `merge_disc_metadata(gnudb,
  cdtext) -> DiscMeta` — table-tested.
- **Drive contention:** CD-TEXT read happens at PROBE TIME ONLY (same
  moment the TOC/gnudb probe runs), never during burn/rip; respect the
  exclusive-read refcount added in P2 (`0356e3f`) — if the drive is
  exclusively held, skip CD-TEXT silently (metadata stays gnudb/fallback).
- Frontends: no new UI — the disc view simply shows better names.
  GTK/mac/TUI all read the same core DiscMeta already.

## Automated tests

- v07t parser: full fixture (album + performers + track titles), partial
  fixture (titles only), garbage/empty → None, NUL/odd-encoding
  sanitization.
- merge_disc_metadata table: gnudb-full (cdtext ignored), gnudb-partial
  (gaps filled), gnudb-miss (cdtext used), both-miss (fallback), track
  count mismatch (min-length zip, extras keep fallback names).
- Probe integration: contention flag set → read skipped (unit with the
  refcount stubbed/held).
- Live test (ignored-by-default, like the existing `live_*` disc tests):
  `live_cdtext_read` — requires a real disc with CD-TEXT; human runs it.

## Manual test plan

1. Insert a CD-TEXT-bearing disc absent from gnudb → disc view shows real
   album/artist/track titles.
2. Disc known to gnudb → gnudb names unchanged (CD-TEXT doesn't override).
3. Disc with neither → "Track N" fallback as today.
4. Start a burn in one window, insert/probe another operation — no drive
   fight (CD-TEXT skipped under exclusive hold, no error dialogs).
5. Rip flow: ripped filenames/tags inherit the CD-TEXT names.
6. mac: same three discs walk; TUI disc screen shows names.

## Performance / pitfalls

- CD-TEXT read spins the disc — do it once per insertion within the
  existing probe (cache in the disc session state; do NOT re-read on every
  view refresh).
- Some drives/discs lie or return mojibake: sanitize + length-cap fields;
  if the parse yields empty strings treat as miss.
- Never block the UI thread on the read — the probe already runs
  background; keep CD-TEXT inside that job.

## Open questions

1. Whether to ALSO offer CD-TEXT when gnudb matched but user wants disc
   names (a "prefer disc CD-TEXT" toggle) — propose NO (YAGNI), confirm.
