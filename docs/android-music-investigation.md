# Android Music protocol investigation

Status: migration blocked on link-based authentication, not released. Observed on 2026-09-17.

The intended migration uses Android Music for every API operation and retains sign-in through an official Google authorization link. Importing an Android phone's credentials is not an accepted sign-in method for this migration. The released 0.4.0 behavior has not been changed.

## Evidence source

- Official installed package: `com.google.android.apps.youtube.music`, version `9.36.50`, version code `93650240`, Android 16.
- Base APK SHA-256: `8d2f97449455d60aaa32a4caf6b91dcfd33c31e270931378688d2414192a4ec2`.
- JADX 1.5.5, four workers. Whole-APK decompilation reported 68 method errors; the classes cited below were readable. This is not a claim that every method was recovered.
- Frida 17.10.1 attached to the running application. Only the target process was temporarily unfrozen; its override was cleared afterward. SELinux remained Enforcing.
- Native requests were observed at the `org.chromium.net.impl.CronetUrlRequest` constructor. Upload bodies were copied from the `ByteBuffer` owned by `awmy`, without consuming or changing the original buffer.

Credentials, account identifiers, raw library results, authorization responses, and APK sources are excluded from this repository. The application access token was used only for isolated protocol validation; it was not installed as the CLI's default session.

## Request and authentication findings

`vwj` constructs requests using `Content-Type: application/x-protobuf`, `X-GOOG-API-FORMAT-VERSION: 2`, and optional gzip compression. The captured application user agent identifies Android Music 9.36.50. JSON requests to `https://youtubei.googleapis.com/youtubei/v1/` with API format version 1 and a matching `ANDROID_MUSIC` context were also accepted with the application's access token.

`rgr` defines the `youtube` and `youtube.force-ssl` OAuth scopes, optionally adding `identity.lateimpersonation`. `jpr` binds to `com.google.android.gms.auth.GetToken`; its `g` method obtains `com.google.android.gms.auth.TokenData`. This is a Google Play services token acquisition path. It does not demonstrate an independent desktop refresh-token flow.

Google's token-info endpoint reported `youtube`, `youtube.force-ssl`, `identity.lateimpersonation`, and `accounts.reauth` for the captured app token. The existing TV grant reported `youtube` and `youtube-paid-content`. The audience/client identifiers differ. These observations do not by themselves prove which server-side check rejects a token.

### Controlled authentication comparison

| Request | App access token | Existing TV access token |
| --- | --- | --- |
| Android Music JSON saved songs | HTTP 200, 3 songs | HTTP 400, `INVALID_ARGUMENT` |
| Android Music JSON library artists | HTTP 200, 5 artists | HTTP 400, `INVALID_ARGUMENT` |
| Captured native protobuf browse request, only Authorization changed | HTTP 200 | HTTP 400 |

Using the captured API key or the installed emulator's 6.49.53 client profile did not make the TV token work for saved songs. This narrows the problem to authentication compatibility; changing the parser or merely renaming the client is insufficient.

## Link authorization probes

| Probe | Observed result |
| --- | --- |
| Device-code request using the Android token's audience or authorized-party client ID | HTTP 401, `invalid_client` |
| Existing official TV client requesting `youtube` plus `youtube.force-ssl` | HTTP 400, `invalid_scope`; Google explicitly rejected the latter device-flow scope |
| Standard Google authorization-code URL with the Android client, PKCE, and a reverse-client-ID custom URI | Google's authorization error page: `invalid_request`, “Custom scheme URI not allowed.” |
| Standard authorization URL with the Android client and the legacy out-of-band redirect | Google's authorization error page: `invalid_request`, obsolete security-flow message |

The existing TV grant was not replaced. These probes establish failures for the specific tested flows, not impossibility for all official clients or future protocols. No working official-link-to-Android-session path has been established.

### Additional link research and unverified-app warning

- The publicly distributed Google Cloud SDK OAuth configuration rejects the YouTube scopes with `restricted_client`. Its device-code request also failed with `invalid_client`.
- The OAuth Account Manager `v1/issuetoken` endpoint rejected the existing TV refresh token with HTTP 401 and the TV access token with HTTP 403. No TV-to-Music token conversion was demonstrated.
- Static inspection of the modified **YTMusicUltimate 2.3.1 IPA**, containing Music 6.51.1, found a candidate public client in `GoogleService-Info.plist`. The IPA was not executed or installed. Its modified provenance means the recovered identifiers cannot all be assumed to be original Google configurations.
- That candidate accepts an authorization-code URL with PKCE and a reverse-client-ID URI callback far enough to display Google's login/consent pages. The user then reported **“Google hasn't verified this app”**, showing developer `yt-woodstock-eng@google.com`. The displayed email is recorded evidence, not sufficient verification of the client or this use of it.
- The user has not yet confirmed continuing past that warning. No callback, code exchange, refresh token, Android library compatibility, or independent refresh has been verified for this candidate. Opening the authorization page is not evidence that the entire flow works.

The branch now contains experimental Rust PKCE/session handling, Android transport,
renderer normalization, and playback routing for an explicitly configured native
session. `auth login` still uses the released TV device flow; there is no production
native callback handler yet. Anonymous and legacy profiles retain their existing
routes, so this is not a completed Android-only migration. The experimental modules
must not be presented as a verified official login or released as the completed
migration before authentication and end-to-end API tests pass.

## API coverage with the app token

- `account/accounts_list`: HTTP 200; selected account is represented by `accountItem`. `account/account_menu` returned HTTP 400.
- Library: 8 playlists, 3 saved songs, 5 library artists, 16 subscriptions, and 13 liked songs; albums returned an explicit empty-state message. Saved songs and library artists now match Music semantics instead of the TV substitutes.
- `search` and `next`: HTTP 200. Modern search uses `elementRenderer` models such as `musicTopResultCardShelfModel`; Android library items use `musicTwoColumnItemRenderer`. Experimental normalization has synthetic unit coverage, but parsed live responses and full pagination remain unverified. A successful HTTP response is not a completed integration.
- `player`, 9.36.50: playable metadata and format descriptions, but media delivery uses `serverAbrStreamingUrl` rather than per-format direct URLs. SABR delivery was not implemented or validated.
- `player`, installed emulator profile 6.49.53 / SDK 34: returned direct audio URLs. M4A itag 140 and Opus itag 251 each passed a bounded 4 KiB CDN request with HTTP 206. These requests sent no account token to the CDN.
- Lyrics, parsed search results, real pagination, independent Android-token renewal, and an end-to-end Android-only CLI remain unverified.

The next implementation gate is a verified official-link authentication flow accepted by Android Music. The phone-token experiments cannot satisfy that requirement and must not be presented as the migration's login solution.
