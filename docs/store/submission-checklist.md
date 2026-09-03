# What is still needed before the listing can go live

Checked against the tree on 2026-09-02, not copied from a generic checklist.
Everything here is either missing or unverified.

## Blocks the upload

### The 1024x1024 app icon is missing

`Assets.xcassets/AppIcon.appiconset/` has every size from 16x16 through
512x512, and no `icon_512x512@2x.png`. The App Store requires the 1024px
variant and the upload fails without it.

### Info.plist is missing three keys

| Key | Why |
|---|---|
| `LSApplicationCategoryType` | Required for the App Store. `public.app-category.music`. |
| `ITSAppUsesNonExemptEncryption` | Set `false`. Sparkamp uses TLS through rustls for gnudb, which is the standard exemption, but without the key every submission stops to ask. |
| `NSHumanReadableCopyright` | Shown in the About window. `2026 Josef Schelch`. |

`ITSAppUsesNonExemptEncryption` became relevant on 2026-09-02, when the gnudb
client moved to HTTPS. It was genuinely absent before that.

### Screenshots

None exist. `docs/screenshots/` holds a README describing five and no images.
At least one is required. Sizes: 1280x800, 1440x900, 2560x1600 or 2880x1800.

### A privacy policy URL

Required, and doubly so now that the privacy label declares Contact Info,
Email Address. There is no privacy policy anywhere in the repo or linked from
the README. It needs writing and hosting somewhere stable.

The content is short, because the facts are short. No analytics, no account,
no advertising identifier, no tracking. One outbound request, to gnudb, which
carries a disc ID and, if the user has filled the field in, their email
address split into username and hostname. Everything else stays on the machine.

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

## Account work only Josef can do

- An **Apple Distribution** certificate. Xcode, Settings, Accounts, Manage
  Certificates, the plus button. Not needed to test the sandbox locally, only
  to submit.
- A **Mac Installer Distribution** certificate, for the upload.
- An app record in App Store Connect against `com.sparkamp.sparkampmac`. One
  already exists.
- The age rating questionnaire. Nothing here rates above 4+.
- A category. Music.

## Engineering still open

- **The sandboxed build has never been run.** This is the one with unknown
  unknowns in it. v1.3.3's own release notes record eight failures the Flatpak
  sandbox surfaced that no test caught. The macOS sandbox will have its own
  list and nobody has looked yet. An Apple Development certificate is enough to
  find out.
- **Config and data migration** from a DMG install into the sandbox container.
  Not built. Needs an `NSOpenPanel` flow, because a sandboxed app cannot read
  `~/Library/Application Support/sparkamp` without being handed it.
- **CD-TEXT reads a raw device node.** Whether the sandbox allows it is
  unsettled and the sandboxed run answers it.

## Legal, not engineering

- The CLA is AI-drafted and unreviewed.
- The ReplayGain coefficient provenance argument rests on merger doctrine and
  should be put to the same lawyer at the same time.
