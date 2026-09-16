Browser-session authentication and personal YouTube Music libraries are now available in the Rust core, JSON CLI, and C ABI.

- `auth login` guides browser sign-in; `auth import --browser-port PORT` imports directly from a local signed-in Chrome/Edge Music tab. Request-header/stdin/Netscape imports are also supported.
- Import verifies the selected account before saving. `auth status`, `account`, `--profile`, `--anonymous`, and local `auth logout` manage account access.
- Linux uses `pass`; Windows/macOS use a system credential-backed AES-256-GCM session vault. No plaintext fallback or Google password handling.
- `library playlists|likes|songs|albums|artists|subscriptions` provides read-only access with explicit pagination. Use returned IDs with `browse` or `playlist`.
- Browser partitioned Cookie ordering is preserved through exact request capture. Captured Authorization hashes are discarded, fresh signatures are generated per request, and account credentials stay isolated from anonymous VR/CDN streaming.

Validation: 42 offline tests, formatting and clippy; a real Windows-browser session imported into the native Linux CLI, encrypted store round trip, all six library sections, three playlist contents, authenticated search, and anonymous VR streams returning HTTP 206. The real account had no library continuation; pagination and multi-account/brand selection are covered offline. See docs/protocol.md for platform and protocol limits.

This is browser-session authentication, not Google OAuth. Sessions may expire and require re-import. Library writes, private-track playback, JavaScript signature/n deciphering, and PO-token generation remain unsupported or unverified. Rust Config literals gain `delegated_session_id`; existing JSON requests remain compatible.

Native Linux x86_64/ARM64, Windows x86_64, and macOS Intel/Apple Silicon archives include the CLI, shared library, C header, documentation, license, and SHA-256 checksums. Linux requires a compatible glibc (2.39 or newer). macOS/Windows binaries are unsigned.
