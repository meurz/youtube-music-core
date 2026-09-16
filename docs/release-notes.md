Initial experimental release of the native Rust YouTube Music core.

- Search filters, song metadata, album/artist/playlist browsing, section pagination, playback queues, and plain lyrics.
- JSON CLI, Rust library, and C ABI with explicit string ownership.
- Automatic WEB_REMIX bootstrap; optional private configuration for cookies, proxy, visitor data, and externally supplied PO tokens.
- Native Linux x86_64/ARM64, Windows x86_64, and macOS Intel/Apple Silicon archives with SHA-256 checksums.

Playback is conditional. This release exposes ready direct audio URLs only; JavaScript signature/n deciphering and PO-token generation are not implemented. Live testing of this environment encountered YouTube player restrictions. Authenticated access is not yet verified. See docs/protocol.md for exact validation evidence.

Linux binaries are built on Ubuntu 24.04 and require a compatible glibc (2.39 or newer). macOS and Windows binaries are unsigned; this release does not include notarization or code signing.
