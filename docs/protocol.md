# Web protocol and validation

## Transport

Version 0.7 uses `WEB_REMIX` for every Innertube operation. It does not send TV,
Android Music, or Android VR requests, even as a playback fallback. The service is
YouTube's official Web backend; this library itself is unofficial.

The core discovers `INNERTUBE_CLIENT_VERSION` and `VISITOR_DATA` from
`https://music.youtube.com/`. API requests use the fixed origin
`https://music.youtube.com/youtubei/v1/`, client number 67, and a JSON client
context containing WEB_REMIX, the discovered version, language, and country.
Client versions are not compiled into the library.

| Operation | Endpoint | Main fields |
| --- | --- | --- |
| Account | `account/account_menu` | Selected Web account context |
| Library / browse / playlist | `browse` | `browseId`, optional continuation |
| Search | `search` | Query and optional filter parameters |
| Pagination | Original endpoint | Continuation token |
| Song / player / stream | `player` | Video ID, signature timestamp, content checks |
| Queue | `next` | Video ID; follows one automix playlist endpoint |
| Lyrics | `next`, then `browse` | Selectable lyrics tab's browse ID |

Responsive rows, two-row cards, queue videos, shelf/carousel/grid containers and
per-section continuation tokens are supported. Unknown page/library layouts are
errors, not empty libraries. The six library browse IDs distinguish saved songs
(`FEmusic_liked_videos`) from likes (`VLLM`), and library artists
(`FEmusic_library_corpus_track_artists`) from subscriptions
(`FEmusic_library_corpus_artists`). Every library call verifies the selected
account first.

## Browser session import

`auth login` opens Google's official sign-in page and returns import guidance.
A regular browser does not automatically hand its Cookies to the core.
`auth import --browser-port PORT` captures an already signed-in Music session;
`auth login --browser-port PORT` waits for sign-in/MFA. Header files, stdin and
Netscape exports remain supported. No account password or OAuth client is needed.

The CDP bridge reads the exact Cookie header on a uniquely identified, harmless
same-origin `/generate_204` request. This preserves partitioned cookie ordering.
It verifies that the selected account did not change during capture. It does not
read unrelated sites or password databases. CDP HTTP/WebSocket connections are
restricted to the selected loopback port and bypass proxies.

The imported account is remotely verified before storage. Linux/WSL uses `pass`;
Windows/macOS use a random key in the OS credential store and an AES-GCM encrypted
session file. Writes are atomic, profiles are authenticated as associated data,
and decryption errors do not silently fall back to anonymous access. Logout
removes only the selected local profile. Existing browser profiles are compatible;
old OAuth grants return an explicit Cookie-import instruction.

## Credential boundaries

Only fixed Music-origin requests receive Cookies and `SAPISIDHASH`. The signature
is regenerated for each API request from the timestamp, signing cookie and Music
origin. `x-goog-authuser` and optional `x-goog-pageid`/`onBehalfOfUser` retain the
selected account/channel. Conflicting signing Cookies are rejected.

The shared HTTP transport has no default account headers. Static player downloads
and CDN probes receive no account credentials. API and script redirects are
rejected. Account/Cookie input and upstream JavaScript errors are not printed.
`--anonymous` bypasses stored profiles and clears configured account/visitor data.

Music homepage, watch-page and successful API responses can rotate Cookies.
The core serializes session-bearing requests and applies Set-Cookie only from the
exact HTTPS Music origin. It accepts applicable root-scoped cookies, validates
Domain and __Secure-/__Host-/__Http- restrictions, handles Max-Age/Expires and
deletions, and ignores narrow-path or partitioned updates that cannot safely be
represented by a request-header import. Updates to ambiguous duplicate cookie names are ignored; imported request
ordering is retained. Learned expiry metadata survives serialization; imported request
headers do not contain expiry dates. Anonymous clients do not accumulate cookies.

Updates are used for subsequent request signing. A host must explicitly obtain
`browser_session()` / `ytmusic_client_export_session` for persistence; pending
cookies are account-verified before export. Rejected or malformed account
responses do not provide a replacement session. CLI operations save verified
changes automatically to the existing encrypted profile. Per-profile file locks
and compare-and-save prevent a late response from overwriting a newer CLI import
or logout; independent external pass writers do not participate in those locks.
Config-file credentials are not silently copied to the CLI profile.

