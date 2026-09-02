# App Store signing: what is ready, and what only you can do

## What is ready

- `packaging/macos/build-appstore.sh` — archive, export, verify.
- `packaging/macos/export-options-appstore.plist` — `app-store-connect`
  method, manual signing, the profile named explicitly.
- `packaging/macos/entitlements-appstore.plist` — audited against the code as
  it now is.

Run the script. It checks the signing chain first and stops with the list
below if anything is missing, so it is safe to run before setting anything up.

## What only you can do

Three things, none scriptable, all needing an Apple Developer account.

### 1. An "Apple Distribution" certificate

Xcode → Settings → Accounts → Manage Certificates → **+** → Apple Distribution.

This machine already has two **Developer ID Application** certificates and one
**Apple Development**. Neither kind is accepted for App Store submission —
Developer ID is for distribution outside the store, which is what the DMG uses.
Same team (`HR3P54M383`), different certificate.

### 2. An App ID for `dev.sparkamp.Sparkamp`

<https://developer.apple.com/account/resources/identifiers>

**Enable no capabilities.** Every entitlement this build requests is a sandbox
entitlement, and none of them is a capability that needs registering. Adding
capabilities the app does not use invites questions at review.

### 3. A provisioning profile named exactly `Sparkamp Mac App Store`

<https://developer.apple.com/account/resources/profiles> → Mac App Store
distribution, for that App ID and that certificate. Download and double-click
to install.

The name matters: `export-options-appstore.plist` names it, deliberately,
rather than letting Xcode choose. A build that picks its own profile is a build
whose output you cannot reproduce.

For uploading you will also want a **Mac Installer Distribution** certificate.
The export does not need it; the upload does.

## What the script checks before handing you a package

It refuses to produce one that fails its own checks:

- **Sandboxed.** Read back out of the signature, not out of the plist that was
  passed in. An unsandboxed build is rejected, and the rejection does not
  explain itself in those terms.
- **No GStreamer.** This whole effort was about not shipping it. The bundle is
  searched for `*gst*` and `liborc*` rather than trusted.
- **Bundle identifier** is `dev.sparkamp.Sparkamp`.
- **Signature verifies**, `--deep --strict`.

## Two things that will differ from the DMG build, and why

**The archive is signed for real.** `build-dmg.sh` archives with
`CODE_SIGNING_ALLOWED=NO` and ad-hoc signs afterwards, which is fine for a
notarised DMG. Doing that here would strip the provisioning profile, and the
failure surfaces at upload as a generic rejection with nothing pointing at the
cause.

**Nothing is bundled.** No dylib walk, no plug-in tree, no launcher script. The
crate does not link GStreamer on macOS, so there is nothing to carry.

## The one thing to test the moment you have a signed build

`live_cdtext_absence_is_quiet`, with the 15-track *Bespoke Bounce* disc in the
drive.

Reading CD-TEXT opens the media's raw BSD node, and whether the App Sandbox
permits that is the top open question — see the sandbox readiness audit. That
test distinguishes "the sandbox denied it" from "this disc has no CD-TEXT",
which is exactly the distinction that matters and exactly the one a manual
check would get wrong.
