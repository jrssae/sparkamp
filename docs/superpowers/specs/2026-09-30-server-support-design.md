# Navidrome / OpenSubsonic server support — design

Date: 2026-09-30
Branch: `navidrome-opensubsonic-support`
Status: draft for review. No code written yet.

## Summary

Sparkamp learns to show music that lives on one or more Navidrome (or other
OpenSubsonic) servers next to the music on this computer, as one library. A
song that exists locally and on a server is one row. Each row says where its
copies are and whether they still agree. Server songs play through a local
download cache, so every frontend plays them the same way and a dropped
connection does not cut a song in half.

The server is a read-mostly partner. The Subsonic API cannot write tags,
upload files, or delete songs, so the design treats local tag work as the
source of change and gives it a way back to the server: export the changed
files to a USB stick and copy them onto the server. A later step does the
same over SMB.

Everything below was decided in conversation on 2026-09-30. Where an option
was rejected, the reason is recorded so nobody has to rediscover it.

## Scope

In scope, for TUI, GTK and macOS, core first:

- Any number of servers, each with a LAN URL and a remote HTTPS URL.
- A local cache of each server's catalog, used for all display and search.
- Merging local and server copies of the same song into one row, with manual
  link and unlink.
- Detecting and resolving differences between any number of copies.
- Source indicators, source filters, and the "Local changes" and "Needs
  attention" views.
- Server playlists, merged with local playlists.
- Playback of server songs, "make available offline", and graceful offline
  behaviour.
- USB export of local changes, laid out for the server.

### Non-goals

- Writing tags to a server. The API has no endpoint for it.
- Uploading or deleting server files through the API. Same reason.
- SMB push. Deferred until the USB flow has settled. The export function is
  built with a destination parameter so SMB is a second destination, not a
  second design.
- Moving device sync onto the new merge engine. Device sync stays untouched
  for now. It can join later as its own step, starting with characterization
  tests.
- Server duplicate reports in the dedupe tool. Dedupe stays local-only.
- Pinning self-signed server certificates.

## What the API allows

Checked against the OpenSubsonic docs, Navidrome's docs, and Navidrome's
source at v0.64.2, the latest release on 2026-09-30.

Playlists can be created, updated and deleted, which is more than we first
assumed. `updatePlaylist` changes name, comment and public flag, adds songs by
ID and removes songs by position. `createPlaylist` with a `playlistId`
replaces the whole song list. Navidrome marks a playlist `readonly` when it is
a smart playlist, when the user does not own it, or when it is an
auto-synced `.m3u` import. Navidrome keeps playlist edits in its database and
does not write `.m3u` files back.

Tags cannot be written. No endpoint writes tags or files, there is no upload,
and Navidrome only reads the music folder. What the API can write is kept in
Navidrome's database, per user:

- rating, 0 to 5, where 0 clears it (`setRating`)
- star and unstar
- play count, only by adding one play per `scrobble`
- bookmarks and the play queue

A scrobble does more than count. Navidrome also raises the album and artist
counts and forwards the play to Last.fm, ListenBrainz or plugin scrobblers if
the user enabled them. Sending scrobbles to make up an old play count
difference would therefore invent listening history on those services.

Song IDs are not stable. Navidrome's default track ID is
`musicbrainz_trackid|albumid,discnumber,tracknumber,title`: the MusicBrainz
track ID if the file has one, otherwise a hash of album, disc, track and
title. Retagging a file on the server gives it a new ID. The user's own
workflow (tag locally, copy the file to the server) does exactly that. This
single fact drives several decisions below.

Paths are only real if the server is told to report them. Navidrome returns
the absolute server path when the player setting "Report Real Path" is on (or
`ND_SUBSONIC_DEFAULTREPORTREALPATH` is set). Otherwise it returns a made-up
`AlbumArtist/Album/NN - Title.ext`, which is also built from tags and changes
with them.

Other facts the design relies on:

- `search3` with an empty query returns everything, paged. OpenSubsonic
  requires servers to support it for offline sync.
- `getScanStatus` returns `scanning`, and Navidrome adds `lastScan` and
  `folderCount`.
- There is no per-song modification time. Change detection means re-pulling
  the catalog and comparing.
- The song object carries the fields we compare: title, artist(s), album,
  album artist, track, disc, year, genre(s), comment, bpm, ISRC,
  MusicBrainz ID, ReplayGain, user rating and play count. Embedded artwork and
  other frames are invisible to the API.