`auth refresh` / `auth_refresh` visits the Music homepage and verifies the account;
ordinary requests also consume response updates. Hosts may schedule refresh while
active; the core does not start background timers. Normal CLI data remains available
when automatic persistence fails (a sanitized warning goes to stderr), while an
explicit refresh fails if verification or storage fails. Refresh output contains
only metadata; CLI `saved` reports whether this call persisted a changed profile.

This maintains an active session without a browser process, not an OAuth refresh
grant or recovery of revoked credentials. Re-import a current browser session
after authentication rejection. Long-duration login retention is not guaranteed.

## Web audio resolution

The Music watch page identifies the official player script. Script URLs must use
HTTPS port 443 on `music.youtube.com` or `www.youtube.com`, under `/s/player/`,
ending in `/base.js`. Credentials, query strings, fragments, alternate hosts and
redirects are rejected. The signature timestamp comes from that same script,
which is cached per `MusicClient` for six hours. Prepared player code is also retained in a
bounded in-memory cache for reuse across calls; it contains no account credentials
or track challenges. A fresh process still performs the initial script analysis.

Audio `signatureCipher` and `n` challenges are resolved together by a pinned
vendored yt-dlp-ejs AST solver in embedded QuickJS. No yt-dlp, Node, Python,
standalone JS engine, or browser subprocess is invoked. The runtime exposes no
network/filesystem/process/host callbacks and applies input, memory, stack and
execution limits. Player source is untrusted; raw script errors and challenge
values do not appear in errors. See `vendor/` for versions, hashes and licenses.
The generated media URL retains the transformed `n` parameter. Direct raw
parser calls still reject unresolved `n` challenges.

`player` preserves metadata on transform failure and reports a sanitized
`resolution_error`; unresolved URLs remain excluded. It resolves candidates without
CDN probing; `stream` verifies formats by
descending bitrate while honoring `any`, `mp4` or `webm`. Media URLs must use
HTTPS port 443 on Google video CDN hosts and `/videoplayback`. Each probe requests
at most the initial 4 KiB, checks HTTP 200/206, content type, Content-Range and
WebM/MP4 container bytes. URLs expiring within 30 seconds are rejected. Up to
eight formats and three CDN redirects per format are allowed; account credentials
are never forwarded. A successful initial probe does not guarantee full playback.

## Limits and validation

The Web API and player can change upstream. Region/account restrictions or
additional attestation may block playback. PO-token generation, SABR transport,
DRM, downloads and audio decoding are not implemented. Hosts own
playback, later CDN errors, IP-bound URL handling, seeking and re-resolution.
API responses are bounded to 16 MiB. Per-request timeouts remain configurable;
total operation deadlines and bounded read retries are described below.

Offline tests cover Web-only routing, Cookie/account isolation, legacy-profile
migration, encrypted storage, callback-free login guidance, parser pagination,
AST transforms, runtime interruption/host isolation, script/CDN origin validation,
malformed ciphers and media byte limits. Tests use synthetic or reduced public
fixtures; personal library results, credentials and audio are not committed.
### Live validation — 2026-09-17

Using the signed-in Windows browser session imported into encrypted Linux storage:

- Verified account; 8 playlists, 3 saved songs, 5 library artists, 16 subscriptions,
  13 likes, and explicit empty albums. Three returned playlist pages loaded.
- All six search filters, album and artist pages, lyrics (3,432 characters), queue
  and queue continuation passed. Song search returned 20 items and another 20 on
  its continuation page. No personal library continuation was available to test.
- The optimized native CLI and C ABI verified the account and saved songs. Web
  audio returned `WEB_REMIX`: M4A itag 141, Opus itag 774, and a Japanese track's
  Opus itag 774 each passed HTTP 206 / 4 KiB probes. Each 256 KiB sample decoded
  for one second with FFmpeg exit 0. FFmpeg is validation-only.
