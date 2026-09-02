# The licence question, reopened

> **Status: open. Nothing here is decided, and this is not legal advice.**
> Raised by Josef, 2026-09-02, against an earlier working decision that has not
> survived contact with the problem.

## What changed

The plan of record was **keep AGPL-3.0, add an App Store exception**. Josef's
position now: an exception bolted onto AGPL-3.0 will not suffice and Apple will
decline it, so the alternative on the table is **relicensing the project
outright** — Apache 2.0 was named as the example, not as a conclusion.

That is a project-level decision with consequences well outside packaging, which
is why it is a discussion rather than a task.

## Why the exception may not be enough

The conflict is not about the AGPL's copyleft as such. It is that GPL-family
licences forbid imposing *further restrictions* on recipients, and the App Store
terms impose several — device limits and the usage rules every download is bound
by. This is the ground VLC was removed over.

The usual remedy is an additional-permission clause under GPL §7 written by the
copyright holder, and it has two failure modes worth naming before anyone drafts
one:

1. **It only works if the licensor holds all the copyright.** Any outside
   contribution under the unmodified AGPL cannot be relicensed or excepted
   without that contributor's agreement.
2. **It has to actually cover what Apple requires**, not merely gesture at it.
   An exception that names the wrong restriction leaves the conflict intact.

Neither is settled here. Both are checkable, and the first is checkable from the
git history.

## What relicensing would cost

Apache 2.0 removes the conflict, and removes the copyleft with it. That is a
real change in what the project is, not a packaging detail — anyone may ship a
closed derivative. It also needs the agreement of every copyright holder, the
same question as (1) above.

There is one thing working in favour of a clean answer: the App Store build no
longer links GStreamer (LGPL). Whatever the shipped macOS dependency set turns
out to be, it is smaller and simpler than it was when the AGPL decision was
made, so the constraints imposed *by dependencies* should be re-derived rather
than assumed.

## What the discussion needs

- Who holds copyright on the tree today — a `git shortlog -sne` on the real
  history, not an assumption.
- The licence of every dependency the **App Store** build actually links, now
  that GStreamer is gone from it.
- Whether the goal is "ship on the App Store" or "ship on the App Store *and*
  keep copyleft", because those may not both be available.
- If relicensing: which licence, and whether the AGPL history stays as a dual
  offer or is replaced.

## What this does not block

The engineering work does not depend on the answer. The sandbox port, the
DiscRecording work and the audio backend switch are all licence-neutral, and
none of them becomes wasted effort under either outcome.