- Authentication today is token auth: `t = md5(password + salt)` with a fresh
  salt per request. The OpenSubsonic API key extension was merged into
  Navidrome on 2026-09-28 (navidrome#6219) and is not in a release yet.
  Sparkamp detects it through `getOpenSubsonicExtensions` and switches when it
  appears.
- `stream` with `format=raw` returns the original file and needs no special
  permission. `download` also returns the original but needs the user's
  download permission, so we use `stream?format=raw`.

## Words used in this document

- A song is what the user thinks of as one track. It has one or more copies.
- A copy is one file: the local copy on this computer, or a server copy on
  one server. A song has at most one local copy and at most one copy per
  server.
- Linked copies are copies Sparkamp has decided are the same song, either by
  matching or because the user said so.
- The last agreed state is what each copy's fields looked like the last time
  the copies were in agreement, or the last time the user accepted a
  difference.
- The cache has two parts. The catalog cache is the local copy of a server's
  song list, playlists and covers. The playback cache holds downloaded audio
  files.
- Available offline means a server song was downloaded into a watched folder
  and became a local copy.

## Servers and configuration

Servers live in the TOML config. Each gets a generated ID that never changes,
so renaming a server does not break anything that refers to it.

```toml
[[servers]]
id = "6f1c2b0e-..."        # generated once, never edited
name = "oscar"
lan_url = "http://oscar.local:4533"
remote_url = "https://music.example.com"
username = "me"
enabled = true
priority = 1               # order used when several servers hold a song

[server_sync]
update_interval_hours = 24
update_on_launch = false
```

The password never goes in the TOML. It lives in the OS keychain: the macOS
Keychain, or the Secret Service on Linux. There is no plaintext fallback.
Headless Linux without a keyring prompts for the password each session.

Connections try the LAN URL first with a short timeout, about 2 seconds, then
the remote URL, about 5 seconds, and remember which worked last. Plain HTTP
is allowed only for the LAN URL, and the server settings say "unencrypted"
next to it. The remote URL must be HTTPS.

Adding a server runs `ping`, `getOpenSubsonicExtensions`, `getScanStatus` and
`getMusicFolders` and shows the server version and song count. It also checks
whether the returned paths look real (absolute) or made up, and if they are
made up it explains that turning on Report Real Path makes matching and
export far more reliable.

All three frontends can add, edit and remove servers, including the TUI with
a masked password prompt. Unlike watched folders, the TUI does not defer this
to the GUI, because a TUI-only Linux box must be able to use a server.

## When Sparkamp talks to a server

The catalog cache is the source for search, display, filters, indicators and
server playlists. Outside of that cache's update, Sparkamp contacts a server
only in these cases:

- the periodic update, every 24 hours by default, configurable
- at launch, only if "Update on launch" is checked, which it is not by default
- playing a song that has no local copy
- downloading a song (making it available offline)
- an explicit refresh from a menu, button or TUI key
- changing a rating, which is sent immediately
- editing a server playlist, which is sent immediately

A rating or playlist change that fails to send is queued and retried on the
next update, refresh or play. A failed update is retried on the next of
those triggers, or when the operating system reports a network change. There
is no retry timer.

## Storage

The existing `tracks` table keeps its meaning: one row per file on this
computer. Server songs get their own tables.

- `server_tracks` holds one row per song per server, keyed by server ID and
  server path. The Navidrome song ID is an ordinary column, updated in place
  when it changes.
- A membership table groups linked copies into songs. Each member records
  how it was linked (path, MusicBrainz ID, ISRC, tags and duration, filename
  and duration, or manual) and its last agreed field values.
- Pairs the user unlinked are kept as "never link" records, keyed by paths,
  so the matcher does not undo the user's decision.
- `server_tracks` carries a flag saying whether that copy is shown as its own
  row: true when it is not linked to anything, or when it represents a
  server-only song held by several servers. Partial indexes on that flag keep
  the list and album queries fast.

Exact columns are decided in the TDD plan.

### Why not store server songs in `tracks`

Putting server-only songs into `tracks` with a URI in `path` was the
alternative, and the one the user first leaned towards. It would have made
every existing list, search and album query work immediately. It was rejected
for three reasons.

First, a server-only row's identity would change often. The URI would carry
the song ID, and song IDs change on every retag. Downloading a song or
deleting its local copy would also rewrite the row's path. Everything keyed
on path would have to follow: device sync pairs, the saved play queue,
now-playing lookups and the file status cache.

Second, on a machine like gobook, where the server holds 36,500 songs and the
Mac holds 500, most `tracks` rows would not be files. Every operation that
walks the table treats rows as files: path normalization, rescans, ReplayGain
and artwork passes, file status checks, delete and the tag editor. Each would
need a guard, and some are only safe by accident today. For example,
`normalize_track_paths` canonicalizes every row and leaves `subsonic://x/y`
alone only because no ancestor of that string exists on disk.

Third, CLAUDE.md's deletion rule. With server songs outside `tracks`, the
delete path cannot reach them at all. Inside `tracks`, only a guard someone
remembered to write would stop it.

B still needed a separate table for server copies of linked songs, so it did
not even save a table.

### Performance

A throwaway benchmark settled the performance question. It used the real
`tracks` schema, its seven indexes, WAL mode, and the list, search and album
SQL copied from `src/media_library/queries.rs`, with rusqlite 0.31 bundled
like the app. The data was 37,000 synthetic songs in about 4,800 albums,
written in batches of 100. Each figure is the median of seven runs, and two
full runs agreed within about 5%. A is shown with the flag and partial index
described above. Without them A lost every read, because the naive
`NOT EXISTS` filter checked all 37,000 server rows even when every one was
linked.

gobook has 500 local songs, all on oscar. thesius has the same 37,000 songs
locally and on oscar.

| Workload, ms | gobook A | gobook B | thesius A | thesius B |
|---|---|---|---|---|
| First catalog import | 950 | 3,500 | 1,150 | 830 |
| Full list, sorted by artist | 100 | 110 | 150 | 115 |
| Search "love", 4,100 hits | 46 | 46 | 55 | 47 |
| Album gallery | 35 | 31 | 35 | 31 |
| 500 linked songs retagged on server | 24 | 29 | 16 | 16 |
| 500 server-only songs retagged | 8.5 | 31 | n/a | n/a |
| Database size, MB | 16 | 32 | 37 | 36 |

Reads come out within 40 ms of each other everywhere, and no user will notice
100 against 150 ms for a full reload of 37,000 rows. Writes favour A by a
wide margin on gobook, which is the server-heavy case. Performance does not
decide between the two; the reasons above do.

One caution for A. Its album query only avoids a sort when it names the
partial index with `INDEXED BY`, because the planner otherwise picks the flag
index and sorts. That hint must stay in step with the index, the same way the
existing album index already carries a warning to stay in step with
`album_rows()`.

### Song URIs

Playlists, the play queue and the engine refer to a server song as
`subsonic://<server id>/<server path>`, with the path percent-encoded. This
follows the `cdda://` precedent. The URI uses the server path rather than the
song ID because the ID changes with tags. The engine resolves the URI to a
file in the playback cache when the track loads.

## Catalog updates

An update starts with `getScanStatus`. If `lastScan` has not changed since the
last complete pull, it stops there. If the server is scanning, it postpones
the pull: comparing against a half-scanned library would look like mass
deletion.

Otherwise it pulls the catalog with `search3` and an empty query, page by
page, and compares each song's fields with the cache.

- New songs are added as they arrive. This is safe to do before the pull
  finishes, because adding never loses anything. The first import on a new
  server works this way too, so rows appear progressively and then merge into
  linked rows once matching runs at the end of the pull.
- Changed songs are updated in place, including their song ID.
- Removals are applied only after a complete pull. A pull interrupted by a
  lost connection never marks the songs it did not reach as gone.
- Before a removal of a linked copy is applied, the pull's new songs are
  checked for the same song at a new path. A match keeps the link and its
  history. This handles files moved or renamed on the server, and the case
  where a Navidrome upgrade changes every song ID at once.
- If more than 500 songs would disappear, or at least 50 and more than 10%
  of the cached songs, nothing is removed and the user is asked. A missing
  NAS mount on the server looks exactly like a user who deleted 31,000
  songs, and only a person can tell the difference. The floor of 50 keeps a
  small library from asking about every deletion. Songs found again at a new
  path in the same pull were moved, not lost, and do not count. Removing
  here only ever means dropping rows from Sparkamp's own copy of the catalog;
  nothing on the server or on disk is touched. How Sparkamp asks is still
  open (see Open questions).
- Cover art is fetched during the update as 512 px thumbnails (sharp on a
  Retina gallery tile up to 256 pt), once per album, only for new or changed
  cover IDs, and not at all for albums that are fully local. About 4,800
  covers and 300 MB on oscar the first time. Cover fetching resumes where it
  stopped if interrupted.
- After a server version change, `getOpenSubsonicExtensions` is checked again,
  which is how API key support will be noticed.

Cancelling the first import keeps the rows already added and resumes next
time. When the first import finishes, a summary reports the counts: server-only
songs, linked songs by how they were linked, possible matches and
differences. It links to Needs attention and to a list of recently auto-linked
songs for spot checks.

## Matching and linking

Matching runs in memory after a complete pull. It tries these in order and
links only when exactly one candidate is found:

1. same server path and relative local path, when the server reports real
   paths
2. same MusicBrainz track ID
3. same ISRC, duration within 2 seconds
4. same normalized artist, title and album, duration within 2 seconds
5. same filename, duration within 1 second

The user's library layout does not fully mirror the server's, so path
matching is the first attempt, not the only one. The fifth tier is the only
way to match a file that has no tags on one side, which is the user's own
example (test.mp3 untagged on the server, tagged locally). It links
automatically, and the Copies panel shows "linked by filename and duration"
so a wrong link is easy to spot.

When two or more candidates fit, nothing is linked. The row gets a `≈` mark
and shows up in Needs attention, and the Copies panel offers the candidates.

A song still unlinked after that may be on another server with no local
copy. It is matched against those other servers' songs with the same tiers
except the path, because two servers' library roots differ and their paths
prove nothing. A match makes one song and one row, so a song on three
servers lists once and plays from whichever answers first. Ambiguous
candidates link nothing and get no `≈`, which stays a mark about local files.
Unlinking two servers' copies records the pair by song URI, so neither
server's next update links them again.

The Copies panel, opened from a row's menu or a TUI key, lists every copy of
the song side by side with its path, its fields, and how it was linked. Each
copy has an Unlink button. Unlinking turns the copy into its own row and
records a never-link pair. Selecting two or more rows and choosing "Link as
same song" merges them. The link is refused if it would give a song two local
copies or two copies from one server, and it warns if durations differ by
more than 5 seconds. Manual links and never-link pairs always win over the
matcher.

Matching and dedupe share `dedupe::normalize`, so "duplicate" and "same song"
never disagree about what counts as the same artist or title.

## Differences between copies

### The merge

Each copy stores its last agreed field values, not a hash. A hash can say that
something changed. With three copies the design needs to know which field
changed where, so it stores the values. For every field:

- If no copy changed it since the last agreed state, nothing happens. Any
  remaining difference was accepted earlier.
- If one copy changed it, or several copies changed it to the same value,
  that value wins. The other copies are behind on that field.
- If two or more copies changed it to different values, that field is a
  conflict. Only that field. The others still merge.

An example with one local copy and two servers. The last agreed state was
title "Temp", genre "Rock", rating 3.

| Field | Local | oscar | server2 | Result |
|---|---|---|---|---|
| title | Temp (Live) | Temp | Temp (Live) | same new value on two copies, it wins; oscar is behind |
| genre | Rock | Live Rock | Rock | only oscar changed, local takes it; server2 is behind |
| rating | 3 | 3 | 4 | only server2 changed, local becomes 4 and oscar gets `setRating(4)` |

There is no conflict here. Local ends up with all three changes. The servers
stay behind on the tags until an export reaches them, because the API cannot
write tags.

A copy that joins a song later has no history. Each of its fields is
compared with the song: equal means it agrees, empty means it is behind,
filled where the song is empty means it is ahead, and filled with a different
value means the user chooses.

The device sync engine in `src/devices/sync.rs` works pairwise with one hash
and stays as it is. The server merge is new code, so the earlier idea of
changing `decide()` to take one baseline per side is no longer needed.

### Rating and play count

Rating merges like any other field, with one difference: Sparkamp can write
it to servers. A rating lives in the file: changing one writes the local
file's tag first, then the library, then the servers, and if the file will
not take it nothing changes anywhere. MP3 uses an ID3 POPM frame owned by
"Sparkamp" (the frame device sync writes); FLAC, Ogg Vorbis and Opus use the
FMPS_RATING comment (0.0–1.0). Other formats have no rating tag Sparkamp
writes. Scanning reads the rating from the file into the library. A rating
the user changes is sent to the servers at once. A queued rating is
checked against the server's current value before sending. If the server's
rating also changed since the last agreed state, the queued value becomes a
conflict instead of silently overwriting it.

Play count is not merged. The local count is the highest count across all
copies. Other apps playing straight from a server can push a server's count
ahead, and local catches up on the next update. When a song with server
copies is played, every server that holds a copy gets a scrobble, queued with
its play time if the server is unreachable. Old differences are never sent as
scrobbles, because that would fake listening history on Last.fm and
ListenBrainz. While scrobbles are queued, "local equals server plus queued"
counts as in sync. A play count difference never puts a row in Needs
attention.

### Resolving

Updates only mark differences. A background update never writes to local
files. "Server changed" rows show `↓`, and Needs attention offers "Apply N
server changes" as one batch.

The resolve dialog shows one column per copy and one row per differing field.
Cells changed since the last agreed state are marked, the merge result is
preselected, and the user can override any field. Applying writes the local
file and sends ratings. Server tags that cannot be written leave that server
behind, shown as `↑`, and the song appears in Local changes for export.

Batch actions on a multi-selection: take one server's tags, keep local, or
accept the differences. Batches run in the background with progress. Before
writing, the previous local values are saved so "Undo last resolve" can put
200 files back the way they were.

Conflicts are resolved per conflict, by the user. There is no automatic
winner.

## Indicators

One mark per row: where the copies are and whether they agree. The core
decides the state (`servers::indicator`); each frontend only draws it.

| State | TUI (emoji, default) | TUI symbols / ASCII | GTK and macOS icon |
|---|---|---|---|
| Local only | 💻 | `▪  ` / `L  ` | laptop |
| Server only | 🌐 | ` ☁ ` / ` C ` | cloud |
| Both, in sync | ✅ | `▪☁ ` / `LC ` | cloud with a check |
| Local changed | 🔼 | `▪☁↑` / `LC^` | cloud with an up arrow, blue |
| Server changed | 🔽 | `▪☁↓` / `LCv` | cloud with a down arrow, blue |
| Conflict | ❗ | `▪☁!` / `LC!` | cloud with "!", orange |
| First link, differs | ❓ | `▪☁?` / `LC?` | question mark, orange |
| Possible match | 🔗 | `≈` / `~` in the third cell | cloud with "≈", purple |
| Not playable offline | 🚫 | `×` / `x` for the cloud | cloud with a slash |

With several servers the row shows the most urgent state, in the order
conflict, server changed, local changed, first link, in sync. The tooltip
says it in words.

GTK and macOS draw the same icons: SVGs in `frontends/gtk/icons/source/`
(GTK loads 32 px PNGs rendered from them) and the same SVGs in the macOS
asset catalog. macOS does not use the `icloud.*` SF Symbols, which Apple
reserves for iCloud itself.

The TUI emoji all have emoji presentation by default and are East Asian
"wide", so every terminal draws them two cells wide; emoji that need a
variation selector (☁️, ⬆️, ⚠️) are avoided because terminals disagree on
their width. A terminal without an emoji font (the Linux text console) shows
boxes, so `server_sync.indicators` can be `"symbols"` or `"ascii"`. The
symbols are ambiguous-width characters, which some terminals in CJK locales
draw two cells wide, hence ASCII.

## Filters

There is no source filter today, only search. Each frontend gets filter
entries under Files in its Media Library sidebar: All, Local, one entry per
server, Local changes, and Needs attention. On macOS this becomes a
`.files(filter:)` case of `MLNavigation`.

- Local shows songs that have a local copy, whether or not they are also on a
  server. That is also the set that plays offline.
- A server entry shows songs with a copy on that server.
- Local changes shows local-only songs and songs where local is ahead of a
  server.
- Needs attention shows conflicts, first-link differences, possible matches
  and server changes.

## Playlists

Server playlists live only in the database. They are refreshed with each
update and shown in the playlist view and side navigation with the same
indicators as songs.

A local `.m3u8` and a server playlist with the same name, ignoring case, are
linked once and stay linked. They are compared as lists of songs, so a local
path and a server entry for the same song count as equal. If one side
changed, it wins and the other side is updated: a server playlist through
`createPlaylist` with the `playlistId`, which replaces the list. If two sides
changed differently, the user picks a version. Entries are not merged one by
one, because merging ordered lists entry by entry confuses more than it helps.
Read-only server playlists only pull, show a lock, and offer "Duplicate as
local playlist".

First step, in place now: each update pulls every server's playlists into
the database (the list every time, since a playlist changes without a
library scan, and the songs only of those whose `changed` stamp moved).
They are listed beside the playlist files with a cloud where a file has a
computer, and each entry plays its local copy when there is one. Until
changes are sent to servers, every server playlist is read-only in Sparkamp
and Save As makes a local copy. Same-name linking comes with sending.

### Server-only songs in local playlists

A local `.m3u8` records a server-only song as a Sparkamp comment line:

```
#SPARKAMP-SONG:server=6f1c2b0e-...;path=Artist/Album/01 Song.mp3;title=Song;artist=Artist
```

Other players ignore `#` lines they do not know, so they see the local entries
and nothing breaks. Sparkamp sees the whole list. The line holds no URL, no
token and no song ID. If the same playlist file reaches a machine where the
song is local, the line resolves to the local copy. When the song becomes
local, the next save rewrites the line as a normal path. No `#EXTINF` is
written for these lines, because players attach `#EXTINF` to the next real
path and would mislabel it.

This also changed an earlier decision. A pulled server playlist used to leave
the local file as a subset, marked partial. With these lines it keeps every
entry.

If the server removes the song, the line stays and shows as missing, like a
missing local file. It resolves again if the song comes back or re-matches.

Device playlists never get these lines. A device `.m3u8` holds device paths
only.

### Other playlist rules

New Playlist asks where to create it: this computer, which is the default, a
server, or both, linked. A local-only song cannot be added to a server
playlist, because the server cannot refer to a song it does not have. The user
gets a message saying the song is not on that server yet.

Code that must change first: `save_playlist_tracks` in
`src/media_library/playlists.rs` looks every entry up in `tracks` and
silently drops the ones it cannot find. Server-only songs are not in `tracks`,
so saving a playlist would quietly lose them. A failing test for this comes
before any other playlist work.

## Playback

A local copy always plays first. Without one, Sparkamp plays from the
playback cache, downloading with `stream?format=raw` if needed. Every
frontend works this way, macOS, GTK and TUI alike. macOS needs it because
`AVAudioFile` only opens local files, and switching to `AVPlayer` would bypass
the `AVAudioEngine` equalizer. Linux and the TUI use it too so there is one
code path and so the next two points work everywhere.

- While a server song plays, the next two server songs in play order
  (queued songs first) are downloaded ahead. In shuffle, the next pick is
  made early so the song fetched is the song that plays. Jumping or skipping
  past what was fetched means waiting for a download. The song played before
  the current one stays cached so "previous" plays at once.
- The songs fetched ahead follow the playlist, not only track changes:
  adding, removing or reordering songs, the play queue, shuffle and repeat
  all update them on the next tick, so a server song added after the one
  playing starts downloading at once, and one removed stops being kept.
  Nothing is fetched ahead while stopped.
- A connection dropping mid-song does not interrupt it once the song is
  cached.

A song does not have to finish downloading before it plays. The download
writes a `.part` file, and playback starts from it:

- macOS cannot read a growing file with `AVAudioFile`, so the core decodes
  the `.part` with symphonia on its own thread and hands the engine PCM, the
  same way CD audio reaches it, keeping the equalizer, ReplayGain and the
  visualizer. A background probe works out the format first, so the UI
  thread never waits on the network. Formats symphonia cannot decode (Opus)
  and files whose index sits at the end play once the download completes.
  A seek past what has arrived waits for those bytes.
- GStreamer (Linux, and the TUI there) streams the server URL itself and
  decodes any format, while the cache download carries on beside it for
  later plays. The song is fetched twice the first time it plays.

While a song is still waiting on its download, the player stops whatever
played before, remembers that it should play, and the frontends try again
every 500 ms (`Player::retry_download`); a stop gives up waiting.

The playback cache is for playing, not storing. It lives in the OS cache
directory, not in `~/.config/sparkamp`: the sandbox container's Caches on
macOS, the XDG cache directory on Linux. Beyond the song playing, the songs
fetched ahead and the one played before (which are never evicted), it keeps
at most `cache_max_mb` (default 128 MB, a few CD-quality FLACs), dropping the
least recently played first. Files are named by hash. Listening offline is
"Make available offline", which puts the file in the library.

If several servers hold a song, playback tries them in priority order and
moves to the next server before skipping the song.

Two existing behaviours need changing:

- `record_play(path)` updates only a `tracks` row with that path and does
  nothing otherwise. A cache file path would never count a play or trigger a
  scrobble. It must accept a song URI and route the play to the song.
- The controller marks a track `broken` for the rest of the session when it
  fails to load (`src/controller.rs`). A server song that fails because the
  server is unreachable needs a separate unavailable state. It is skipped the
  same way but cleared when the server comes back. Otherwise one train ride
  would leave half a playlist marked broken.

### Available offline

"Make available offline" downloads the original file into a dedicated watched
folder, `~/Music/Sparkamp/<server name>/<server path>`, added as a watched
folder automatically. The user's own folder layout stays untouched. The link
is recorded immediately with both sides agreeing, so the row becomes a
linked song with no differences. On macOS, `~/Music` is already covered by the
`assets.music.read-write` entitlement. The action is disabled while the
server is unreachable. Albums get "Make album available offline".

## Now playing

The now-playing panel reads tags straight off the file (`read_tag_fields` in
`src/now_playing.rs`), and lyrics come from embedded tags. The playback cache
holds original files with their tags, art and lyrics intact, so pointing these
readers at the cache file makes them work unchanged.

| Item | Server-only song |
|---|---|
| Tags | from the cache file; from cached server metadata until the download finishes |
| Artwork | embedded art, then the cover fetched during the update, then a placeholder |
| Lyrics | embedded first; if none, `getLyricsBySongId` during play, then cached |
| ReplayGain | the server's `replayGain` values, also present in the file's tags |
| MPRIS and macOS Now Playing | `file://` paths to cache files, never stream URLs |
| Play count | the local count, as described above |

## Devices and export

### Send To

Send To with server-only songs selected copies them from the playback cache,
downloading first if needed. These device copies are unpaired: device sync
records pairs only against library files, so it will not track tag changes
on them. Server-only songs that are not cached are skipped while offline, with
a count. CD burning follows the same rule and downloads first.

### Exporting local changes for a server

This is how local tag work reaches a server. The user opens Local changes,
chooses a target server, and exports to a USB device or any folder. Free space
is checked first.

- Linked songs are written to the server's own relative path, so copying the
  export onto the server's music folder replaces the old file. Without this
  the server would end up with a second copy, and the matcher would find two
  candidates and link neither.
- Local-only songs are written to their path relative to their watched
  folder. Each is checked against the server's cached paths, and a path that
  already holds a different song on the server produces a warning.
- A server with several libraries gets one subfolder per library.
- The whole file is copied, so the local audio and tags arrive intact.
- Export never deletes anything.
- A `sparkamp-export.txt` in the export folder says where to copy it and
  lists the files.

After the user copies the export onto the server and Navidrome rescans, the
next update finds the server copies equal to the local ones and the rows turn
in-sync by themselves. The songs get new IDs on the server, and the links
survive because they are keyed by path. Local changes shows the export batch,
for example "exported 2026-10-01: 37 files, 30 in sync, 7 pending".

SMB later uses the same export with the mounted server folder as destination,
plus `startScan`.

## Album gallery

`albums()` already merges album rows in Rust, keyed by album and effective
album artist. It takes two inputs now: local rows, including linked songs, and
server songs shown as their own rows. A linked song counts once, so 5 local
and 7 server-only songs make an album of 12. `album_tracks()` reads both the
same way.

Album tiles get a badge in the lower left: on this computer, on a server, or
both, with a tooltip such as "5 of 12 on this computer, 9 of 12 on a
server". The gallery takes the Files source filters too (All, Local, each
server, Local changes, Needs attention), each folding only the songs that
filter lists, so a tile's count and badge describe what opening it shows. Art comes from local artwork first, then
the server cover, then a placeholder. Server-only albums are dimmed while
offline unless all their songs are cached.

