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
Now rips CDs straight to FLAC, writes CD-TEXT when it burns, and measures
ReplayGain without leaving the app. Every format your library actually has.
```

147 characters.

## Description (4000 characters)

```
Sparkamp is a music player for people who miss when a player was a player: a
window, a playlist, an equalizer you could actually see, and a skin you chose
yourself.

It plays your files. All of them — MP3, FLAC, AAC, M4A, WAV, AIFF, Ogg Vorbis
and Opus — from wherever you keep them, without asking you to import anything
into a walled garden first.

PLAYBACK
• Ten-band graphic equalizer with preamp, and presets that stay put
• ReplayGain, so a shuffled playlist stops lurching between quiet and loud
• Gapless-friendly transport with crossfade-free honest stops and starts
• Visualizer with spectrum, oscilloscope and plasma modes

YOUR LIBRARY, NOT A SERVICE
• Watch folders that notice new music without a manual rescan
• Album gallery with cover art
• Search across artist, album, title, genre and year
• Tag editor for fixing what the internet got wrong
• Play counts and last-played, kept locally

COMPACT DISCS, STILL
• Play an audio CD, with track names read from the disc's own CD-TEXT
• Rip to FLAC — lossless, tagged, and named from CD-TEXT or an online lookup
• Burn audio CDs that carry CD-TEXT, so the next player shows the titles
• Burn data discs, and erase rewritables
• ReplayGain analysis over a whole album at once, measured as one album rather
  than averaged from its tracks

MAKE IT YOURS
• Skinnable, with the classic look on by default and light and dark variants
• Touch Bar controls
• Keyboard-driven throughout — every transport, view and panel has a key

Sparkamp is free and open source under the AGPL-3.0. The complete source is
public, and it is the same source this build was made from.
```

Roughly 1,600 characters — well inside the limit, and short enough to be read.

## Keywords (100 characters, comma separated)

Spaces count against the limit, so there are none after the commas. The app
name is already indexed and would be wasted here.

```
mp3,flac,equalizer,skins,playlist,audio,player,CD,ripper,visualizer,library,ogg,opus,aac,tags
```

92 characters.

## Copyright

```
2026 Josef Schelch
```

App Store Connect adds the © itself; do not type one.

## Support and marketing URLs

- Support URL is **required**. `https://github.com/jrssae/sparkamp/issues`
- Marketing URL is optional. `https://github.com/jrssae/sparkamp`

## Age rating

Nothing in the app warrants anything above 4+. It plays local files and makes
one outbound request, to gnudb, for disc metadata. Answer no to every content
question.

## Privacy

The nutrition label needs answering even though the answer is "none". Sparkamp
collects nothing, has no analytics, no account and no advertising identifier.
The one network request is a disc lookup, sends only a disc ID, and is not tied
to a person.

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
