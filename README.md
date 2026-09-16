# YouTube Music Core

Native Rust client for YouTube Music's unofficial Innertube API. Includes a reusable blocking library, the `ytmusic` JSON CLI, and a C ABI for native application hosts. No browser, Node.js, Python, or yt-dlp runtime is required.

**Status: experimental 0.1.0.** Catalog access works anonymously where YouTube Music is available. Playback is conditional: this version exposes direct audio URLs only. It does **not** implement player JavaScript signature/n deciphering or generate PO tokens. A blocked player returns a structured error, not a fabricated playable URL. See [validation and limitations](docs/protocol.md).

## Install

Download an archive for Linux, macOS, or Windows from [Releases](https://github.com/meurz/youtube-music-core/releases). Each archive contains the CLI, native shared library, C header, documentation, and MIT license. Verify against `SHA256SUMS`, extract, and run `./ytmusic --help` (Windows: `.\ytmusic.exe --help`).

With Rust 1.91 or newer:

```sh
cargo install --git https://github.com/meurz/youtube-music-core --tag v0.1.0 --locked
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

Failures use `{"ok":false,"error":{"code":"unplayable","message":"..."}}` and exit code 1.
CLI argument errors use clap's stderr help and exit code 2. Result arrays contain actual IDs, never display indices. The `player` command succeeds when it can inspect a player response, even if `status` is `UNPLAYABLE`; `stream` fails in that case.

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

Additional fields: `proxy` (HTTP/SOCKS URL), `cookie` (raw Cookie header), `visitor_data`, `po_token`, `client_version`, `auth_user` (default 0).
Use a private file outside the repository for credentials; do not put cookies in command arguments. Cookie input is a header string, not a Netscape cookie file. Cookie login, account switching, and externally supplied PO tokens are supported inputs but have not been verified with an authenticated account.

The client obtains the current WEB_REMIX client version and visitor data from the homepage. Set `client_version` explicitly when bootstrap is blocked. HTTP redirects are rejected; consent/login redirects must be resolved by the caller. Standard proxy environment variables are supported by reqwest. No automatic retries are performed. The timeout is per HTTP request (bootstrap plus API requests can take multiple timeouts). Responses are limited to 16 MiB.

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

Reuse `MusicClient` to reuse HTTP connections. This is a **blocking API**; async applications must use a blocking worker. Playback/decoding stays in the host application. `stream` selects the highest-bitrate ready audio format; URLs expire and may be tied to the requesting session/IP.

## C ABI / native hosts

See [include/youtube_music_core.h](include/youtube_music_core.h) and [examples/ffi.c](examples/ffi.c).
`ytmusic_core_call` accepts `{"config":{},"request":{"op":"search","query":"Daft Punk"}}` and returns an allocated UTF-8 JSON string. Release it exactly once with `ytmusic_string_free`. Do not use the host allocator. Calls are synchronous and each creates a client; use the Rust API for a persistent session.

The CLI's `call` command accepts the inner `request` object; the C ABI accepts the wrapper with optional `config`.

## Development

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

Tests run offline using reduced public responses and edge-case fixtures. GitHub Actions checks Linux, Windows, and macOS and builds five native release targets. Live probes are manual because region, account state, catalog changes, and anti-bot controls are outside the library's control.

This project is unofficial and is not affiliated with Google or YouTube. MIT licensed.
