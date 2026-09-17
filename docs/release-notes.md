Version 0.9.0 removes the speculative DRM and browser-playback surface. Native Web audio, DASH, SABR, PO tokens, account access and session maintenance remain available through WEB_REMIX.

- Remove the DRM model, official playback operation/CLI command, dedicated error and advertised capability. There is no license acquisition, CDM probing or browser playback fallback. Unsupported encrypted formats are still excluded from audio candidates.
- Keep official-page PO generation in an optional, attestation-only Windows helper: `web-attestation` / `OfficialBrowserAttestation`. The helper imports a Music session, checks identity and installs PO tokens; audio playback remains native. CLI browser-port attestation remains available.
- JSON protocol 2.0 replaces 1.2 because the operation and descriptor were removed. C ABI 2 entry points and handle semantics are unchanged. Rust callers and .NET hosts using removed playback APIs must update; see `docs/web-delivery.md`.

Validation covers Rust tests, formatting/static checks, Windows attestation builds and native audio delivery. Six native release targets include Windows x64 and ARM64. Archives contain the CLI, native library, C header, documentation, .NET examples and third-party notices.

Verify SHA256SUMS before installing. Linux requires compatible glibc (2.39 or newer); Windows/macOS binaries are unsigned. PO generation still needs a real signed-in official browser when requested. Native SABR is audio-only VOD; progressive playback needs a host demuxer. Revoked sessions require official login/import.