## Offline and unreachable servers

Offline is a normal state, not an error. Availability is tracked per server,
never globally. One server failing never blocks another server, the UI, or
playback of anything else.

Every server call runs on a background thread. A failure marks that server
offline. The next attempt happens on the next trigger listed under "When
Sparkamp talks to a server", or when the OS reports a network change:
`NWPathMonitor` on macOS, `gio::NetworkMonitor` in GTK. The TUI has no
monitor and learns from failures. Core stays UI-agnostic and only takes a
network-changed hint from the frontend.

| Symptom | Treated as | Status bar |
|---|---|---|
| Timeout, connection refused, DNS failure | offline | `oscar: not responding, updated 3h ago` |
| HTTP 5xx, or a proxy's maintenance page | offline | same |
| A response that is not Subsonic, such as a captive portal | offline | same |
| TLS error on the remote URL | offline, noted once | `oscar: certificate problem` |
| Subsonic auth error 40, 41 or 44 | needs the user | `oscar: sign-in failed`, automatic tries stop until the credentials change |
| Server reachable but scanning | online, pull postponed | `oscar: scanning, update postponed` |

The OS network hint only changes the wording: "no network" when the machine
is offline, "not responding" when the network is up but the server is not.
That covers server maintenance while the laptop is online.

What keeps working offline:

