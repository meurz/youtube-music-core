YouTube Music now uses its official Web client for the entire request chain.

- Import the signed-in Music browser session with `ytmusic auth import --browser-port 9222`, `--headers-file FILE`, or `--stdin`. `auth login` opens Google's official page and explains the import step; there is no OAuth consent application or device-code flow.
- Account verification, all six personal-library categories, search, browse, pagination, queue, lyrics and playback use WEB_REMIX. Android/VR/TV fallback and OAuth APIs were removed.
- The native binary resolves Web signature and n challenges using a pinned parser in an embedded, resource-limited QuickJS context. No Node, Python, yt-dlp process or browser is needed after Cookie import. Static-script and CDN requests carry no account credentials.
- Cookie sessions remain encrypted in Linux pass or Windows/macOS credential-backed storage. Re-import when the browser session expires or is revoked; independent Cookie renewal is not promised.

Migration: existing browser profiles remain compatible. Old OAuth profiles now report an explicit Cookie-import instruction. Import into the same profile to replace one only after successful account verification. Rust hosts migrate to BrowserSession; playback_client accepts auto or web_remix, both Web-only.

Limitations: no PO-token generation, SABR, DRM, library writes or audio decoder. Google's private protocol, region restrictions and account requirements can change. A bounded initial CDN probe does not prove full-track playback.

Native Linux x86_64/ARM64, Windows x86_64, and macOS Intel/Apple Silicon archives include the CLI, shared library, C header, documentation, third-party licenses, and SHA-256 checksums. Linux requires compatible glibc (2.39 or newer); macOS/Windows binaries are unsigned.
