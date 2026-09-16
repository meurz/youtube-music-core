# Protocol and validation

## Transport

The client bootstraps `https://music.youtube.com/` using a desktop browser User-Agent and reads the public `INNERTUBE_CLIENT_VERSION` and `VISITOR_DATA` configuration. Observed WEB_REMIX version on 2026-09-16: `1.20260913.16.00`. The version is discovered at runtime, not compiled into the library.

Catalog POST requests go to `https://music.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false`, with JSON `context.client` fields `clientName=WEB_REMIX`, `clientVersion`, `hl`, `gl`, and optional `visitorData`. Headers include Origin/Referer and client number `67`. These endpoints accepted requests without an API key during validation. Audio resolution additionally uses the anonymous Android VR player endpoint described below.

| Operation | Endpoint | Main request fields |
| --- | --- | --- |
| Account verification | `account/account_menu` | Context with selected account |
| Personal library | `browse` | Library browseId and optional continuation |
| Search | `search` | `query`, optional filter `params` |
| Album / artist / playlist | `browse` | `browseId` (playlist IDs gain `VL`) |
| Pagination | Original endpoint | `continuation` |
| Song / player / stream | `player` | Profile-specific context, `videoId`, content checks; WEB_REMIX also uses signature timestamp and optional `serviceIntegrityDimensions.poToken` |
| Queue | `next` | `videoId`, audio-only/persistent-panel flags; follows one returned automix playlist endpoint |
| Lyrics | `next`, then `browse` | Finds the selectable lyrics tab and uses its `browseId` |

The parser recognizes responsive rows, two-row cards, queue videos, shelf/carousel/grid containers, and their continuations. Per-section continuation tokens are preserved. Missing optional fields are `null` or empty arrays. An empty recognized page is valid; a response without page contents is a protocol error. Layouts are private API contracts and may change.

## Authentication and playback boundary

Optional Cookie headers are attached per request only to the fixed music origin. `SAPISID` (or a secure PAPISID fallback) signs `timestamp + space + cookie_value + space + origin` with SHA-1 to build `SAPISIDHASH`. Authorization is refreshed on every request. Header values are marked sensitive in the HTTP library. Config parsing errors do not echo submitted credentials. The shared HTTP transport has no default Cookie or Authorization header, so anonymous VR, watch-page bootstrap, and media requests do not inherit account credentials.

Browser-session sign-in and personal library reads are authenticated-session verified in 0.3.0. The CLI can import request headers or capture an exact same-origin request through an explicitly enabled local Chromium debugging port, then verify and securely save the session. There is no Google OAuth flow, password handling, token generation, player JS interpreter, DRM handling, or yt-dlp subprocess. Account library mutations, downloads, and audio decoding are outside this release.

`player` separates metadata from playability: song metadata may remain available even when the player is blocked. Only HTTPS audio URLs without an `n` challenge are exposed as candidates. Cipher-only and n-challenged formats increment `unresolved_audio_formats`. `player` does not probe URLs; `stream` does. If every profile fails, `stream_unavailable` includes profile/format diagnostics without signed URLs. Private-track playback remains unverified.

## Account and library — 0.3.0

The core accepts a `BrowserSession` and exposes account verification, auth status, and six read-only library sections. Cookie presence is insufficient: `account/account_menu` must return an `activeAccountHeaderRenderer`. Explicit logged-out responses and HTTP 401/403 become authentication rejection; unexpected layouts remain protocol errors. `x-goog-authuser` chooses the Google account index. Optional brand/channel selection adds `x-goog-pageid` and `context.user.onBehalfOfUser`. The CLI captures these from the selected Music tab and verifies that selection is unchanged during import.

Library browse IDs are `FEmusic_liked_playlists`, `VLLM`, `FEmusic_liked_videos`, `FEmusic_liked_albums`, `FEmusic_library_corpus_track_artists`, and `FEmusic_library_corpus_artists`. Liked songs and saved library songs are distinct. Each library page verifies the account first, retains section continuations (including `gridContinuation`), and removes action tiles without dropping the first real playlist. Recognized empty-state messages are accepted; unknown empty layouts are errors.

The CLI reads the Cookie header from a unique same-origin `/generate_204` request through `Network.requestWillBeSentExtraInfo`, matching both request ID and URL regardless of event order. This preserves actual browser partitioned-cookie selection; combining `Network.getCookies` entries was ambiguous for duplicate preferences. Conflicting signing-cookie duplicates are rejected. CDP HTTP/WebSocket access is restricted to the selected loopback port and bypasses proxies. Login/MFA stays in the browser.

Session persistence is a host concern. The CLI uses `pass` on Linux, or a system credential-backed AES-256-GCM vault on Windows/macOS. The key is random and stored separately from the encrypted file, avoiding Windows credential-size limits. Nonces are random, the profile is authenticated as associated data, writes replace atomically, and decryption failures are explicit. Import saves only after remote verification. Logout deletes the local selected profile. API/library calls do not require the browser after import; expired sessions need re-import.

Live validation on 2026-09-16 used a signed-in Windows Chrome tab through CDP and the native WSL/Linux CLI. Verified import, encrypted `pass` storage, a fresh-process auth status/account lookup, all six library sections, and contents of three returned playlists. The account returned 8 playlists, 13 likes, 3 saved songs, 5 library artists, and 16 subscriptions. Albums returned the explicit “No albums yet” empty state. No account identity, playlist names/IDs, cookies, or private response fixtures are committed. Account-backed search returned 20 songs; account-backed default streaming and explicit anonymous M4A streaming both used anonymous VR and returned HTTP 206.

