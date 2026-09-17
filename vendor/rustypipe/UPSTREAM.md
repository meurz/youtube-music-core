# Pinned RustyPipe source

- Project: <https://github.com/TeamPiped/rustypipe>
- Revision: `3f1491263929c22036429b665e370fdd2f066017`
- Upstream version label: `0.11.4` (this revision differs from the older crates.io release)
- License: GPL-3.0, reproduced in `LICENSE`

The source directory is retained from this revision, including its tests and
snapshots. Large upstream `testfiles/` fixtures are not bundled; local tests
exercise the adapter and targeted compatibility patches. The workspace manifest is flattened so the library can be built as a
path dependency. This is a pinned dependency, not a second project protocol
implementation.

Local compatibility changes are limited to:

- Replace the yanked `wreq` dependency with the host's existing `reqwest` 0.12
  transport through a package alias; only their shared HTTP APIs are used.
- Pin `flexon` to 0.4.6 because later 0.4.x releases fail this build; apply
  mechanical formatting and lint fixes for the pinned host toolchain.
- In-memory, explicitly selected Web sessions and client versions, with disk
  caches, reports, OAuth and automatic BotGuard discovery disabled by the host.
- A Web-only raw player entry point that preserves DASH/SABR delivery fields,
  accepts host-provided player attestation and reload context, and reuses
  RustyPipe's player discovery and OXC/QuickJS signature and `n` transforms.
- Bounded, cancellable JavaScript execution and strict script/media URL checks.
- Bounded response bodies and an exact-origin response observer so the host can
  maintain its Cookie state without accepting stale in-flight rotations.
- Music search/artist shelf titles and independent real continuation tokens,
  including carousel shelves and individually wrapped main-search rows.
- Music item/header action metadata, playlist entry identifiers, availability,
  explicit markers and suggestion history flags needed by the existing host.

The adapter lives in `src/upstream/` of the parent crate. Features not supplied by
RustyPipe remain extensions there: account verification and cookie maintenance,
home/explore, library artists, timed lyrics, contextual queues, writes, native
SABR delivery, C ABI operation control and host-safe credential persistence.
