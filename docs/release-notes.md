Music response Cookies were previously discarded. Version 0.6.0 applies server-issued updates during normal requests and adds explicit refresh, so active sessions can be maintained without reopening a browser.

- Cookie updates stay scoped to HTTPS Music, honor root-path/domain/prefix/expiry rules, and update the next request signature. Expiry metadata survives secure storage; previous browser profiles remain readable.
- The CLI verifies updates before encrypted persistence. Per-profile locks and compare-and-save preserve newer imports and logout. `ytmusic auth refresh` performs an explicit refresh and reports whether it saved changes. Normal command results survive an automatic-save failure with a sanitized warning.
- Persistent C ABI handles provide create/call/export-session/destroy for desktop hosts. Only explicit export returns secret material; ordinary results and refresh output do not. Existing one-shot calls remain compatible.
- A compilable .NET 8 wrapper demonstrates worker-thread calls, serialized session export and secure-persistence callbacks. The WinUI 3 guide separates host playback/UI responsibilities and prioritizes remaining core work.

Validation includes offline cookie, concurrency and native-handle tests, fmt/clippy, live encrypted-profile rotation/reload, and preservation of rejected profiles. Release validation also checks Windows CLI/DLL behavior.

Boundaries: refresh maintains valid sessions and cannot restore revoked credentials. Hosts schedule idle refresh and own secret storage; no background timer is installed. Native in-flight cancellation, total-operation deadlines, URL/script renewal policy, library writes and a complete WinUI player are not implemented. Cold Web player preparation still takes tens of seconds; reuse a persistent client. Playback probes do not guarantee full-track playback.

Five-platform archives include the CLI, native library, C header, documentation, .NET example and third-party licenses. Verify SHA256SUMS before installing. Linux requires compatible glibc (2.39 or newer); Windows/macOS binaries are unsigned.
