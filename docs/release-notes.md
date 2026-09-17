Version 0.9.1 enables host-provided anonymous Proof of Origin bundles for native
WEB_REMIX playback. It does not embed a token generator or change the playback
client, JSON protocol 2.0 or C ABI 2.

- Add `anonymous_attestation_context` and the `anonymous_po_tokens` capability.
  The sensitive descriptor exposes the exact visitor identity and a separate
  local ownership fingerprint for an anonymous client.
- Accept matching anonymous GVS-only bundles through the existing `set_po_tokens`
  operation. Existing video/purpose/expiry limits, cache invalidation and SABR
  proof checks remain in force.
- Keep authenticated session/account binding and the signed-in browser helper
  unchanged. Invalid or expired cookies never downgrade to anonymous identity.
- Preconfigured anonymous bundles require explicit matching `visitor_data`;
  a host may instead bootstrap a client, obtain its descriptor and install proof.

Offline validation covers visitor separation, invalid session rejection,
GVS-only installation, atomic replacement and the unchanged signed-in path.
Complete playback still depends on the server accepting the provider's real
proof; a successful 4 KiB CDN probe does not establish full-track availability.

Version 0.9.0 removes the speculative DRM and browser-playback surface. Native Web audio, DASH, SABR, PO tokens, account access and session maintenance remain available through WEB_REMIX.

- Remove the DRM model, official playback operation/CLI command, dedicated error and advertised capability. There is no license acquisition, CDM probing or browser playback fallback. Unsupported encrypted formats are still excluded from audio candidates.
- Keep official-page PO generation in an optional, attestation-only Windows helper: `web-attestation` / `OfficialBrowserAttestation`. The helper imports a Music session, checks identity and installs PO tokens; audio playback remains native. CLI browser-port attestation remains available.
- JSON protocol 2.0 replaces 1.2 because the operation and descriptor were removed. C ABI 2 entry points and handle semantics are unchanged. Rust callers and .NET hosts using removed playback APIs must update; see `docs/web-delivery.md`.

Validation covers Rust tests, formatting/static checks, Windows attestation builds and native audio delivery. Six native release targets include Windows x64 and ARM64. Archives contain the CLI, native library, C header, documentation, .NET examples and third-party notices.

Verify SHA256SUMS before installing. Linux requires compatible glibc (2.39 or newer); Windows/macOS binaries are unsigned. The built-in PO browser helper still needs a real signed-in official browser. Native SABR is audio-only VOD; progressive playback needs a host demuxer. Revoked sessions require official login/import.
