# YouTube Music Core

Native Rust client for YouTube Music's unofficial Web Innertube API. Includes a reusable blocking library, the `ytmusic` JSON CLI, and a C ABI for native application hosts. Sign in on Google's official website and import the Music browser session; all subsequent API calls and audio resolution run locally without a browser, Node.js, Python, or yt-dlp process.

**Status: experimental 0.7.0.** Search/suggestions, home/explore, accounts, all six library categories, library/playlist writes, lyrics, contextual queues, and playback use `WEB_REMIX`. Web audio signatures and `n` parameters are resolved by a bounded embedded QuickJS engine using a pinned player parser. No TV, Android, or VR fallback is used. Google can change its private protocol or require additional playback attestation. See [validation and limitations](docs/protocol.md).

## Install

Download an archive for Linux, macOS, or Windows from [Releases](https://github.com/meurz/youtube-music-core/releases). Each archive contains the CLI, native shared library, C header, documentation, and MIT license. Verify against `SHA256SUMS`, extract, and run `./ytmusic --help` (Windows: `.\ytmusic.exe --help`).

With Rust 1.91 or newer:

```sh
cargo install --git https://github.com/meurz/youtube-music-core --tag v0.7.0 --locked
ytmusic search 'Daft Punk Get Lucky' --filter songs --pretty
```

## CLI

```sh
ytmusic capabilities
ytmusic home --pretty
ytmusic explore --pretty
ytmusic suggestions 'Daft'
ytmusic search 'Daft Punk' --filter artists --pretty
ytmusic search 'Daft Punk Get Lucky' --filter songs --pretty
ytmusic song 4D7u5KF7SP8 --pretty
ytmusic browse MPREb_K8qWMWVqXGi --pretty
ytmusic queue 4D7u5KF7SP8 --pretty
ytmusic lyrics 4D7u5KF7SP8 --pretty
ytmusic player 4D7u5KF7SP8 --pretty
ytmusic stream 4D7u5KF7SP8 --pretty
ytmusic stream 4D7u5KF7SP8 --format mp4 --pretty
```

Search filters: `all`, `songs`, `videos`, `albums`, `artists`, `playlists`.
Use a result's `video_id` for track operations and `browse_id` for albums/artists.
For a playlist, use `ytmusic playlist PLAYLIST_ID`; the `VL` browse prefix is added automatically.
Each section carries its own continuation token. Fetch another page with
`ytmusic continue search 'TOKEN'` (or `browse` / `next`, matching the original endpoint).
These tokens are opaque and expire; pages are fetched on demand.

All operation output is JSON on stdout:

```json
{"ok":true,"data":{"sections":[]}}
```

Failures use `{"ok":false,"error":{"code":"stream_unavailable","message":"..."}}` and exit code 1.
CLI argument errors use clap's stderr help and exit code 2. Result arrays contain actual IDs, never display indices. The `player` command preserves track metadata even when `status` is `UNPLAYABLE` or URL transforms fail; `resolution_error` reports a sanitized transform failure and unresolved formats stay excluded. `stream` succeeds only after a media probe succeeds.

### Account login and personal library

Sign in at the official Music website and import its browser session. No OAuth application, Google Cloud project, device code, or localhost login page is required.

```sh
ytmusic auth login --no-open
# Open the returned Google login_url and sign in.
# Import the signed-in Music session using one of the methods below.
```

`auth login` opens Google's official sign-in page and returns import instructions; it does not claim that opening the page has authenticated the CLI. Login/MFA remains in your browser. The core handles Cookie authentication, account verification, and all Web API calls; browser integration and encrypted storage belong to the CLI or host.

With Chrome/Edge already exposing a **local** debugging port and a signed-in Music tab:

```sh
ytmusic auth import --browser-port 9222
ytmusic auth status
ytmusic auth refresh
ytmusic account
ytmusic library playlists --pretty
ytmusic library likes --pretty
ytmusic playlist PLAYLIST_ID --pretty
```

For a new browser session, start Chrome/Edge with `--remote-debugging-port=9222 --user-data-dir=PATH_TO_DEDICATED_PROFILE`, then run `ytmusic auth login --browser-port 9222`. Finish sign-in/MFA and select the intended account in the Music tab; browser capture waits up to 600 seconds (`--wait-seconds` can shorten this). Recent Chromium versions require a non-default user-data directory for debugging. Keep the port local. The CLI reuses the first Music tab or opens one, reads account selection, and captures the exact Cookie header on a harmless same-origin request. Browser partitioned cookies retain their actual request ordering. It does not read password databases or unrelated sites' cookie stores.

For manual browser-session import, open Developer Tools > Network, reload Music and copy the request headers of a `music.youtube.com/youtubei/v1/browse` request to a private local file:

```sh
ytmusic auth import --headers-file /path/to/browser-headers.txt
# Alternatively, pipe headers to: ytmusic auth import --stdin
```

Raw/split-line browser headers, JSON header objects, raw Cookie strings, and Netscape exports are accepted. Prefer exact request headers, including `X-Goog-AuthUser` and `X-Goog-PageId` when present, for account selection. Netscape exports filter unrelated/expired cookies and reject ambiguous duplicates; they default to account index 0 (override with `--auth-user N`). Captured Authorization hashes are discarded and recalculated. Delete temporary header files after import; never put credentials in shell arguments or commit them.

Browser import verifies `account/account_menu` before saving. Linux/WSL uses an initialized `pass` store at `youtube-music-core/session/PROFILE`. Windows/macOS use the OS credential store for a random encryption key and an AES-256-GCM encrypted session file under per-user application data; there is no plaintext fallback. An unreadable store produces `credential_storage`, not an anonymous fallback. Browser sessions can expire or be revoked; re-import when `auth status` reports `rejected`.

Use `--profile personal` for separate accounts and `--store auto|pass|keyring` to select storage. Keep these options consistent across commands. The default profile loads automatically unless explicit config credentials or `--anonymous` are supplied. `ytmusic --anonymous search 'Daft Punk'` bypasses stored credentials. `ytmusic auth logout` removes only the selected local profile and does not sign the browser out of Google.

| Library section | Contents |
| --- | --- |
| `playlists` | Owned and saved playlists |
| `likes` | Liked songs playlist |
| `songs` | Songs saved to the library |
| `albums` | Saved albums |
| `artists` | Artists in the library |
| `subscriptions` | Subscribed artists |

All six sections use the selected Music Web account. Use a returned `browse_id` with `browse`, or a playlist ID with `playlist`. Fetch library pages with `ytmusic library playlists --continuation 'TOKEN'` (retain the original section). Each library call verifies `account/account_menu`; rejected sessions cannot masquerade as empty libraries. Library and playlist writes are also available through Rust and JSON `call`; see the request examples below.

**Upgrading from 0.4:** existing browser profiles still work. TV OAuth profiles are not convertible to browser sessions and now return an explicit import instruction; run `ytmusic auth import --browser-port 9222` or import browser headers into the same profile. The import replaces the selected profile only after account verification. Rust OAuth APIs and the `android_vr` playback setting were removed; migrate hosts to `BrowserSession` and `web_remix`/`auto` (both Web-only).

### Audio streaming

`stream` uses the official Music Web player. It discovers the current player script and its signature timestamp, resolves the returned signature/`n` challenges in a restricted embedded engine, then checks audio formats by descending bitrate. `--format mp4` selects AAC/M4A; `--format webm` selects Opus. The default is `any`. A requested container is never silently changed.

The result includes `url`, `itag`, `mime_type`, `bitrate`, `content_length`, `expires_at` (Unix seconds), `source_client`, and `http_headers`. Pass those playback headers along with the URL to your media player. No account cookies are included. `verification` records the HTTP status, bytes read, and content type of a successful probe. The core reads at most 4 KiB per probe and checks the byte range and container header. URLs can expire or be tied to the requesting IP; resolve again after expiry or a later playback failure. A successful initial probe does not guarantee the entire track will remain available.

First use analyzes the current player script and can take tens of seconds. Reuse a persistent client and schedule `prewarm` off the UI thread. Player source expires after six hours; up to eight verified streams are cached for five minutes while they have more than 90 seconds of validity left. `prefetch` resolves one to three upcoming tracks, `stream_refresh` bypasses a track’s URL cache, and `playback_reset` invalidates source/URL state. Qualifying stream failures get one fresh bootstrap. Separate CLI processes repeat cold preparation; a one-shot `ytmusic prewarm` does not warm another process.

`player` exposes candidate formats without CDN probing; their `verification` field is `null`. Use `stream` when handing a URL to a player. This release was tested with real Opus and M4A CDN responses and one-second audio decoding; FFmpeg was used for validation only and is not a runtime dependency.

Send a request through stdin:

```sh
printf '%s\n' '{"op":"search","query":"Daft Punk","filter":"songs"}' | ytmusic call
```

### Discovery, library edits and desktop playback

The full API is exposed as JSON Request objects through `ytmusic call` and the
persistent C ABI. Examples below are individual requests; send one per call:

```json
{"op":"home","params":null,"continuation":null}
{"op":"queue_context","video_id":"4D7u5KF7SP8","index":0}
{"op":"prewarm"}
{"op":"prefetch","video_ids":["4D7u5KF7SP8"],"format":"mp4"}
{"op":"stream_refresh","video_id":"4D7u5KF7SP8","format":"mp4"}
{"op":"create_playlist","title":"My queue","privacy":"private","video_ids":[]}
{"op":"rate_song","video_id":"4D7u5KF7SP8","rating":"like"}
```

Writes include ratings (`indifferent` removes a rating), saved-library actions,
subscriptions, playlist creation/deletion/metadata/privacy, and track
append/remove/reorder. Returned `actions` contain available state and save/remove
tokens; unique `set_video_id` values identify playlist entries, including duplicate
songs. Reload state after editing and use fresh action tokens. See the
[complete request reference](docs/protocol.md#desktop-json-contract--protocol-11--abi-2)
for exact fields. Queue/create/edit fields are flat, never nested under `context`
or `options`.

Home/explore expose filter parameters and feed continuations separately from
carousel continuations. `timed_lyrics` uses official Web timings if present;
otherwise it returns plain text with `timed:false` and
`timing_availability:"not_provided_by_web"`. `accounts` lists identities exposed
by the imported Music session, with reusable selectors when available. Verified
account selection returns a new independent client; browser-wide account
enumeration and recovery of revoked sessions are not implied.

Errors add `retryable`, optional `http_status`, `retry_after_seconds` and
`cause_code`. An uncertain failure after sending a write returns
`mutation_outcome_unknown` with `retryable:false`: cancellation does not prove
that the server rolled back the change. Reload and reconcile before retrying.

## Configuration

Use `--config /path/to/config.json` or `YTMUSIC_CONFIG`. All fields are optional:

```json
{
  "language": "en",
  "country": "US",
  "timeout_seconds": 30
}
```

Additional fields: `cookie_expirations` (name-to-Unix-expiry metadata from an exported session), `proxy` (HTTP/SOCKS URL), `cookie` (raw Cookie header), `visitor_data`, `po_token`, `client_version`, `auth_user` (default 0), `delegated_session_id` (optional brand/channel ID), `playback_client` (`auto` or `web_remix`; both use Web).
Prefer `auth import` for secure credential storage. Config cookies require a private file outside the repository; never put them in command arguments. Config cookie input is a header string, not a Netscape file. Old nonempty `oauth`/`music_oauth` configuration is rejected with a migration instruction.

The client discovers the current WEB_REMIX version and visitor data from the Music homepage. `client_version` can override catalog bootstrap. Playback discovers the official player script from the Music watch page, downloads it without account headers, and caches it per client. Only official YouTube HTTPS player-script URLs are accepted. The embedded solver has no filesystem, network, process, or host callbacks and enforces time/memory/stack limits. Its pinned source and licenses are in [vendor](vendor/yt-dlp-ejs/README.md); it does not download replacement solver code at runtime.

Cookies are sent only to the fixed Music origin; CDN requests have no account credentials. API/static-script redirects are rejected. Media probes allow at most three redirects restricted to HTTPS Google video CDN URLs, trying at most eight formats. API responses are limited to 16 MiB. Eligible reads retry network errors/timeouts and HTTP 429/502/503/504 at most three attempts, honoring server retry delays up to ten seconds. Writes never retry automatically. A total-operation deadline includes bootstrap, player work, retries and probes; the default is 120 seconds (`--timeout-ms` in the CLI), in addition to per-request timeouts. Native operation handles can cancel HTTP I/O, QuickJS execution and lock waits; CLI Ctrl+C requests cancellation. Standard proxy environment variables are supported; signed media URLs may be tied to the requesting IP.

Music responses now update root-scoped Cookie values and expiry metadata. The CLI verifies changed sessions before saving them to its encrypted profile; `auth refresh` explicitly visits the Music homepage, verifies the account, and saves updates. A newer import or logout wins over a late automatic save. Normal command results remain available if background persistence fails, with a sanitized stderr warning; explicit refresh reports failure. This maintains an active session without a browser process, but cannot restore a revoked session: re-import when rejected. PO-token generation, DRM and SABR delivery are not implemented. If Google requires additional attestation or changes the player layout, the core reports an error instead of switching to another client.

## Rust library

```rust,no_run
use youtube_music_core::{Config, MusicClient, model::SearchFilter};

# fn main() -> youtube_music_core::Result<()> {
let client = MusicClient::new(Config::default())?;
let page = client.search("Daft Punk", SearchFilter::Songs)?;
for section in page.sections {
    for song in section.items {
        println!("{} {:?}", song.title, song.video_id);
    }
}
# Ok(())
# }
```

Reuse `MusicClient` to reuse HTTP connections. This is a **blocking API**; async applications must use a blocking worker. Playback/decoding stays in the host application. Call `client.stream(video_id)` for automatic format selection or `client.stream_format(video_id, model::AudioFormat::Mp4)` for M4A.

For account access, your host obtains the browser session and owns its secure persistence:

```rust,no_run
use youtube_music_core::{auth::BrowserSession, library::LibrarySection, Config, MusicClient};

# fn main() -> youtube_music_core::Result<()> {
let headers = std::fs::read_to_string("/private/browser-headers.txt").expect("private input");
let session = BrowserSession::from_browser_headers(&headers)?;
let mut config = Config::default();
session.apply_to(&mut config)?;
let client = MusicClient::new(config)?;
let account = client.account()?;
let playlists = client.library(LibrarySection::Playlists, None)?;
let updated = client.browser_session()?; // secret: encrypt in the host store
let refresh = client.refresh_session()?; // metadata only; export again to persist
# Ok(())
# }
```

`BrowserSession` Debug output is redacted, but serialization intentionally contains credentials for host-provided secure storage. Never log serialized sessions or `Config`. All account and playback operations use this same Web session. Reuse `MusicClient` to cache the player script and HTTP connections.

## C ABI / native hosts

See [include/youtube_music_core.h](include/youtube_music_core.h) and [examples/ffi.c](examples/ffi.c).
`ytmusic_core_call` accepts `{"config":{},"request":{"op":"search","query":"Daft Punk"}}` and returns an allocated UTF-8 JSON string. Release it exactly once with `ytmusic_string_free`. Do not use the host allocator. This legacy entry point is synchronous and creates a new client for each call. It does not return or persist updated session credentials.

For desktop hosts, retain a handle from `ytmusic_client_create(config_json)` and pass inner Request objects to `ytmusic_client_call(handle, request_json)`. Export verified Cookie state only through `ytmusic_client_export_session(handle)` into secure host storage, then release the handle with `ytmusic_client_destroy(handle)`. All results use the same JSON envelope and string allocator. Destroyed handles are rejected; already-running calls finish safely. `auth_refresh` returns metadata, never cookies. See the [compilable .NET wrapper](examples/dotnet) and [WinUI 3 host guide](docs/winui-host.md).

ABI 2 adds caller-controlled operations. Allocate a single-use handle with
`ytmusic_operation_create({"timeout_ms":120000})`, then dispatch a
`ytmusic_client_*_with_operation` call on a worker. Poll
`ytmusic_operation_status` or signal `ytmusic_operation_cancel` from another
thread, and always destroy the operation handle. The deadline starts at
allocation and includes queue waits. `ytmusic_client_select_account` also takes
an operation and returns a new verified client handle. The .NET wrapper connects
native cancellation to `CancellationToken`. `ytmusic_capabilities()` reports
protocol 1.1 / ABI 2 and limits without creating a client or making a request.

The CLI's `call` command accepts the inner `request` object; the C ABI accepts the wrapper with optional `config`. Account operations are `{"op":"auth_status"}`, `{"op":"account"}`, and `{"op":"library","section":"playlists","continuation":null}`. The C host supplies browser `cookie`, `auth_user`, and optional `delegated_session_id` in `config` from its own secure store; the library does not implicitly load CLI profiles.

Streaming through the C ABI uses `{"request":{"op":"stream","video_id":"4D7u5KF7SP8","format":"mp4"}}`. The `format` field is optional. Existing JSON requests continue to work. Rust callers constructing `Request::Stream` include `format`.

## Development

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

Tests run offline using reduced public responses and edge-case fixtures. GitHub Actions checks Linux, Windows, and macOS. The release matrix covers Linux x64/ARM64, Windows x64/ARM64 and macOS Intel/Apple Silicon; each archive requires its native release build to pass. Live probes are manual because region, account state, catalog changes, and anti-bot controls are outside the library's control.

This project is unofficial and is not affiliated with Google or YouTube. MIT licensed.