- In one C ABI process, first M4A resolution took 22.57 seconds including player
  preparation; subsequent WebM and Japanese-track calls took 5.56 and 4.09 seconds.
  The cache is in-memory only: separate CLI processes repeat cold preparation.
  These timings describe this machine/network, not a performance guarantee.
- A stale browser profile was rejected explicitly; re-importing the current
  browser session restored authenticated status and all account reads. Cookie
  import was then selected as the local default.
- The original Web migration passed 59 offline tests, fmt and clippy. One private-capture test is ignored in
  CI and was run separately against the observed official player. Linux, Windows
  and macOS CI all passed.

These observations cover the tested account, region and tracks, not every account
or full-track playback. No private results, signed URLs, credentials or audio
samples are published.


### Session maintenance and desktop bridge — 0.6

Offline coverage includes cookie expiry/Max-Age precedence, source/domain/path and
prefix restrictions, anonymous isolation, signing with rotated cookies, profile
compare-and-save under concurrent writers, stale/native handle lifecycle, and
explicit secret export. The .NET 8 example is compiled in Windows CI.

Live Linux validation observed a changed encrypted profile and three persisted
expiry records after refresh, followed by authenticated status and five saved
artists from a new process. Rejected sessions failed refresh without changing the
stored credential. This demonstrates rotation/persistence, not indefinite renewal.


## Desktop JSON contract — protocol 1.1 / ABI 2

`ytmusic capabilities`, `ytmusic_capabilities()` and Rust `capabilities()` report
versions, supported operations and limits without network access or credentials.
The inner Request object is accepted by `ytmusic call` and persistent C clients;
`ytmusic_core_call` takes `{"config":{},"request":{...}}`. Existing operations remain
supported. Unknown fields are rejected. `queue_context`, `create_playlist` and
`edit_playlist` fields are **flat**, not nested under `context` or `options`.

### Discovery, accounts and playback

| Request | Fields beyond `op` |
| --- | --- |
| `capabilities`, `accounts`, `prewarm`, `playback_reset` | None |
| `search_suggestions` | `query`: nonempty string |
| `home`, `explore` | Optional `params`, `continuation`: opaque values from an earlier response |
| `queue_context` | Optional `video_id`, `playlist_id`, `params`, `index` (unsigned integer), `continuation`, `queue_context_params`; provide a video, playlist or continuation |
| `timed_lyrics` | `video_id` |
| `dash_manifest` | `video_id`; returns a verified AAC DASH descriptor for adaptive playback |
| `stream_refresh` | `video_id`, optional `format`: `any` (default), `mp4`, `webm` |
| `prefetch` | `video_ids`: one to three IDs; optional `format` as above |

```json
{"op":"queue_context","video_id":"4D7u5KF7SP8","playlist_id":"PLAYLIST_ID","index":0}
```

Home/explore return ordinary page fields plus `filters` and a top-level
`continuation`. A feed token is distinct from each section's carousel token.
Filters contain `title`, `browse_id`, `params` and `selected`; reuse the server's
parameters instead of constructing them. Suggestions return `query` and
`from_history`, without history mutation tokens.

Contextual queues return page fields, resolved `context`, `continuation`,
`lyrics_browse_id`, `related_browse_id` and optional `automix`. The host explicitly
follows the returned automix context. The original `queue` operation continues
to follow one automix endpoint automatically.

`timed_lyrics` returns `browse_id`, `text`, `source`, `source_client`, `timed`,
`timing_availability` and `lines`. Each timed line has `text`, `start_ms`, `end_ms`.
The core accepts only complete, ordered nonnegative ranges supplied by Web.
Plain/missing/malformed timings produce `timed:false`, empty `lines`, and
`timing_availability:"not_provided_by_web"`; timestamps are never estimated.
Unavailable lyrics produce `lyrics_unavailable`. The tested Web response supplied
plain lyrics; no mobile request is used to obtain additional timing data.