- The whole library, search, filters, indicators and playlists, from the
  catalog cache. Sync marks freeze and the tooltip says "as of" the last
  update.
- Songs with a local copy, and cached server songs.
- The Local changes export, which compares against the cached server state.

Server-only songs that are not cached stay listed but dimmed. Playing one
skips to the next playable song and shows one status message, not a dialog per
song. A song on an unreachable server and a reachable one stays fully
playable.

Queued work is stored in the database so it survives a restart: ratings, one
per song with the latest value, and scrobbles with their play times. When a
server becomes reachable, Sparkamp sends ratings first, then scrobbles, then
runs the update if one is due.

## Deletion rule

Deleting a server-only song is disabled, with a tooltip explaining that the
server offers no way to delete. Deleting a linked song from the Media Library
file view deletes only the local copy, after confirmation that says the song
stays on the server. The row becomes server-only. Removing songs from
playlists never deletes anything, as before.

## Security

- Passwords only in the OS keychain, as described above.
- Every request URL carries `u`, `t` and `s`, or later `apiKey`. One core
  function builds requests, and the HTTP error type's `Display` never includes
  the query string, for example `GET /rest/search3 -> 503`. The protection has
  to live in the error type because there are 92 `eprintln!` call sites and a
  panic hook that appends to `crash.log`. A test checks that no error variant's
  text contains `t=`, `s=`, `p=` or `apiKey=`.
