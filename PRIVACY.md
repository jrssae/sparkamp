# Sparkamp privacy policy

**Last updated: 3 October 2026**

Sparkamp is a music player. It has no Sparkamp account, no analytics, no
advertising and no tracking.

Nothing about you or your listening is sent to the developer. There is no
server to send it to.

Sparkamp connects to two kinds of place, and only these: gnudb, when you look
up a CD, and any music server you add yourself, such as your own Navidrome.
Both are described below.

This policy covers every build of Sparkamp: the Mac App Store version, the
downloadable macOS disk image, and the Linux builds.

---

## What stays on your device

Sparkamp keeps its working data in your own user folder, and nowhere else:

- Your media library index: file paths, and the tags read from those files,
  such as artist, album, title, genre and year
- Your playlists
- Your settings, including your equalizer presets and skin choice
- Play counts and last-played dates
- A crash log, if Sparkamp ever crashes
- If you add a music server: its address and your username on it, a copy of
  its catalog (titles, tags and where each song sits on the server), its
  playlists, cover art, and the songs you played recently, kept in a playback
  cache capped at 128 MB unless you change it

None of it is sent to the developer, and Sparkamp does not back any of it up.
Only two things leave your device: disc lookups sent to gnudb, and what goes
to a music server you added. Both are described below. Deleting the app's data
folder, or the app, removes everything listed here.

The crash log is written to a local file for you to read. It is never
transmitted anywhere.

---

## Disc lookups: gnudb

Sparkamp contacts gnudb only when you ask it to look up or submit information
about a compact disc.

That service is **gnudb**, at `gnudb.gnudb.org`, a free community database of
CD track listings. It is not run by the developer of Sparkamp.

### When it happens

Only when you use a disc feature that needs it: identifying a CD you have
inserted, or submitting a correction back to the database. Nothing else in
the app contacts gnudb.

### What is sent

**A description of the disc.** The disc's own identifier, the start position
of each track, and the disc's total playing time. This describes the disc, not
you. Any two people with the same album send the same values.

**An identifier built from your email address, if you have set one.** gnudb
speaks the CDDB protocol, which requires every request to carry a "hello"
identifying the client. Sparkamp builds that from the address in Settings by
splitting it at the last `@`, so `jane@example.org` is sent as
`jane+example.org+Sparkamp+<version>`.

Two things about this are easy to assume wrongly:

1. **It is sent on every lookup, not only on submissions.** The protocol
   carries it on all requests.
2. **Leaving the address blank is a real option.** Lookups work perfectly
   well without one, and Sparkamp then sends `anonymous+localhost` instead of
   anything about you.

An address is only genuinely required if you want to **submit** a disc
correction back to gnudb, which is a deliberate action you have to take, and
which gnudb requires so that contributions are attributable.

Your email address is stored in Sparkamp's local settings file on your own
device. It is not sent anywhere except to gnudb as described above, and it is
never sent to the developer.

### What gnudb does with it

Once a request reaches gnudb, gnudb's own practices apply, not this policy.
Sparkamp has no control over and no visibility into what they log or retain.
If you would rather not send an address, leave the field empty.

---

## Music servers you add

Sparkamp can play music from a server you add, such as Navidrome or another
server that speaks the Subsonic API. Until you add one, none of this happens.
Sparkamp ships with no server, and none of these requests go to the developer.

### When it happens

Only with servers you have added, and only at the addresses you gave for them:

- When Sparkamp starts, if you turned that on
- On the update schedule you set, every 24 hours by default
- When you press Test, or ask for a refresh
- When you play, or are about to play, a song that lives on the server

### What is sent

**Your username and a sign-in token.** Every request carries your username and
a token made from your password and a random value. Your password itself is
never sent, but anyone who sees the token can use it to sign in as you until
you change your password. That is why the next point matters.

**Over HTTPS, unless you choose otherwise.** The remote address must use
HTTPS. The home address may use plain HTTP, which is common for a server on
your own network. On plain HTTP, anyone on the same network can read the token.
Sparkamp warns you if a plain-HTTP address is outside your home network.
Sparkamp never follows a redirect, so the token only ever goes to the address
you entered.

**Requests for your music.** The catalog, playlists and cover art, and the
songs you play. The server sees which songs you browse and stream, your IP
address, and that the requests come from Sparkamp and which version.

