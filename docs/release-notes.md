Native audio streaming is now implemented.

- `ytmusic stream VIDEO_ID` resolves through the Android VR player profile, validates a bounded CDN byte range, and returns the best working audio URL.
- `--format mp4` selects AAC/M4A; `--format webm` selects Opus. Failed formats fall back within the requested container, followed by a WEB_REMIX profile fallback.
- Results include public playback headers, source client, URL expiry, and media verification details. Account cookies are isolated from anonymous player and CDN requests.
- WEB_REMIX requests now discover the current signature timestamp; `playback_client` configuration can force either profile.
- Rust, JSON CLI, and C ABI share the resolver. Existing JSON stream requests still work; Rust `Request::Stream` constructors now need `format`.

Validation: 25 offline tests; real English audio, official music-video audio, and Japanese music streams returned HTTP 206. Opus and M4A samples decoded successfully for one second using FFmpeg (validation only, not a runtime dependency). Full-track playback was not tested.

Player JavaScript signature/n deciphering and PO-token generation remain unsupported. The anonymous Android VR profile avoids those requirements for the public tracks tested; it is not a guarantee against region, account, or anti-bot restrictions. See docs/protocol.md.

Native Linux x86_64/ARM64, Windows x86_64, and macOS Intel/Apple Silicon archives include the CLI, shared library, C header, documentation, license, and SHA-256 checksums. Linux requires a compatible glibc (2.39 or newer). macOS/Windows binaries are unsigned.
