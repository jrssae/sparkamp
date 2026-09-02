# The licence question

> **Not legal advice.** This is a factual read of how the mechanism works,
> assembled from the repository's own history and dependency set. A lawyer
> should confirm before anything ships.

## Bottom line

The blocker you were worried about is probably not one, and the route that
avoids it needs **no change to the AGPL at all**.

Two facts settle most of it, and both were checked rather than assumed:

1. **You are the sole copyright holder.** All 893 commits, across four author
   spellings, are the same GitHub identity. There is no other rights holder in
   the tree.
2. **Nothing in the macOS dependency set forces copyleft any more.** Removing
   GStreamer removed the only LGPL dependency. What remains is permissive
   (MIT / Apache-2.0 / BSD / ISC / Zlib) plus MPL-2.0 for symphonia, and
   MPL-2.0 explicitly permits distributing a Larger Work under other terms.

## The premise, corrected

The concern was that "modifying the AGPL will not suffice and Apple will
decline it." Two parts of that are worth separating.

**Apple does not review your licence.** There is no step where App Review reads
a LICENSE file and approves or declines it. Review checks technical and policy
compliance. Nobody at Apple forms a view on your copyleft.

**What actually happened to VLC** — the case this fear traces back to — is that
in late 2010 a VLC *contributor*, Rémi Denis-Courmont, objected that the App
Store's terms conflicted with the GPL, and filed a complaint. Apple removed the
app in January 2011 in response to the complaint. Apple did not evaluate the
licence; it responded to a rights holder asserting infringement. VLC returned in
2013 only after VideoLAN relicensed and obtained contributor agreement.

So the risk is not Apple's opinion. **The risk is a copyright holder with
standing to complain** — and in this tree there is exactly one, and it is you.

## Why there is a conflict at all

GPL-family licences forbid imposing *further restrictions* on downstream
recipients (GPLv3 §10). The App Store terms impose several — device limits and
the usage rules every download is bound by. A licensee who redistributes under
both sets of terms is caught between them.

A **licensor is not a licensee.** The AGPL is a grant you make to other people.
It does not bind you, because you cannot infringe your own copyright.

## Three routes

### 1. Dual distribution — recommended, and needs no licence change

Publish the source under AGPL-3.0 exactly as today. Separately, distribute your
own build through the App Store under Apple's terms. You are not exercising
rights the AGPL granted you; you already hold them.

This is the ordinary dual-licensing model — MySQL, Qt and MongoDB all work this
way. The AGPL text stays untouched, the public repository stays copyleft, and
the App Store binary is simply a different distribution by the rights holder.

The MPL-2.0 dependencies are fine here: §3.3 permits a Larger Work under terms
of your choice, provided the MPL-covered files keep their licence and notices,
which they do upstream.

### 2. AGPL plus an additional permission (GPLv3 §7)

Also workable, and the mechanism is real and widely used. It states in the
licence itself that recipients may distribute through app stores under their
terms. Strictly more generous than route 1: it lets *other people* ship it to
the App Store too.

More drafting risk. An exception that does not name the conflicting requirement
precisely leaves the conflict intact.

### 3. Relicense to Apache-2.0 or MIT

Removes every question permanently, and you can do it unilaterally today. It
also ends the copyleft: anyone may ship a closed fork of Sparkamp, including
to the App Store, without contributing anything back.

That is a decision about what the project *is*, not a packaging detail. It is
not required by anything here.

## What genuinely needs care

**Contributors.** The moment someone else contributes under plain AGPL, routes
1 and 2 stop covering their code, and you would need their agreement. If any
route but 3 is chosen, that needs a contributor licence agreement — or at
minimum a stated policy — **before** the first outside pull request, not after.

**AGPL §13 is not the issue.** The network-service clause is what separates AGPL
from GPLv3, and a desktop audio player triggers none of it. For this project the
AGPL behaves as GPLv3, so the analysis above is the GPL analysis.

## If the App Store is abandoned entirely

The engineering done for it is not wasted, because almost none of it is
App-Store-specific:

| Work | Value without the App Store |
|---|---|
| GStreamer removed from macOS | The DMG stops carrying ~40 MB of dylibs and a shell-script launcher |
| AVFoundation backend | Native playback; three more formats than the bundle could decode |
| DiscRecording port | Detection roughly 88× faster; no subprocess spawns |
| CD-TEXT writing | A feature macOS never had, on both platforms |
| Burn returned success while writing nothing | A real bug, found and fixed |
| CD-TEXT unreadable on any over-reporting drive | A real bug, pre-existing, fixed |
| Erase-then-burn failed outright | A real bug in the "erase and burn" button, fixed |
| Security-scoped bookmarks | Inert outside a sandbox; costs nothing |

The genuinely App-Store-only work is the entitlements file, the bundle
identifier, and signing configuration. That is a small fraction of it.

## What to decide

Route 1 unless there is a reason to want other people to be able to ship it to
the App Store as well, in which case route 2. Route 3 only if ending the
copyleft is something you want for its own sake.

None of it is blocked on the engineering, and none of the engineering is blocked
on it.