- Stream URLs appear nowhere else: not in playlists, not in MPRIS, not in
  the config, not in cache file names.
- Token auth on plain HTTP can be sniffed and replayed, and the md5 cracked
  offline if the password is weak. That is why plain HTTP is allowed only on
  the LAN URL, and why Sparkamp moves to API keys once Navidrome ships them.
- macOS 15 and later ask permission for local network access. The app's
  plist has no `NSLocalNetworkUsageDescription` today, and it needs one. If
  the user denies access, the LAN URL fails and the remote URL takes over.
  `com.apple.security.network.client` is already granted for gnudb. minreq
  does not go through App Transport Security, so plain HTTP on the LAN works.

## Dedupe

`find_duplicates` keeps receiving local tracks only, and its delete action
still follows the deletion rule. Linked copies are one song and can never show
as duplicates. The only change is that the matcher reuses `dedupe::normalize`.

## TUI

- Sidebar filter entries under Files, as above.
- The three-cell indicator column with the ASCII fallback setting.
- A status line per server, for example `oscar ✓ 2h, server2 × 3h`.
- The Copies panel, the resolve table with arrow keys choosing a value per
  field, explicit refresh, make available offline, and link and unlink. Key
  letters are chosen against the existing keymap in the TDD plan.
- Full server setup with a masked password prompt.

