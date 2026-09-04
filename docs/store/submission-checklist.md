# What is still needed before the listing can go live

First checked against the tree on 2026-09-02. Revised 2026-09-04, when most of
what was blocking had been built. Everything under "Blocks the upload" is
verified against the tree rather than remembered.

## Blocks the upload

### Screenshots

Still the only engineering-side blocker. `docs/screenshots/` holds a README
describing five and no images. At least one is required. Sizes: 1280x800,
1440x900, 2560x1600 or 2880x1800.

Josef is taking these.

## Done

### The 1024x1024 app icon

Done 2026-09-03. `Assets.xcassets/AppIcon.appiconset/icon_512x512@2x.png` is
the 1024px variant, cut from `logo 1024x1024.png` at the repository root. That
master is force-added, because `.gitignore` excludes `*.png`.

### The three Info.plist keys

Done 2026-09-03, all three present in `frontends/SparkampMac/Info.plist`:

| Key | Value |
|---|---|
| `LSApplicationCategoryType` | `public.app-category.music` |
| `ITSAppUsesNonExemptEncryption` | `false` |
| `NSHumanReadableCopyright` | `2026 Josef Schelch` |

`ITSAppUsesNonExemptEncryption` became relevant on 2026-09-02, when the gnudb
client moved to HTTPS. It was genuinely absent before that. `false` is the
standard TLS exemption, and without the key every submission stops to ask.

### A privacy policy

Written 2026-09-03 as `PRIVACY.md` at the repository root, and linked from
Settings, About. It covers what stays on the device, the single outbound
request to gnudb, what that request carries, and the fact that leaving the
email field blank is a real option.

One thing is not finished: the URL. GitHub serves the file from the branch it
is on, so the link 404s until this branch merges to main. App Store Connect
needs a URL that resolves at review time.

### The certificates and the app record

Done by Josef 2026-09-02. Apple Distribution and Mac Installer Distribution
are both installed. The App Store Connect record against
`com.sparkamp.sparkampmac` already existed.

### The age rating and category

Done. Nothing here rates above 4+. Category is Music.

## Settled: the deployment target is macOS 26.0

```
MACOSX_DEPLOYMENT_TARGET = 26.0     (both configurations)
```

Intentional (Josef, 2026-09-02). macOS 26 and later is the supported floor.

`OSX.plan` says macOS 13 in three places, and that is stale rather than
contradictory: it is the original port plan, written before this build existed,
and its "Out of Scope (v1)" section lists App Store distribution and Touch Bar,
both of which shipped. Anything older than 26 cannot be tested here, and a
compatibility claim nobody can verify is worth less than an honest floor.

## Settled: the sandbox questions, answered by running it

The 2026-09-02 version of this document called the sandboxed build "the one
with unknown unknowns in it". It has now been built and run repeatedly, and the
unknowns turned into a list:

- **`files.removable-media.read-write` does not do what its name suggests.** It
  grants nothing inside a mounted volume. Every USB volume and every optical
  data disc returned EPERM on `read_dir` under the shipping entitlements. The
  fix is a user-selected path plus a security-scoped bookmark, which is what
  `VolumeAccess.swift` and the `volume_grants` table are for.
- **CD-TEXT reading a raw device node is allowed.** This was listed as
  unsettled. `/dev/rdiskN` is readable in the sandbox, which is why audio CD
  detection, playback and ripping all work through the raw device rather than
  through the mount.
- **Subprocesses are not.** `Process()` is forbidden, so eject moved from
  shelling out to `drutil` into the core, through DiscRecording.

## Decided against: DMG to App Store library migration

Moving a DMG install's config and library into the sandbox container is not
being built. Recorded 2026-09-03 in commit `2ea606c`. A sandboxed app cannot
read `~/Library/Application Support/sparkamp` without being handed it, so the
flow would need its own `NSOpenPanel` step, and the two builds can coexist.

## Legal, not engineering

- The CLA is AI-drafted and unreviewed.
- The ReplayGain coefficient provenance argument rests on merger doctrine and
  should be put to the same lawyer at the same time.