The tested account had no library continuation, so library pagination is covered offline. Multi-account/brand request selection and credential isolation are covered offline; only the currently selected real account was exercised. Windows/macOS vault code is compiled/tested by CI; live OS keychain integration requires a desktop session. No automatic session renewal or account-library write operations are implemented.

## Native streaming — 0.2.0

The first release's generic `UNPLAYABLE` response did not prove that all player profiles were blocked. Two request-level differences resolved it:

1. WEB_REMIX requires `playbackContext.contentPlaybackContext.signatureTimestamp`. The watch page's public `STS` value was `20702` during validation. Discovering that value changed the same song's web player response from `UNPLAYABLE` to `OK`, with four ciphered audio formats. The timestamp is discovered dynamically and cached for the client lifetime.
2. The native `ANDROID_VR` profile returned direct URLs when its version, User-Agent, and device context matched. The tested version is `1.65.10`, client number `28`, device `Oculus / Quest 3`, Android SDK `32`, OS `Android / 12L`. Its User-Agent is `com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip`. Requests go to `https://www.youtube.com/youtubei/v1/player`. This profile needs no JavaScript execution, signature timestamp, account cookie, or generated PO token for the public tracks tested. Client compatibility can change upstream.

Automatic mode tries VR first and then WEB_REMIX. `player` returns the first OK response with direct candidates; if none exists it retains inspection metadata. `stream` checks each profile's candidates by bitrate, honoring `any` / `mp4` / `webm`. The selected profile appears in both player and audio results. URLs whose declared expiry is within 30 seconds are rejected.

Each media probe requests `Range: bytes=0-4095` with the returned public User-Agent. It accepts HTTP 200/206, checks content type, validates the initial Content-Range when present, reads at most 4 KiB, and checks WebM EBML or MP4 `ftyp` bytes. A broken URL or unsupported container is skipped. There are at most eight candidate formats per profile and three redirects per candidate. Initial and redirect URLs must use HTTPS port 443, the Google video CDN domain, and `/videoplayback`; cookies are never forwarded. Error pages, malformed/truncated ranges, unexpected redirects, and container mismatches fail validation.

The probe establishes that the beginning of the audio is accessible. Hosts still own decoding, seeking, later CDN errors, and re-resolution after URL expiry. Streaming requires the same effective network path when URLs are IP-bound; use the configured proxy consistently in the host.

### Live streaming validation — 2026-09-16

| Probe | Observed result |
| --- | --- |
| `4D7u5KF7SP8`, WebM | ANDROID_VR, itag 251, Opus, HTTP 206, 4,096 verified bytes |
| `4D7u5KF7SP8`, MP4 | ANDROID_VR, itag 140, AAC/M4A, HTTP 206, 4,096 verified bytes |
| `5NV6Rdv1a3I`, music video audio | ANDROID_VR, itag 251, HTTP 206 |
| `m9SMT5ipbxk`, Japanese song | ANDROID_VR, itag 251, HTTP 206 |
| WEB_REMIX inspection | OK with four unresolved ciphered audio formats after timestamp discovery |
| Real media decode | 256 KiB samples of Opus, M4A, and Japanese audio each decoded for one second; FFmpeg exit 0 |

One initial homepage request encountered a transient transport error; repeating the request succeeded. Transport retries are not implemented. Typical successful resolution in this environment took roughly 1.4–1.6 seconds including bootstrap, player request, and CDN probe. Timing is observational, not a performance guarantee. No signed media URLs, cookies, or audio samples are committed. FFmpeg is only a validation tool.

The 25 offline tests include high-bitrate failure fallback, strict container selection, media response limits, malformed/truncated ranges, error pages, expired URLs, redirect host restrictions, and credential isolation.

## Original catalog validation — 0.1.0, 2026-09-16

Performed using the compiled Rust CLI from WSL/Linux, anonymous session, default `en` / `US` request context. YouTube selected the actual region based on network conditions; the country field does not override geolocation.

| Probe | Observed result |
| --- | --- |
| Songs: `Daft Punk Get Lucky` | 20 items; first song `4D7u5KF7SP8`, duration 370 s, three artist links, album `MPREb_K8qWMWVqXGi` |
| Videos / albums / playlists filters | 20 items each |
| Artists: `Daft Punk` | 10 items |
| Unfiltered: `Daft Punk` | 32 items |
| Search continuation | 20 additional items |
| Album browse | `Random Access Memories`; 24 total catalog cards across tracks and recommendations |
| Artist browse | `Daft Punk`; eight sections, 75 total cards |
| Playlist browse | Public playlist returned its title and 32 items |
| Song details | Correct title, artist/channel, 370 s duration, thumbnails |
| Queue / automix | 50 tracks |
| Queue continuation | 49 tracks |
| Lyrics | Nonempty plain text, 3,432 characters; copyrighted lyrics are not stored in the repository |
| Player | `UNPLAYABLE`, reason `Video unavailable`; metadata remains available |
| Stream | JSON `unplayable` error, exit 1; no playable URL claimed |

Reduced public search, queue, and blocked-player responses are in `tests/fixtures/`. Visitor IDs, response context, menus, and tracking fields were removed. Lyrics tests use invented two-line text. Offline tests cover durations, links, renderer variants, per-section tokens, unsupported stream challenges, structured errors, bootstrap string decoding, Cookie hashing, and C ABI ownership/invalid inputs.

Build checks: formatting, clippy with warnings denied, 14 offline tests, optimized CLI/shared-library build, and a native C host invoking the shared library. Cross-platform validation and release artifacts are recorded in GitHub Actions.