`accounts` returns `name`, `channel_handle`, `thumbnails`, `selected`, `disabled`
and optional `selector`. Enumeration covers the imported Music session, not the
host browser's full Google account list. An opaque GAIA/account token is not a
reusable session selector: such choices return null `selector`. Select an identity
with Rust `select_account` or
`ytmusic_client_select_account(client, selector_json, operation)`, whose result is
a **new client handle**, never a credential snapshot. Selector fields are
`auth_user` (0–99), optional `delegated_session_id` and
`expected_channel_handle`. Changing the identity requires the expected handle;
remote verification must match it exactly. The original client is unchanged.

`prewarm` actually prepares the current player and returns `ready`,
`signature_timestamp`, `generation`. Player source has a six-hour lifetime.
`prefetch` resolves up to three tracks serially and returns audio-stream results.
The per-client stream cache holds eight entries for at most five minutes and
requires more than 90 seconds of remaining URL validity; a missing expiry prevents
cached reuse. Container selection is part of the cache match. Cached results
retain their previous probe evidence without another CDN request.
`stream_refresh` bypasses that track's cached URLs. `playback_reset` clears the
client's script/URL caches and increments a generation so old work cannot restore
stale entries. Qualifying stream-resolution/probe failures trigger at most one
fresh player bootstrap. None of these operations install a background timer.

### Library and playlist writes

All writes verify the selected Web account, then send the mutation once. Requests
use these exact snake_case fields:

| Request | Fields beyond `op` |
| --- | --- |
| `rate_song` | `video_id`, `rating`: `like`, `dislike`, `indifferent` |
| `rate_playlist` | `playlist_id`, `rating` as above |
| `edit_library` | `feedback_tokens`: 1–100 current add/remove tokens |
| `subscribe` | `channel_id`: `UC…` ID, `subscribed`: boolean |
| `create_playlist` | `title`, optional `description` (default empty), `privacy` (`private` default / `unlisted` / `public`), `video_ids` (0–100) |
| `edit_playlist` | `playlist_id`; at least one of `title`, `description`, `privacy` |
| `delete_playlist` | `playlist_id` |
| `add_playlist_items` | `playlist_id`, `video_ids` (1–100), optional `allow_duplicates` (default false) |
| `remove_playlist_items` | `playlist_id`, `entries` (1–100 objects with `video_id` and `set_video_id`) |
| `move_playlist_item` | `playlist_id`, `set_video_id`, optional `before_set_video_id`; null/omitted moves to the end |

```json
{"op":"create_playlist","title":"My queue","privacy":"private","video_ids":[]}
```

```json
{"op":"edit_playlist","playlist_id":"PLAYLIST_ID","title":"New title"}
```

Song/playlist ratings use `like/like`, `like/dislike` or `like/removelike`.
Library save/remove uses `feedback`. Subscription uses
`subscription/subscribe` or `subscription/unsubscribe`. Playlist create/delete
uses `playlist/create` / `playlist/delete`; metadata and entry edits use
`browse/edit_playlist` with official action values. Playlist IDs accept an
optional `VL` prefix. Album save/remove uses the album's returned playlist ID,
not its `MPRE…` browse ID.

Items carry optional `playlist_id`, `set_video_id` and `actions`; pages also
expose their own `playlist_id` and header `actions`. Actions contain `rating`,
`in_library`, `subscribed`, `can_edit`, `add_library_token` and
`remove_library_token`. Values are populated only from the server response;
absence means unknown, not false. Tokens are account/state-specific. Obtain a
fresh page after a mutation rather than replaying stale feedback tokens.
`set_video_id` identifies one playlist entry, so duplicate video IDs can be
removed/reordered independently. Never use a display index or video ID as the
entry ID.

Mutation results contain `status` (`succeeded` for an explicit server status,
`accepted` for an acknowledged response without a separate status), optional
`playlist_id`, and `added_items` with returned entry IDs. A protocol failure or
an account-interaction response does not imply success. Reload state to confirm
changes before issuing dependent edits.

### Operations, errors and retry policy

