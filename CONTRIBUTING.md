# Contributing to Sparkamp

Contributions are welcome — bug fixes, features, refactoring, documentation,
design feedback. The codebase was AI-generated, so there are certainly places
where a human would make different and better choices. Point them out, or just
fix them.

Please open an issue before starting large feature work, so it can be
coordinated rather than duplicated.

## One thing to do first

Read [the CLA](CLA.md), and say this on your first pull request:

> I have read the Sparkamp CLA and I agree to it.

It is short, and section 8 is the part worth reading: whatever else happens to
your contribution, it stays under an OSI-approved open-source licence. It
cannot be taken closed.

**Why it is asked for.** Sparkamp is AGPL-3.0, and the AGPL cannot be handed to
some distribution channels — the Mac App Store among them, whose terms conflict
with it. A project can still be distributed through those channels *by its
copyright holder*, because a licensor is not bound by the licence they grant to
others. That only works while the rights stay consolidated, and it stops
working the moment a contribution arrives with no agreement attached.

Asking now costs a sentence. Asking afterwards means finding every past
contributor and hoping they answer; VLC lost years to exactly that.

## Building and testing

See [the README](README.md) for build instructions per platform.

Before opening a pull request:

```bash
cargo fmt
RUSTFLAGS="-D warnings" cargo build --lib
cargo test --lib
cargo check --all-targets
```

`cargo check --all-targets` is not optional. The binary and the library have
separate module trees, and a change that compiles as one can fail as the other
— it has broken CI here more than once.

Tests marked `#[ignore]` need hardware (an optical drive, a disc) or sample
files the repository does not carry. They are run by hand; each says what it
needs in its doc comment.

## What the code expects of itself

- **Platform differences live behind a seam, not in the middle of core logic.**
  `engine::backend`, `disc::transcode` and `duration_probe::platform` are the
  pattern: the trait speaks the vocabulary of the job, adapters speak to the
  operating system, and nothing above the seam knows which one it is talking to.
- **Prefer deleting a text parser to porting it.** Several ports here replaced
  scraped subprocess output with structured API values, and left the parser
  behind. Don't.
- **A test that would pass if the code did nothing is not a test.** Read values
  back off the object under test, not out of the field you just set.
