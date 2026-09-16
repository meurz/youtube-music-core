# Protocol and validation

## Transport

The client bootstraps `https://music.youtube.com/` using a desktop browser User-Agent and reads the public `INNERTUBE_CLIENT_VERSION` and `VISITOR_DATA` configuration. Observed WEB_REMIX version on 2026-09-16: `1.20260913.16.00`. The version is discovered at runtime, not compiled into the library.

POST requests go to `https://music.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false`, with JSON `context.client` fields `clientName=WEB_REMIX`, `clientVersion`, `hl`, `gl`, and optional `visitorData`. Headers include Origin/Referer and client number `67`. These endpoints accepted requests without an API key during validation.

| Operation | Endpoint | Main request fields |
| --- | --- | --- |
| Search | `search` | `query`, optional filter `params` |
| Album / artist / playlist | `browse` | `browseId` (playlist IDs gain `VL`) |
| Pagination | Original endpoint | `continuation` |
| Song / player / stream | `player` | `videoId`, content checks, optional `serviceIntegrityDimensions.poToken` |
| Queue | `next` | `videoId`, audio-only/persistent-panel flags; follows one returned automix playlist endpoint |
| Lyrics | `next`, then `browse` | Finds the selectable lyrics tab and uses its `browseId` |

The parser recognizes responsive rows, two-row cards, queue videos, shelf/carousel/grid containers, and their continuations. Per-section continuation tokens are preserved. Missing optional fields are `null` or empty arrays. An empty recognized page is valid; a response without page contents is a protocol error. Layouts are private API contracts and may change.

## Authentication and playback boundary

Optional Cookie headers are sent only to the fixed music origin; redirects are disabled. `SAPISID` (or a secure PAPISID fallback) signs `timestamp + space + cookie_value + space + origin` with SHA-1 to build `SAPISIDHASH`. Authorization is refreshed on every request. Header values are marked sensitive in the HTTP library. Config parsing errors do not echo submitted credentials.

Cookie sign-in, account index selection, and supplied PO tokens are not authenticated-session verified. There is no OAuth flow, cookie extraction, browser automation, token generation, player JS interpreter, DRM handling, or yt-dlp subprocess. Account library mutations, downloads, and audio decoding are outside this release.

`player` separates metadata from playability: song metadata may remain available even when the player is blocked. Only HTTPS audio URLs without an `n` challenge are exposed as ready formats. Cipher-only and n-challenged formats increment `unresolved_audio_formats`. `stream` rejects non-OK playability and returns `stream_resolution_required` if no ready audio remains. Direct URLs are not automatically downloaded or checked against the media CDN; the caller must handle expiry, session/IP binding, and CDN errors. A successful player response alone does not prove end-to-end playback.

## Live validation — 2026-09-16

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
