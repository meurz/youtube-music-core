# Official Web playback and DRM

This optional Windows helper hosts the **real `music.youtube.com/watch` page**.
The page owns EME, its CDM and official license requests. Rust does not decrypt
protected media, create licenses or convert Widevine into PlayReady.

Use `WindowsMusicSource` for clear AAC/DASH. When `player.drm` reports protected
formats, this helper provides a browser playback route with play, pause, seek,
state and an actual runtime key-system probe. A successful CDM probe establishes
capability only: account entitlement, device robustness and an authorized license
are still required for the particular track.

**WebView2 must not be assumed to include Widevine.** Microsoft tracks that
capability in [WebView2Feedback #4828](https://github.com/MicrosoftEdge/WebView2Feedback/issues/4828).
Call `ProbeKeySystemsAsync` on the loaded official origin. If the required system
is absent, use a supported full browser and the descriptor's `official_watch_url`.
`DrmRouteAsync(playerDescriptor.GetProperty("drm"))` returns `official_webview` or
`full_browser_required` and always keeps `license_verified:false`: CDM capability
cannot establish the account's rights to an individual track.
Opening a full browser transfers playback ownership to that browser; it is not
native MediaPlayer playback or a promise of SMTC integration.

```csharp
// Run on the WinUI UI thread, using an initialized WebView2 control.
await webView.EnsureCoreWebView2Async();
var player = new OfficialWebPlayer(webView.CoreWebView2);

// Optional: import an explicitly exported BrowserSession into a dedicated profile.
// await player.ImportMusicSessionAsync(sessionJson);
// Never log the snapshot. Import requires only Music cookies, not Google credentials.

await player.LoadAsync(videoId, cancellationToken);
var cdmAvailability = await player.ProbeKeySystemsAsync();
var playback = await player.PlayAsync();
// The official page may require a click for autoplay, login or consent.
var state = await player.StateAsync();
await player.SeekAsync(30);
await player.PauseAsync();

// Reuse this same logged-in official page for proof-of-origin attestation.
// The helper checks signing cookies and selected account before and after minting,
// then installs the PLAYER/GVS token pair into this core session. No token is returned.
var refreshed = await player.RefreshPoTokensAsync(coreClient, videoId, cancellationToken);
```

Create the WebView with a dedicated persistent user-data folder protected by the
current Windows user. The folder contains browser-managed credentials; it is not
the core's session vault. Cookie import is host-only for `music.youtube.com`, uses
secure cookies, retains known expiry, rejects duplicate names and never widens
credentials to `.google.com` or `.youtube.com`. Header snapshots do not preserve
every browser cookie attribute. The helper rejects nonzero `auth_user`, delegated
accounts and duplicate cookies; select/sign into those accounts in the official
page instead. Do not equate a successful import with verified browser identity.
Core session maintenance
and browser session maintenance are independent.

Keep the official page visible for account/consent decisions. The host owns its
navigation policy, new-window handling, lifetime and profile cleanup. Dispose
the control to close playback. The helper must run on the UI thread and does not
own or dispose the supplied control. Its state result contains no signed URLs,
license challenges or cookie values. `encrypted` means a MediaKeys object is
attached, not that this particular frame was decrypted successfully.

Build independently of the portable wrapper:

```powershell
dotnet build examples/dotnet/web-player/YouTubeMusic.Interop.WebPlayer.csproj -c Release
```

`Microsoft.Web.WebView2` is an optional host dependency. The native Rust core and
portable `.NET` wrapper retain their browser-independent clear-audio path.

The default build uses the WinRT `CoreWebView2` projection required by WinUI 3.
For a WPF/WinForms host using the managed WebView2 SDK, build this project with
`-p:WebView2EnableCsWinRTProjection=false`; those two types are not interchangeable.
Proof-of-origin runs the core's official-page provider; cancellation while Google's
browser VM is active waits for that provider's bounded cleanup (up to 25 seconds),
and never installs a canceled result. Browser permission, login and consent remain
the official page's responsibility.

Validation on Windows with WebView2 153.0.4234.32 included both the official Music
page (clear track, real-time advancement, seeking and pause) and the publicly
licensed Shaka `angel-one-widevine` test vector. The latter obtained two license
responses, reported usable keys, decoded the encrypted 60-second video in real
time, sought, paused and reached its natural end. The test vector and demo license
server are **test-only**, not product dependencies. No protected YouTube Music track
was supplied for an account-specific license test; this evidence does not assert
that every Music entitlement or device robustness level is supported.
