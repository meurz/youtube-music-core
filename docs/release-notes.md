Official Google device authorization is now the default login flow.

- Run `ytmusic auth login --no-open`, open the printed `https://www.google.com/device` link, enter the code, and approve the account. No Google Cloud project or Cookie copying is required.
- The core dynamically discovers YouTube TV's public client configuration, polls the official device-token endpoint, and stores the issued tokens securely. Access tokens refresh automatically; changed tokens are persisted by the CLI.
- TV OAuth supports account verification, playlists, likes, albums, subscriptions, and playlist contents/pagination. TV cards are normalized into the existing models. Saved-library songs and library artists still require browser authentication; TV libraries may additionally include automatic Mixes and ordinary YouTube content.
- Browser import remains available through `auth import` or `auth login --browser-port`. Existing encrypted browser profiles remain compatible.
- OAuth tokens only reach authenticated TV requests. Public web catalog operations, VR playback, and CDN probes stay anonymous with an OAuth profile. Rust hosts can retrieve refreshed state for their own secure storage.

Validation: 50 offline tests, fmt/clippy; real official Google consent, matching account identity, 22 TV playlists, 15 likes, 16 subscriptions, empty albums, two playlist contents, and forced-expiry refresh with encrypted persistence. Public search and VR M4A HTTP 206 passed. Actual library pagination and multiple OAuth accounts were not available for live testing. Browser-session flows and platform storage were validated in 0.3.0.

Compatibility: `auth login` now waits for official device authorization; the URL/code are on stderr and final JSON is on stdout. Rust Config literals gain the optional `oauth` field. The TV API can change or restrict this unofficial client's access. No JavaScript execution, Python, Node.js, or yt-dlp runtime is needed. Private-track playback, PO-token generation, and signature/n deciphering remain unimplemented or unverified.

Native Linux x86_64/ARM64, Windows x86_64, and macOS Intel/Apple Silicon archives include the CLI, shared library, C header, documentation, license, and SHA-256 checksums. Linux requires a compatible glibc (2.39 or newer). macOS/Windows binaries are unsigned.
