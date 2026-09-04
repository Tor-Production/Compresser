# ADR 0004 — Version-independent magic

**Status:** accepted (2026-09-03). The principle stands; the byte values were superseded by
ADR 0008, which renamed the format. This document is left as it was written.

## Context

Version 1 used the magic `BRP1` *and* a separate `version` byte. The two encoded the same thing, so
the first format revision would have produced a file whose magic said 1 and whose version byte said
2. Every reader would then have had to decide which to believe.

## Decision

The magic is `42 52 50 1A` — ASCII `BRP` followed by 0x1A — and carries no version. The `version`
byte is the single source of truth.

## Rationale

0x1A is the byte PNG puts in the same position, for the same two reasons: it is the DOS end-of-file
character, so `type file.brp` stops there instead of spewing binary at the terminal, and it is
outside the printable range, so a file mangled by a text-mode transfer fails the magic check
immediately rather than decoding into garbage.

Doing this at version 2 costs nothing. No BRP file exists outside this repository, and the change is
one constant plus its tests. Deferring it would have meant living with the contradiction forever.

## Consequences

- A v1 file is rejected with `BadMagic` rather than `UnsupportedVersion`. That is the correct
  outcome — v1 belongs to a different container generation — and `malformed.rs` asserts it.
- Future revisions change only the `version` byte.
