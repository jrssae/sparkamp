# Sparkamp privacy policy

**Last updated: 3 September 2026**

Sparkamp is a music player. It has no account, no sign-in, no analytics, no
advertising and no tracking.

Nothing about you or your listening is sent to the developer. There is no
server to send it to.

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

None of it leaves your device. Sparkamp does not upload, back up or
synchronise any of it. Deleting the app's data folder, or the app, removes it.

The crash log is written to a local file for you to read. It is never
transmitted anywhere.

---

## The one connection Sparkamp makes

Sparkamp makes network requests to exactly one service, and only when you ask
it to look up or submit information about a compact disc.

That service is **gnudb**, at `gnudb.gnudb.org`, a free community database of
CD track listings. It is not run by the developer of Sparkamp.

### When it happens

Only when you use a disc feature that needs it: identifying a CD you have
inserted, or submitting a correction back to the database. Playing your own
files never contacts anything. Neither does anything else in the app.

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
- No accounts, and no passwords to store

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
control. Deleting the application and its data folder removes all of it.

---

## Verifying any of this

Sparkamp is free software under the AGPL-3.0, and the complete source is
public. You do not have to take this document's word for it:

- The only outbound requests in the entire codebase are in
  [`src/disc/gnudb.rs`](src/disc/gnudb.rs). That file contains the only use of
  the HTTP library anywhere in the project.
- The value described above is built by `hello_param` in that same file.
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
