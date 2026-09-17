# Building a WinUI 3 host

The 0.6 core can support a search/library/playback prototype. It does not include a
WinUI application. Start with the [.NET 8 wrapper](../examples/dotnet/README.md)
and [C ABI contract](../include/youtube_music_core.h); ship the matching Windows
x64 `youtube_music_core.dll` beside the application executable.

## Available now

- Official Music Web (`WEB_REMIX`) requests throughout: search and filters,
  album/artist/playlist browsing, explicit pagination, recommended queues, plain
  lyrics, account verification, and all six read-only library sections.
- M4A/AAC and WebM/Opus URL resolution with signature/`n` handling, expiry and
  playback headers, plus a bounded CDN probe. Playback and decoding are external.
- Imported browser sessions receive server-issued Cookie updates. Explicit
  `{"op":"auth_refresh"}` visits the Music homepage and verifies the selected
  account. This maintains a valid session; it cannot guarantee renewal or restore
  a revoked login.
- Persistent native clients preserve HTTP connections, player-script state, and
  evolving Cookies. Use `ytmusic_client_create`, `ytmusic_client_call`,
  `ytmusic_client_export_session`, and `ytmusic_client_destroy`. Handles are opaque
  `uint64_t` IDs, not pointers. Free every returned JSON string with
  `ytmusic_string_free`. The older `ytmusic_core_call` remains a one-shot API.

Native calls block. The wrapper runs them off the UI thread and serializes each
instance's operations. Retain one instance per account and dispose it explicitly.
Destroying a native handle does not cancel in-flight calls; those retain ownership
until completion. Keep the DLL loaded while any call is running.

## Host responsibilities

| Responsibility | Recommended owner |
| --- | --- |
| Official browser login/MFA, account selection, Cookie import UI | WinUI host; pass imported session fields to the core |
| Secure session persistence | Host using Windows DPAPI or equivalent, atomic replacement, and coordination across processes |
| Music protocol, Cookie updates, account verification, page parsing, stream resolution | Rust core |
| Audio decoding, buffering, seek, volume, playback state | Windows `MediaPlayer` |
| Playback queue, repeat/shuffle policy, next-track scheduling, local resume state | Host playback service |
| SMTC/media keys, notifications, tray, view models, image cache | WinUI host |

Use `RefreshAndPersistAsync` to refresh, verify, and send the resulting session to
your encrypted-store callback. Use `ExportSessionToAsync` to persist rotations
observed during ordinary calls. Export may perform account verification. Its
output is secret session JSON and must never enter logs, UI bindings, or telemetry.
Complete the callback only after persistence succeeds. The example serializes
within one instance; multiple instances/processes need a shared lock or a
compare-and-save mechanism so a late request cannot undo logout or a newer import.

Choose a bounded refresh schedule while the app is active; the core and wrapper
install no background timer. Surface `authentication_rejected` as a request to
sign in through the official browser and import again. Do not promise permanent
login or retry rejected sessions indefinitely.

For the first player integration, request `"format":"mp4"` and pass the returned
`http_headers` with the URL. M4A is the recommended initial Windows path, but actual
Windows MediaPlayer compatibility, full-track playback, and seeking still need
host-level verification. CLI/DLL URL probes and FFmpeg sample decoding do not
constitute that verification. Test WebM/Opus on the supported Windows versions
before exposing it as a user option. Keep media requests on the same network/proxy
path as resolution where required by IP-bound URLs.

## P0: reliable interactive playback

1. **Native cancellation and an operation deadline.** Current `timeout_seconds`
   bounds each HTTP request, not the complete operation. Player processing has
   separate limits, and multiple CDN probes may run. The wrapper's cancellation
   token cancels waiting for its gate; it cannot abort an already running native
   call. Until native cancellation exists, use request generations in the host to
   discard stale results after a track change; this does not stop wasted work.
2. **Playback URL and player-script lifecycle.** URLs include `expires_at` but
   have no automatic refresh service. Resolve close to playback, prefetch the next
   track, and request a replacement on expiry or a qualifying media failure. The
   host owns saving/restoring the seek position. The core still needs explicit
   invalidation/rebootstrap of its cached player script for a long-running client.
3. **Startup and latency.** The first player-script analysis can take tens of
   seconds. Prepared scripts are cached in-process; process restart repeats cold
   preparation. Add a dedicated prewarm operation and phase/progress reporting;
   schedule bounded prefetch without delaying user-initiated playback.
4. **Recoverable error contracts.** Extend code/message errors with structured
   timeout, cancellation, HTTP status, retryability, and rate-limit delay where
   available. Add bounded retries for eligible reads; do not retry every error or
   replay future writes indiscriminately. Existing authentication errors should
   remain distinguishable from network failures and empty library results.

Before shipping, test the WinUI host with complete tracks, rapid track switching,
seek near the end, an expired URL, network loss/recovery, app suspend/resume, and a
rejected session. Measure startup and warm switching in the actual MediaPlayer
integration rather than inferring them from native request timing.

## P1: a fuller Music client

- Library writes: like/unlike, save/remove, subscribe/unsubscribe, and playlist
  creation/editing/track changes. Extend item models with action state and required
  identifiers, including playlist entry IDs; current library access is read-only.
- Dedicated home/discovery and search-suggestion APIs, richer recommendation
  context, and optional official playback-history synchronization. Generic
  `browse` already accepts home-feed IDs, and track-based recommendations exist.
- Timed lyrics, account enumeration/switching, and richer availability/permission
  metadata. Current lyrics are plain text and account information describes the
  selected session.
- Stable JSON schema/version or capability discovery, .NET integration coverage,
  and a Windows ARM64 build if the client will support that architecture.

Keep these protocol additions in the core while leaving presentation and playback
policy in the host. PO-token generation, SABR delivery, and DRM are not implemented;
the supported stream paths should report their limitations explicitly.
