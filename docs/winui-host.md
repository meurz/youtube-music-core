# Building a WinUI 3 host

Version 0.8 provides the core protocol, session, discovery, library editing and
playback lifecycle needed by a desktop Music client. It does not include a WinUI
application or media decoder. Start with the [.NET 8 wrapper](../examples/dotnet/README.md)
and [C ABI contract](../include/youtube_music_core.h). Ship the native DLL matching
the application architecture beside the executable. Windows x64 and ARM64 are exercised by native Rust and .NET CI jobs and have
matching release targets.

## Core and host responsibilities

| Responsibility | Owner |
| --- | --- |
| Official browser login/MFA and Cookie import UI | Host; pass the imported Music session to the core |
| Cookie maintenance, remote identity verification, account enumeration/selection | Core |
| Encrypted session storage and coordination across processes | Host, for example Windows DPAPI with atomic replacement and compare-and-save |
| Search/suggestions, home/explore, queues, lyrics, library and playlist reads/writes | Core |
| Player preparation, signature resolution, URL caching/refresh, bounded prefetch | Core |
| Audio decoding, buffering, seek, volume and playback state | Windows `MediaPlayer` |
| Queue/repeat/shuffle policy, next-track scheduling and local resume state | Host playback service |
| SMTC/media keys, notifications, tray, view models and image cache | WinUI host |

All protocol and playback requests use `WEB_REMIX`; there is no TV/Android
fallback. Lyrics contain time ranges only when the Web response provides them.
`timing_availability: "not_provided_by_web"` means the host should display plain
text. Account enumeration covers identities exposed by the imported Music
session, not every Google account stored in a browser. A null account `selector`
means that identity needs a separate browser import. An identity switch returns a
new verified client; the original remains usable.

## Native lifecycle and cancellation

Keep one persistent client per selected account. Native calls block, so dispatch
them to worker threads. Use ABI 2 operation handles to cancel work already in
progress, set a total deadline, and inspect progress. The .NET wrapper integrates
these calls with `CancellationToken`; cancellation reaches HTTP I/O, player
execution and lock waits. It is no longer limited to waiting for the wrapper gate.

Allocate a **new single-use operation** before dispatch. Its `timeout_ms` starts
at allocation and includes queue/lock waits, requests, retries and player work.
The default is 120,000 ms; accepted values are 1 through 600,000. Use
`ytmusic_client_create_with_operation`, `ytmusic_client_call_with_operation`, or
`ytmusic_client_export_session_with_operation`. Poll `ytmusic_operation_status`
from another thread for `state`, `phase`, `elapsed_ms` and `cancelled`, or call
`ytmusic_operation_cancel`. Always destroy the operation handle after the worker
finishes; destroying an active operation also signals cancellation. Progress is
phase-based, not a percentage or byte counter.

Free every native result exactly once with `ytmusic_string_free`, including
errors and status results. Client destruction does not cancel active work: those
calls retain ownership until completion. Explicitly cancel their operation
handles first when shutting down, await workers, and keep the DLL loaded until
they finish. Legacy ABI entry points remain available but do not expose a
caller-controlled cancellation handle. `ytmusic_capabilities` and
`ytmusic_abi_version` let the host check protocol 1.2 / ABI 2 locally.

Use a request generation in the host as well as cancellation, so a successful
result racing with a track change cannot replace the newly selected song.

## Playback flow

1. On an idle worker, call `{"op":"prewarm"}` to download and actually prepare
   the official player. First preparation can still take tens of seconds; expose
   its progress and let foreground playback cancel unneeded background work.
2. Resolve AAC with `{"op":"dash_manifest","video_id":"…"}`. Pass its MPD and
   `http_headers` to Windows `AdaptiveMediaSource`, then create a `MediaSource`
   for `MediaPlayer`. See the [Windows helper](../examples/dotnet/windows/).
   The manifest describes the original Web media bytes; no remux, local server
   or external runtime is needed. Create a fresh adaptive source for each source
   replacement. Direct WebM/Opus via `stream` with `format:"webm"` is another
   tested path. Raw M4A URI playback is not recommended: Windows prematurely
   ended the tested fragmented MP4 when its edit list was read without DASH.
