# Web attestation, SABR and protected playback

Version 0.8 adds three distinct delivery paths while retaining `WEB_REMIX` and
imported Music sessions. Use the [Windows official-player helper](../examples/dotnet/web-player/README.md)
when the host needs the browser's attestation VM or licensed CDM. Ordinary direct
URL playback and native SABR do not require a browser when the server permits them.

## Proof of Origin

The optional provider executes Google's current challenge and VM inside a real,
signed-in `https://music.youtube.com` page, obtains an integrity response and mints
purpose-specific tokens. The Web Music PLAYER binding is the full Data Sync ID;
GVS binding follows the official player's content/session experiment. The core
never substitutes its signature-transform sandbox for a browser attestation VM.

CLI example, using an existing explicitly enabled local browser debugging port:

```sh
ytmusic stream 4D7u5KF7SP8 --format mp4 --attestation-browser-port 9222
ytmusic sabr 4D7u5KF7SP8 --format webm --output track.webm --attestation-browser-port 9222
```

Keep a signed-in Music player tab open. The saved core profile and browser must
have the same signing session and selected identity. The CLI captures Music-only
request headers before and after generation, rejects account changes, and does
not print or persist the minted tokens. A Windows host can instead use
`OfficialWebPlayer.RefreshPoTokensAsync` on its dedicated WebView2.

Native hosts can provide `Config.po_tokens` or call `set_po_tokens` with a maximum
of 16 `PoTokenBundle` values: `video_id`, `player_token`, `gvs_token`, `expires_at`,
`session_binding`. Use `attestation::session_binding` for the actual imported
session. This fingerprint is a local account-safety check, not a Google token
claim. `attestation_context` returns the official-page provider script, fingerprint,
account selection and `max_expires_at` for a trusted host. **Its output is sensitive.**
Never attach a core fingerprint to an unrelated browser's generated tokens: the
Windows helper checks the actual signing cookies and selected account before and
after minting. All new bundles are validated before replacing the current set.

Tokens remain in memory, are limited to one hour, and are rejected within 30
seconds of expiry. GVS URL/cache lifetime is bounded by both URL and token expiry.
Updating tokens invalidates resolved stream caches. SABR reads recheck the current
bundle; refresh the browser proof on `po_token_required` and retry the read.
`set_po_tokens` replaces the complete set; pass an empty array to clear it.
The legacy `po_token` configuration field only supplies an unscoped PLAYER token;
new integrations should use the scoped bundles.

Browser generation has its own 25-second deadline and VM cleanup. Cancelling a
CDP wait stops waiting; it does not terminate the entire official page. The page's
bounded provider cleans itself up. Native network and SABR reads retain actual
I/O cancellation. Browser availability and Google protocol changes remain external
requirements; no third-party token service is used.

## Native SABR audio

The core implements audio-only VOD SABR requests and UMP/protobuf parsing in Rust.
`player.sabr` describes resolved delivery and clear formats; it contains signed
parameters, so never log it. It is **not** a URL to hand directly to a media player.
The normal `stream` operation preserves its verified-direct-URL contract and can
return `sabr_required` when a native SABR session is needed.

Use a persistent `MusicClient` or persistent C ABI/.NET handle:

| Operation | Request fields | Result |
| --- | --- | --- |
| `sabr_open` | `video_id`, optional `format` (`any`, `mp4`, `webm`) | Verified initialization and selected `handle`, `itag`, MIME type and duration |
| `sabr_read` | `handle` | One bounded complete init/media segment |
| `sabr_seek` | `handle`, `position_ms` | Reset the segment cursor and decoder epoch |
| `sabr_close` | `handle` | Release the session |

There are at most four sessions per client. Format selection tries at most four
candidates within the requested container, in descending advertised bitrate, and
verifies initialization before reporting the selected format. A server reload
request gets one fresh official player context before retrying. There is no
format switch after bytes have been delivered. Authentication, proof, network,
rate-limit and cancellation failures are not hidden by format fallback.

Rust `sabr_read` returns `SabrChunk.data: Vec<u8>`. JSON/C ABI uses `data_base64`
and the same `is_init`, `sequence`, `start_ms`, `duration_ms`, `finished` metadata.
Feed the original initialization and media bytes into a compatible host demuxer;
the response contains media containers, not encoded PCM or a new CDN URL. Discard
old buffered data and reset the decoder after seek. The first segment may begin
before the requested position; the host uses timestamps to reach the exact target.
Stop reading when `finished` is true, processing any returned bytes first.

Reads are transactional: cancellation or an incomplete UMP response does not
advance the committed cursor. Segments are bounded to 16 MiB, each response and
queued media to 32 MiB. Redirects remain on the official HTTPS Google video CDN;
Music Cookies and account authorization headers are never sent there. Close waits
are not a cancellation mechanism: cancel and await an in-flight operation before
closing its handle. One-shot C calls cannot retain SABR handles.

`ytmusic sabr VIDEO --output FILE` runs the complete transfer in one process and
publishes a new file only after completion, without overwriting an existing file.
The original AAC fragmented-MP4 edit-list issue in Windows' direct-file pipeline
still applies; use a compatible demuxer/adaptive path, WebM, or the official Web
player. An offline complete-file test does not prove a host's progressive SABR
playback implementation. Live broadcasts are not supported by this VOD reader.

## DRM and the official player

Encrypted formats are excluded from clear direct URLs, clear DASH and native
SABR candidates. `player.drm` reports families, key systems and the official watch
route without exposing license URLs or DRM challenges. `drm_required` tells the
host to use a licensed player. `official_playback` is a local operation returning
the official Music watch page for a validated video ID, even if native bootstrap
or media access is unavailable.

The optional Windows helper loads that real page in a host-owned WebView2. The
page handles EME, CDM selection and official license acquisition. The helper
supports key-system probing, playback, pause, seek and state; probe the actual
runtime instead of assuming every installation has Widevine or PlayReady.
`DrmRouteAsync` selects the embedded official page or indicates that a compatible
full browser is needed. A CDM capability probe never claims a license was granted.

Use a dedicated protected WebView profile. Cookie-header import cannot reconstruct
partitioned cookies and rejects ambiguous duplicates; use official-page login when
necessary. Non-default/brand identity selection stays in the official page and
must be verified. The core neither extracts content keys nor implements its own
CDM. Account, purchase/subscription, region and device license requirements remain
in force. A compatible CDM does not grant access to an unauthorized track.

## Validation evidence

Native official-browser minting passed a real CLI PLAYER/GVS/CDN round trip with
HTTP 206. The tested track already played without mandatory attestation, so this
does not claim every enforced-PO scenario is covered. Native SABR AAC and Opus
completed full tracks, produced decodable original media, and passed mid-track
seek tests. Offline tests cover framing limits, malformed protobufs, transactional
segments, redirect validation, proof state and actual HTTP cancellation.

On the tested Windows WebView2 runtime, the official Music page passed playback,
seek and pause. A public, authorized Widevine test vector separately passed license
responses, usable keys, real playback-clock advancement, seek and full encrypted
playback to natural end. No known DRM-protected YouTube track was supplied, so a
YouTube account's encrypted-track license is not claimed as live-verified.