`ytmusic_operation_create({"timeout_ms":120000})` returns an opaque operation
handle. Allocate it before dispatch; deadlines start at allocation, including
queue and lock waits. Valid deadlines are 1–600000 ms. Each handle can run exactly
one `_with_operation` call (create client, execute Request, export session) or
account-selection call. Poll `ytmusic_operation_status` or signal
`ytmusic_operation_cancel` from another thread. Status contains `state`
(`queued`, `running`, `finished`), `phase`, `elapsed_ms`, `cancelled`. Phase names
are progress hints, not a stable percentage or an assertion of success.

Destroy every operation with `ytmusic_operation_destroy`; destroying an active
operation also cancels it. Cancelled HTTP work drops the in-flight future;
QuickJS and contended core locks observe the same cancellation/deadline.
`ytmusic_client_destroy` removes a client handle without cancelling its active
calls. Keep the DLL loaded until they return. Every allocated JSON result,
including errors/status, must be freed exactly once with `ytmusic_string_free`.
Limits are 128 live clients and 256 live operations. IDs are opaque and not reused.

Rust hosts use `OperationContext::new(OperationOptions { timeout_ms })`, call
`run` on a worker, and `cancel`/`progress` from another thread. Ordinary operations
get a default 120-second context when no explicit context exists. CLI
`--timeout-ms` configures the total operation and Ctrl+C requests cancellation.
The separate Config `timeout_seconds` remains a per-request limit.

Errors retain `code` and `message`, with `retryable`, optional `http_status`,
`retry_after_seconds` and `cause_code`. `cancelled`, `timeout`, `rate_limited`,
`authentication_required`, and `authentication_rejected` are distinct. Network
errors and response messages omit request URLs and credential values.

Eligible reads retry network failures, request timeouts and HTTP 429/502/503/504
at most three attempts, with 250/500 ms backoff and a server Retry-After delay
when larger. Delays above ten seconds are not waited out automatically. Retry
waits count toward the total deadline and are cancellable. Invalid input,
protocol errors and authentication rejection do not enter a generic retry loop.

Writes never retry automatically. A cancellation before dispatch is `cancelled`.
An uncertain network/timeout/cancellation/invalid-response/server-error outcome
after write dispatch is `mutation_outcome_unknown`, `retryable:false`, with a
`cause_code` identifying the underlying failure. This does not mean the server
rolled back the write. Reload the playlist/library and reconcile before retrying,
especially for create/append operations. A transport retry hint cannot authorize
blind replay of a mutation.

### Desktop completion validation — 0.7

Focused discovery tests and live C ABI reads verified search suggestions,
filtered/paginated home, explore, contextual automix queues and continuation,
plain lyric fallback, account clone selection, mismatched expected-handle
rejection and continued use of the original handle. Offline tests cover complete
and partial lyric timing data without claiming that Web currently returns it.
Native cancellation tests cover waiting for response headers/body, contended
locks and interrupted JavaScript; read-retry tests contrast transient reads with
unreplayed writes. Artifact and live mutation verification are recorded separately
as they complete; source support alone does not claim a platform or account's
full playback behavior.

### Windows adaptive AAC playback

`dash_manifest` returns `manifest` (MPD XML), `mime_type`, `video_id`, `itag`,
`source_client`, `expires_at`, and `http_headers`. It resolves a verified Web AAC
stream and uses its inclusive `init_range` / `index_range`, `duration_ms`, sample
rate and channel count to describe the original media resource with DASH
`SegmentBase`. Missing or invalid segmentation metadata is an explicit error.
The manifest contains a signed media URL; do not log it or persist it indefinitely.

For Windows, load the MPD through `AdaptiveMediaSource.CreateFromStreamAsync`
with the returned public CDN headers, and create a fresh adaptive source on each
replacement. This preserves original audio bytes and edit lists. Raw fragmented
M4A URI playback prematurely ended in the tested Windows `MediaPlayer`; using
standard DASH passed real clock advancement, seek, rapid changes and HTTP404
failure recovery. Direct WebM also passed a complete track through `MediaEnded`.
See the compiled [Windows helper](../examples/dotnet/windows/).

To recover an expired AAC URL, call `stream_refresh` with `format:"mp4"` and
then `dash_manifest` on the same client. Restore the position in the host. The
manifest shares the verified stream cache lifetime; generating XML alone does
not extend the URL expiry.
