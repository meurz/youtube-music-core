# YouTube Music Core

Native Rust client for YouTube Music's unofficial Innertube API. Includes a reusable blocking library, the `ytmusic` JSON CLI, and a C ABI for native application hosts. No browser, Node.js, Python, or yt-dlp runtime is required.

**Status: experimental 0.2.0.** Catalog access and audio streaming work anonymously where YouTube permits them. The native Android VR player profile supplies direct Opus/WebM and AAC/M4A audio; `stream` verifies a small CDN byte range before returning a URL. Player JavaScript signature/n deciphering and PO-token generation are not implemented. Account, region, and anti-bot restrictions may still block a track. See [validation and limitations](docs/protocol.md).

## Install

Download an archive for Linux, macOS, or Windows from [Releases](https://github.com/meurz/youtube-music-core/releases). Each archive contains the CLI, native shared library, C header, documentation, and MIT license. Verify against `SHA256SUMS`, extract, and run `./ytmusic --help` (Windows: `.\ytmusic.exe --help`).

With Rust 1.91 or newer:

```sh
cargo install --git https://github.com/meurz/youtube-music-core --tag v0.2.0 --locked
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

Additional fields: `proxy` (HTTP/SOCKS URL), `cookie` (raw Cookie header), `visitor_data`, `po_token`, `client_version`, `auth_user` (default 0), `playback_client` (`auto`, `android_vr`, or `web_remix`; default `auto`).
Use a private file outside the repository for credentials; do not put cookies in command arguments. Cookie input is a header string, not a Netscape cookie file. Cookies and externally supplied PO tokens apply to WEB_REMIX; Android VR is always anonymous. Set `playback_client` to `web_remix` to inspect the account-backed web player specifically. Cookie login, account switching, and externally supplied PO tokens have not been verified with an authenticated account. Cipher-only web formats still fail with an explicit signature/attestation diagnostic.

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

## C ABI / native hosts

See [include/youtube_music_core.h](include/youtube_music_core.h) and [examples/ffi.c](examples/ffi.c).
`ytmusic_core_call` accepts `{"config":{},"request":{"op":"search","query":"Daft Punk"}}` and returns an allocated UTF-8 JSON string. Release it exactly once with `ytmusic_string_free`. Do not use the host allocator. Calls are synchronous and each creates a client; use the Rust API for a persistent session.

The CLI's `call` command accepts the inner `request` object; the C ABI accepts the wrapper with optional `config`.

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