## Testing and order of work

Core first, then TUI, then GTK and macOS, per CLAUDE.md. Tests never touch a
real server. HTTP goes behind a transport trait with a fake, and responses
come from JSON fixtures shaped like Navidrome's. oscar is not contacted until
the user says so.

Building and testing happen on this Mac for now. GTK and GStreamer paths are
compiled out on macOS, so Linux verification waits for the Linux machine.
`cargo build && cargo test` must pass with zero warnings.

The TDD plan will break this into slices. A rough order:

1. Request building, token auth with an injected salt, error redaction, and
   parsing of fixture responses.
2. Storage: `server_tracks`, membership, never-link pairs and the queue,
   plus the catalog update with the complete-pull, scanning and mass-removal
   rules.
3. Failing tests for the existing hazards: `save_playlist_tracks` dropping
   entries, `record_play` ignoring URIs, and the broken-versus-unavailable
   state.
4. The matcher and manual link and unlink.
5. The field-by-field merge, rating and play count rules.
6. The merged list, filters and indicators in core.
7. The playback cache, prefetch and song URIs.
8. Playlists, including `#SPARKAMP-SONG` lines.
9. Offline state per server and the send queue.
10. Export for a server.
11. TUI, then GTK, then macOS.

## Open questions

- How the mass-removal guard asks: where it shows, what it shows, the
  choices, what held songs look like meanwhile, whether a hold ever applies
  itself. Needs a longer discussion.
- Start delay for server songs: the whole file downloads before it plays, so
  a FLAC picked by hand from away from home can take ten seconds or more.
  Transcoding when remote, or true streaming, would shorten it.
- Where the Flatpak manifest lives; deferred to a session on Linux. It is not in `packaging/`, so its network
  and secret permissions could not be checked. The keychain crate follows
  from the answer: the `oo7` crate works through the secret portal inside a
  Flatpak, while `keyring` needs a D-Bus talk permission.
- Turning on Report Real Path for Sparkamp's player on oscar. That is a server
  setting and the user's action, when ready.
- What Navidrome does to playlist entries when a song's ID changes. To be
  tested on oscar once allowed.
- The largest page size Navidrome accepts for `search3`, the cover thumbnail
  size, and the default playback cache size.
- How to strip the library root from real paths when a server has several
  libraries. `getMusicFolders` names the libraries, but the root paths may
  need a setting.
- Exact TUI key letters.