**What you played, and when.** When a song the server holds counts as
played, by the same rule that adds to your play counts, Sparkamp tells the
server which song and the time. This includes plays
of your own local files that Sparkamp has matched to a song on that server, so
the server's play history stays complete. Plays made while the server is out of
reach are kept on your device and sent at the next update.

**Ratings you set.** If you rate a song the server holds, the rating is sent
to the server.

Sparkamp does not change anything else on a server. It does not upload files,
edit tags, or create or delete playlists there.

### Where your password is kept

On macOS, in your login Keychain. On Linux, only in memory until Sparkamp
quits, so you enter it again each session. It is never written to Sparkamp's
settings file. Removing a server deletes its stored password and its cached
catalog. Your music files are not touched.

On macOS, the system asks once for permission to use your local network, the
first time Sparkamp reaches a server on your home network. That permission
covers nothing else.

### What the server does with it

Your server keeps whatever it keeps, under its own settings, and this policy
does not cover it. If you run it yourself, that is up to you. If someone else
runs it, their practices apply. A server can also pass your plays on to a
service such as Last.fm or ListenBrainz, if it has been set up to. That is the
server's doing, not Sparkamp's.

---

## Links that open your browser

Sparkamp offers a few convenience links. These do not send anything from
Sparkamp. They hand a web address to your browser, which then makes the
request the way any link you click does:

- **Search for lyrics** opens a DuckDuckGo search for the artist and title of
  the current track
- **Artist and album information** opens a Wikipedia search for that name
- **Licence and source code** links open gnu.org and github.com

If you follow one of these, that site learns whatever your browser tells it.
For the first two, that includes the track you were playing. Their privacy
policies apply, not this one. If you never click them, nothing is sent.

---

## What Sparkamp never does

- No analytics, telemetry, usage reporting or crash reporting to the developer
- No advertising, ad identifiers, or third-party ad and analytics SDKs
- No tracking across apps, sites or devices
- No profiling and no automated decision-making
- No selling, renting or sharing of personal information, because none is
  collected
- No Sparkamp accounts. The only passwords Sparkamp handles are for music
  servers you add, kept as described above

---

## The App Store itself

If you installed Sparkamp from the Mac App Store, Apple handles the download
and the purchase record under Apple's own privacy policy. Apple may also share
aggregate statistics and, if you have opted in, crash reports with the
developer. That is Apple's collection, not Sparkamp's, and Sparkamp contains
no code that reports anything.

---

## Children

Sparkamp is not directed at children and collects nothing from anyone,
including children.

---

## Your rights

Since Sparkamp holds no personal information about you and transmits none to
the developer, there is nothing for the developer to disclose, correct, export
or delete. Everything Sparkamp stores is on your own device and under your own
control. Deleting the application and its data folder removes all of it. What a
music server you added keeps is held by that server, under its own settings.

---

## Verifying any of this

Sparkamp is free software under the AGPL-3.0, and the complete source is
public. You do not have to take this document's word for it:

- The disc lookups are in [`src/disc/gnudb.rs`](src/disc/gnudb.rs). The
  value described above is built by `hello_param` in that file.
- Every request to a music server goes through
  [`src/servers/transport.rs`](src/servers/transport.rs), and on macOS
  [`src/servers/transport_apple.rs`](src/servers/transport_apple.rs). On Linux,
  a song that is still downloading can also stream from the same server, at
  the same address, through GStreamer.
- The sign-in token is built in [`src/servers/auth.rs`](src/servers/auth.rs)
  and [`src/servers/request.rs`](src/servers/request.rs), which also keeps
  the request URLs out of every log and error message.
- What is sent back to a server (plays and ratings) is `send_pending` in
  [`src/servers/sync.rs`](src/servers/sync.rs).
- Passwords are stored by `platform_secrets` in
  [`src/servers/manager.rs`](src/servers/manager.rs).
- The crash log writer is in [`src/crash_log.rs`](src/crash_log.rs), and only
  ever opens a local file.

Repository: https://github.com/jrssae/sparkamp

---

## Changes to this policy

If Sparkamp's behaviour changes in a way that affects this document, the
document will be updated in the same commit, and the date at the top will
change. The file's history is public, so any revision can be compared against
the one before it.

---

## Contact

Questions about this policy, or about Sparkamp's handling of data, can be
raised as an issue at https://github.com/jrssae/sparkamp/issues.