3. Schedule `prefetch` for one to three upcoming track IDs. It resolves them
   serially into the same client cache; the core installs no scheduler or timer.
4. On expiry or a media URL rejection, preserve the seek position and call
   `stream_refresh` (format `mp4` for AAC), then request a fresh `dash_manifest`.
   Replace the source and restore playback state in the host.
   `playback_reset` explicitly discards this client's source and URL caches.

Player source expires after six hours. The stream cache holds at most eight
entries, for at most five minutes, and only reuses URLs with more than 90 seconds
remaining. Missing-expiry URLs are never reused from cache. A cached result
retains its earlier probe evidence; returning it does not probe the CDN again.
Explicit refresh bypasses that track's cached URLs. Qualifying stream failures
trigger at most one fresh player bootstrap; cache generations prevent older work
from repopulating an invalidated cache. Keep resolution and playback on the same
network/proxy path where signed URLs are IP-bound.

A 4 KiB CDN probe validates the start of a stream, not full-track playback. Before
shipping the host, exercise full tracks, rapid switching, seek near the end,
expired URLs, network loss/recovery, app suspend/resume and a rejected login in
the actual MediaPlayer integration.

## Session storage and account switching

Use `RefreshAndPersistAsync` for refresh/verification followed by an encrypted
store callback. Use `ExportSessionToAsync` to persist rotations observed during
ordinary calls. Export may require account verification. Its output is secret
session JSON: never expose it to logs, view models or telemetry. Complete the
callback only after persistence succeeds. Multiple clients/processes need a
shared lock or compare-and-save so old responses cannot undo logout or a newer
import. The core and wrapper install no periodic refresh timer.

`accounts` returns display metadata and reusable selectors where available.
`ytmusic_client_select_account(handle, selector_json, operation)` returns a new
handle after verification. A changed `auth_user` or `delegated_session_id`
requires `expected_channel_handle` and an exact remote match. Persist each
selected client's exported session in its own profile. Do not derive delegated
session IDs from opaque GAIA IDs.

Surface `authentication_rejected` as official browser sign-in and reimport.
Maintaining an active Cookie session cannot restore revoked credentials or
promise permanent login.

## Writes and recoverable failures

The core exposes like/unlike/dislike, save/remove through current action tokens,
artist subscription, playlist create/edit/delete and duplicate-safe item
add/remove/reorder. Drive controls from returned `actions` and unique
`set_video_id` values. Missing action state is unknown, not false. Refresh the
relevant page after edits instead of reusing stale feedback tokens.

Errors include `code`, `message`, `retryable`, and optional `http_status`,
`retry_after_seconds` and `cause_code`. Reads retry transient network failures, timeouts and HTTP
429/502/503/504 at most three attempts within the operation deadline. The core
honors server delays up to ten seconds; a longer delay is returned to the host.
Authentication, protocol and invalid-input failures are not blindly retried.

Writes are sent once. Cancellation, timeout or connection loss **after sending a
write does not prove that it was uncommitted**. Reload the playlist/library and
reconcile before retrying. Uncertain failures after dispatch return
`mutation_outcome_unknown`, `retryable:false`, and the underlying `cause_code`. In particular, never automatically replay a
playlist create or append merely because a request timed out.

Native SABR audio, optional official-browser attestation, and licensed official-page
DRM playback are available through the [Web delivery interfaces](web-delivery.md).
The [WebView2 helper](../examples/dotnet/web-player/README.md) probes actual CDM
support and leaves license acquisition to the official player. Native SABR returns
media segments and needs a compatible host demuxer for progressive playback.
The WebView2 route does not satisfy a browser-free native DRM requirement. That
path remains unimplemented; see the [native DRM investigation](native-drm.md)
for the platform probes, playback failures and remaining integration requirements.
See the [protocol reference](protocol.md) for complete request shapes.
