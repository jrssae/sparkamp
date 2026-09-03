# App Store Connect listing copy

Draft copy for the Mac App Store listing. Kept in the repo so it is versioned
alongside the features it describes — a listing that promises something the
build no longer does is a rejection, and a listing nobody can find the source
of is one that drifts.

## Rules these were written against

- **No "Winamp".** It is a registered trademark. Using it in the name,
  subtitle, keywords or description risks rejection under Guideline 5.2 and a
  trademark complaint besides. The README uses it freely; a store listing
  cannot.
- **No other platforms.** No Linux, GTK, Flatpak or terminal mode. Apple's
  review does not want a macOS listing advertising somewhere else.
- **Nothing the macOS build does not do.** Every claim below is a feature that
  ships in this build, on this platform.
- **No pricing, no "beta", no roadmap.**

---

## Name (30 characters)

```
Sparkamp
```

## Subtitle (30 characters)

```
Classic music player, modern
```

28 characters. Alternatives if that reads oddly: `Music player with a past` (24),
`Skinnable player and library` (28).

## Promotional text (170 characters)

Changeable at any time without submitting a new build, so it is the right place
for anything seasonal or recent.

```
Now available on the App Store
```

30 characters.

## Description (4000 characters)

Josef's revision, 2026-09-03. Kept verbatim.

```
Sparkamp is a music player for people who miss the features that were considered standard in the 2000s: a window, a playlist, a media library organizing your library, an equalizer, and the option to customize the look for yourself.

It plays your music files: MP3, FLAC, AAC, M4A, WAV, AIFF, Ogg Vorbis, and Opus. No matter where you got them, if they are using open standards, you can play it.

PLAYBACK
• Ten-band graphic equalizer with preamp, and presets that stay put
• ReplayGain, so a shuffled playlist stops lurching between quiet and loud
• Visualizer with spectrum, oscilloscope and plasma modes
• Jump to specific songs in your playlist
• Queue management

YOUR MEDIA LIBRARY
• See all of the files on your computer in a standardized view, regardless of folder structure
• Search across artist, album, title, genre and year
• Watch folders that notice new music without a manual rescan
• Album gallery with cover art
• Metadata tag editor for fixing what the internet got wrong
• Play counts and last-played, kept locally

COMPACT DISCS AND DVDS
• Play an audio CD, with track names read from the disc's own CD-TEXT
• Rip to FLAC; lossless, tagged, and named from CD-TEXT or an online lookup with your own custom overrides
• Burn audio CDs that carry CD-TEXT, so the next player shows the titles
• Burn data discs (tested with CDs and DVDs) and erase rewritable discs
• ReplayGain analysis over a whole album at once, measured as one album rather
  than averaged from its tracks

USB MEDIA PLAYERS AND DRIVES
• Easily view and manage music on portable music players
• Sync files and playlists with a single button
• Drag and drop file support
• Manage files on USB drives for moving between computers

MAKE IT YOURS
• CSS Skins support, with light and dark defaults included (colors only for now)
• Touch Bar controls
• Keyboard-shortcuts throughout

Sparkamp is free and open source under the AGPL-3.0. The complete source is public and it is the same source this build was made from.
```

> The "tested with CDs and DVDs" claim is true as of 2026-09-02. CD-R, CD-RW
> and DVD+RW have all had a data burn, a readback and an erase against real
> hardware. See the sandbox readiness audit.

## Keywords (100 characters, comma separated)

Spaces count against the limit, so there are none after the commas. The app
name is already indexed and would be wasted here.

```
mp3,flac,media,library,equalizer,playlist,player,CD,ripper,visualizer,usb,drive,device,ID3,tags
```

**95 characters, 5 to spare.** Anything added has to come out of something
else.

## Copyright

```
2026 Josef Schelch
```

App Store Connect adds the © itself; do not type one.

## Support and marketing URLs

- Support URL (**required**): `https://github.com/jrssae/sparkamp/issues`
- Marketing URL: `https://github.com/jrssae/sparkamp`

## Age rating

Nothing in the app warrants anything above 4+. It plays local files and makes
one outbound request, to gnudb, for disc metadata. Answer no to every content
question.

## Privacy

There is no analytics, no account, no advertising identifier and no tracking.
There is one thing to declare, and it is not nothing.

**Contact Info → Email Address.** gnudb speaks CDDB, whose `hello` handshake
carries a `username+hostname` pair. Sparkamp builds that from the address in
Settings by splitting it at the last `@`, and sends it on **every lookup**, not
only on submissions — see `disc::gnudb::hello_param`. Submitting a correction
back to gnudb requires a real address; looking a disc up does not, and an
unset address sends `anonymous+localhost` instead.

So the honest label is:

| | |
|---|---|
| Data type | Contact Info → Email Address |
| Purpose | App Functionality |
| Linked to the user | Yes — an email address identifies a person |
| Used for tracking | No |

Declare it even though it is optional and off by default. The label describes
what the app **can** transmit, and a user who fills that field in is
transmitting it.

Nothing else leaves the machine. The disc lookup also sends a disc ID, which is
a hash of the table of contents — a property of the pressing, not of the person
holding it.

Those requests go over **HTTPS** as of 2026-09-02. They did not before, which
is how this section came to be written — see
`docs/superpowers/plans/2026-09-02-gnudb-cleartext-email.md`.

---

## Screenshots

**Required.** At least one; up to ten. Accepted macOS sizes:

| | |
|---|---|
| 1280 × 800 | 1440 × 900 |
| 2560 × 1600 | 2880 × 1800 |

Capture at 2560 × 1600 on a Retina display and it is accepted directly.

`docs/screenshots/README.md` lists the five the Flathub listing uses, and the
same set works here. Its conventions apply — built-in Dark skin, no personal
metadata in frame, window only:

1. **Player** — a track playing, visualizer running. This is the first one
   anybody sees; make it the one that explains the app.
2. **Playlist** — populated, not empty.
3. **Media library** — sidebar visible, a real library behind it.
4. **Album gallery** — cover art, because this is the shot that reads as
   "modern" against the classic player window.
5. **Disc** — a CD loaded with its CD-TEXT titles showing. Nothing else in the
   category does this, so it is worth a slot.

Use music you are willing to publish. These end up on a public page.
