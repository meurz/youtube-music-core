# Web protocol and validation

## Transport

Version 0.5 uses `WEB_REMIX` for every Innertube operation. It does not send TV,
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

Cookie sessions can expire or be revoked. There is no OAuth refresh grant and no
guaranteed independent Cookie renewal. Re-import a current browser session after
authentication rejection. The browser is not required for subsequent API calls or
stream resolution.

## Web audio resolution

The Music watch page identifies the official player script. Script URLs must use
HTTPS port 443 on `music.youtube.com` or `www.youtube.com`, under `/s/player/`,
ending in `/base.js`. Credentials, query strings, fragments, alternate hosts and
redirects are rejected. The signature timestamp comes from that same script,
which is cached per `MusicClient`. Prepared player code is also retained in a
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
DRM, library writes, downloads and audio decoding are not implemented. Hosts own
playback, later CDN errors, IP-bound URL handling, seeking and re-resolution.
API responses are bounded to 16 MiB. Timeouts apply per request; no automatic
transport retry is performed.

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
- 59 offline tests, fmt and clippy passed. One private-capture test is ignored in
  CI and was run separately against the observed official player. Linux, Windows
  and macOS CI all passed.

These observations cover the tested account, region and tracks, not every account
or full-track playback. No private results, signed URLs, credentials or audio
samples are published.
