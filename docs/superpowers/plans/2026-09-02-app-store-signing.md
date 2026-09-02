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

**One thing**, and it is a single click:

    Xcode → Settings → Accounts → <your Apple ID> → Manage Certificates
          → the + button, bottom left → Apple Distribution

That is it. This machine has three **Developer ID Application** certificates
and one **Apple Development**, and neither kind is accepted for App Store
submission — Developer ID is for distribution outside the store, which is what
the DMG uses. Same team (`HR3P54M383`), different certificate. Automatic
signing selects a certificate; it cannot create one.

### Why the App ID and profile are not on this list

They were, and they should not have been. The build passes
`-allowProvisioningUpdates` with automatic signing, so Xcode registers the App
ID for `dev.sparkamp.Sparkamp` and creates the Mac App Store provisioning
profile itself during the archive.

The first version of this document sent you to the developer portal for both,
on the argument that a build which picks its own profile is one you cannot
reproduce. That argument is sound and it was still the wrong call: the portal
is where the mistakes happen — a stray click there produced a fourth Developer
ID certificate that nobody wanted — and reproducibility on a single-maintainer
project is worth less than not having to go there at all.

**Switch back to manual** if this ever builds somewhere other than one person's
machine. On CI, or with a second maintainer, "whatever Xcode chose" stops being
knowable and the original argument starts being right.

### If you do end up in the portal

It is under **Identifiers**, not "App IDs" — that is the thing that is hard to
find. "App IDs" is the *type* you pick after clicking the **+**.

<https://developer.apple.com/account/resources/identifiers/list>

**+** → App IDs → Continue → App → Continue → Description `Sparkamp`, Bundle ID
**Explicit** = `dev.sparkamp.Sparkamp` → leave every capability unchecked →
Continue → Register.

Leave no capability ticked. Every entitlement this build requests is a sandbox
entitlement, and none is a capability that needs registering; adding unused
ones invites questions at review.

### On spare certificates

Developer ID Application certificates are limited to five per account. Three
are in use here. Extra ones are harmless — nothing references them by name —
and revoking to tidy up carries more risk than leaving them.

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
