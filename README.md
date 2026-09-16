# YouTube Music Core

Native Rust client for YouTube Music's unofficial Innertube API. Includes a reusable blocking library, the `ytmusic` JSON CLI, and a C ABI for native application hosts. Catalog and streaming require no browser, Node.js, Python, or yt-dlp runtime. Account access supports official TV device authorization or an existing Music browser session; subsequent API calls run directly in Rust.

**Status: experimental 0.4.0.** Catalog access and audio streaming work anonymously where YouTube permits them. The native Android VR player profile supplies direct Opus/WebM and AAC/M4A audio; `stream` verifies a small CDN byte range before returning a URL. Player JavaScript signature/n deciphering and PO-token generation are not implemented. Account, region, and anti-bot restrictions may still block a track. See [validation and limitations](docs/protocol.md).

## Install

Download an archive for Linux, macOS, or Windows from [Releases](https://github.com/meurz/youtube-music-core/releases). Each archive contains the CLI, native shared library, C header, documentation, and MIT license. Verify against `SHA256SUMS`, extract, and run `./ytmusic --help` (Windows: `.\ytmusic.exe --help`).

With Rust 1.91 or newer:

```sh
cargo install --git https://github.com/meurz/youtube-music-core --tag v0.4.0 --locked
ytmusic search 'Daft Punk Get Lucky' --filter songs --pretty
```

## CLI

```sh
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
CLI argument errors use clap's stderr help and exit code 2. Result arrays contain actual IDs, never display indices. The `player` command succeeds when it can inspect a player response, even if `status` is `UNPLAYABLE`. `stream` succeeds only after a media probe succeeds.

### Account login and personal library

Run the CLI to get an official Google device-authorization link and code:

```sh
ytmusic auth login --no-open
# Open https://www.google.com/device and enter the displayed code.
# Select your account and approve the device access shown by Google.
ytmusic auth status
ytmusic library playlists --pretty
```

`auth login` discovers the current client identity from YouTube TV's public bootstrap script and uses YouTube's device-code/token endpoints. No Google Cloud project, personal OAuth client configuration, localhost page, or Cookie copying is required for this mode. Google may label the device as YouTube on TV. Only approve the code printed by the CLI session you started. The verification link and user code go to stderr; stdout contains the final JSON result. Omit `--no-open` to open the official page automatically. The CLI waits up to the code's lifetime (normally 30 minutes); `--wait-seconds` can shorten the wait.

Google-issued access/refresh tokens are saved in the selected secure profile after authorization. The CLI then verifies the Music account. If that final check fails, the grant stays saved so `auth status` can diagnose API compatibility without another consent step. Access tokens refresh before expiry, and the CLI persists changed tokens securely. Revoked refresh tokens require another login. An official TV grant's compatibility with Music's private API is controlled by YouTube and may change; successfully displaying a code alone does not prove library access works.

Browser import remains available as an alternative. Authentication and account/library APIs belong to the core; browser integration and persistence belong to the CLI/host.

With Chrome/Edge already exposing a **local** debugging port and a signed-in Music tab:

```sh
ytmusic auth import --browser-port 9222
ytmusic auth status
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

TV OAuth supports `playlists`, `likes`, `albums`, and `subscriptions`, plus playlist/browse pagination. Its music library can include automatic Mixes and regular YouTube playlists/videos that the web Music client omits. `songs` (saved library songs) and `artists` (library artists) require a browser profile; OAuth returns an explicit unsupported-section error. Public search, queues, lyrics, and playback continue to use anonymous clients with an OAuth profile. Browser profiles retain their existing account-backed web behavior.

Use a returned `browse_id` with `browse`, or a playlist ID with `playlist`. Fetch library pages with `ytmusic library playlists --continuation 'TOKEN'` (retain the original section). Each library call verifies the selected account (`account/accounts_list` for TV OAuth); rejected sessions cannot masquerade as empty libraries. This release provides read-only library access. Browser/credential-store integration stays in the CLI or your application host.

### Audio streaming

`stream` tries the anonymous `ANDROID_VR` profile first, then `WEB_REMIX`. Within each profile it checks audio formats by descending bitrate, skipping broken CDN URLs. `--format mp4` selects AAC/M4A for hosts that cannot play Opus/WebM; `--format webm` selects Opus. The default is `any`. A requested container is never silently changed.

The result includes `url`, `itag`, `mime_type`, `bitrate`, `content_length`, `expires_at` (Unix seconds), `source_client`, and `http_headers`. Pass those public headers along with the URL to your media player. No account cookies are included. `verification` records the HTTP status, bytes read, and content type of a successful probe. The core reads at most 4 KiB per probe and checks the byte range and container header. URLs can expire or be tied to the requesting IP; resolve again after expiry or a later playback failure. A successful initial probe does not guarantee the entire track will remain available.

`player` exposes candidate formats without CDN probing; their `verification` field is `null`. Use `stream` when handing a URL to a player. This release was tested with real Opus and M4A CDN responses and one-second audio decoding; FFmpeg was used for validation only and is not a runtime dependency.

Send a request through stdin:

```sh
printf '%s\n' '{"op":"search","query":"Daft Punk","filter":"songs"}' | ytmusic call
```

## Configuration

Use `--config /path/to/config.json` or `YTMUSIC_CONFIG`. All fields are optional:

```json
{
  "language": "en",
  "country": "US",
  "timeout_seconds": 30
}
```

Additional fields: `proxy` (HTTP/SOCKS URL), `cookie` (raw Cookie header), `oauth` (host-supplied OAuthSession), `visitor_data`, `po_token`, `client_version`, `auth_user` (default 0), `delegated_session_id` (optional brand/channel ID), `playback_client` (`auto`, `android_vr`, or `web_remix`; default `auto`).
Prefer `auth import` for secure credential storage. Legacy config cookies require a private file outside the repository; never put them in command arguments. Config cookie input is a header string, not a Netscape file. Cookies and externally supplied PO tokens apply to WEB_REMIX; Android VR is always anonymous. Set `playback_client` to `web_remix` to inspect the account-backed web player specifically. Account verification and personal library reads were tested with a real browser session. Multi-account/brand selection has offline coverage; externally supplied PO tokens and private-track playback remain unverified. Cipher-only web formats still fail with an explicit signature/attestation diagnostic.

The client obtains the current WEB_REMIX client version and visitor data from the homepage. Set `client_version` explicitly when bootstrap is blocked. WEB_REMIX player requests additionally discover the signature timestamp from the watch page and cache it per client. API redirects are rejected; media probes allow up to three redirects, restricted to HTTPS Google video CDN URLs. Standard proxy environment variables are supported by reqwest. There are no transport retries; stream resolution falls back across up to eight formats per profile. The timeout is per HTTP request (bootstrap, profile fallback, and probes can take multiple timeouts). API responses are limited to 16 MiB.

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
# Ok(())
# }
```

`BrowserSession` Debug output is redacted, but serialization intentionally contains credentials for host-provided secure storage. Never log serialized sessions or `Config`. `oauth::DeviceAuthClient` exposes begin/poll to application hosts; `OAuthSession::apply_to` selects Bearer authentication and clears browser credentials. After API calls, hosts can retrieve refreshed state with `MusicClient::oauth_session()` for secure persistence. Rust `Config` literals must include the new `oauth` and `delegated_session_id` fields or use `..Default::default()`.

## C ABI / native hosts

See [include/youtube_music_core.h](include/youtube_music_core.h) and [examples/ffi.c](examples/ffi.c).
`ytmusic_core_call` accepts `{"config":{},"request":{"op":"search","query":"Daft Punk"}}` and returns an allocated UTF-8 JSON string. Release it exactly once with `ytmusic_string_free`. Do not use the host allocator. Calls are synchronous and each creates a client; use the Rust API for a persistent session.

The CLI's `call` command accepts the inner `request` object; the C ABI accepts the wrapper with optional `config`. Account operations are `{"op":"auth_status"}`, `{"op":"account"}`, and `{"op":"library","section":"playlists","continuation":null}`. The C host supplies either `oauth` or browser `cookie`, `auth_user`, and optional `delegated_session_id` in `config` from its own secure store; the library does not implicitly load CLI profiles.

Streaming through the C ABI uses `{"request":{"op":"stream","video_id":"4D7u5KF7SP8","format":"mp4"}}`. The `format` field is optional. Existing JSON requests continue to work. Rust callers constructing `Request::Stream` must now include `format`; parsed `AudioStream` and `Player` records have additional fields in 0.2.0.

## Development

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

Tests run offline using reduced public responses and edge-case fixtures. GitHub Actions checks Linux, Windows, and macOS and builds five native release targets. Live probes are manual because region, account state, catalog changes, and anti-bot controls are outside the library's control.

This project is unofficial and is not affiliated with Google or YouTube. MIT licensed.
