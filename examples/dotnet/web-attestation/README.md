# Official browser attestation

This optional Windows helper loads the real `https://music.youtube.com/` home
page to obtain proof-of-origin tokens for the Rust core. It does not navigate to
a watch page or start media playback.

```csharp
// Run on the WinUI UI thread, using an initialized WebView2 control.
await webView.EnsureCoreWebView2Async();
var attestation = new OfficialBrowserAttestation(webView.CoreWebView2);

// Optional: import an explicitly exported BrowserSession into a dedicated profile.
// await attestation.ImportMusicSessionAsync(sessionJson);
// Never log the snapshot.

await attestation.LoadAsync(cancellationToken);
var refreshed = await attestation.RefreshPoTokensAsync(coreClient, videoId, cancellationToken);
```

`RefreshPoTokensAsync` checks the selected account and signing cookies before and
after minting, then installs the PLAYER/GVS token pair into the same core session.
Its result contains only the video ID, expiry and success flag. Tokens and session
bindings are not returned. Cancellation during Google's browser VM phase waits
for bounded cleanup (up to 25 seconds) and never installs a canceled result.

Create the WebView with a dedicated persistent user-data folder protected by the
current Windows account. Cookie import is host-only for `music.youtube.com`, uses
secure cookies, retains known expiry and rejects duplicate names. Header snapshots
do not preserve every browser cookie attribute. Import supports only the primary
non-delegated account; select other accounts in the official page. Successful
import alone does not establish that the browser and core use the same identity.
Core and browser session maintenance remain independent.

Keep the official page visible for login and consent. The host owns navigation
policy, new-window handling, lifetime and profile cleanup. This helper must run
on the UI thread and does not own or dispose the supplied control.

```powershell
dotnet build examples/dotnet/web-attestation/YouTubeMusic.Interop.WebAttestation.csproj -c Release
```

The default build uses the WinRT `CoreWebView2` projection required by WinUI 3.
For WPF/WinForms hosts using the managed WebView2 SDK, add
`-p:WebView2EnableCsWinRTProjection=false`. These projections are not interchangeable.
The Rust core and portable .NET wrapper do not depend on WebView2.

Migration: replace `OfficialWebPlayer` with `OfficialBrowserAttestation`, reference
`YouTubeMusic.Interop.WebAttestation.csproj`, and call `LoadAsync(cancellationToken)`
without a video ID. Pass the video ID only to `RefreshPoTokensAsync`.
