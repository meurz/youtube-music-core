# .NET / WinUI 3 interop example

This .NET 8 library builds with `dotnet build examples/dotnet`. Reference the project
from a WinUI 3 application and place the matching native `youtube_music_core.dll`
next to the application's executable. Use the native DLL for the application's
architecture and ABI 2 or newer; this wrapper requires the operation-control
exports introduced in core 0.7.0. It has no NuGet runtime dependencies.

```csharp
using YouTubeMusic.Interop;

// Load decrypted BrowserSession JSON from your own secure credential store.
// Anonymous clients can instead use "{}". Never log this configuration.
await using var music = await MusicCoreClient.CreateAsync(savedSessionJson,
    cancellation: cancellationToken, timeout: TimeSpan.FromSeconds(30));
var capabilities = await music.CallAsync("""{"op":"capabilities"}""");
var songs = await music.CallAsync("""{"op":"library","section":"songs"}""",
    cancellation: cancellationToken);

// Save rotations observed during requests, on a host-chosen schedule.
await music.ExportSessionToAsync(SaveEncryptedSessionAsync, cancellationToken);
// Or explicitly refresh, verify, and save in one serialized operation:
var refresh = await music.RefreshAndPersistAsync(SaveEncryptedSessionAsync,
    cancellationToken, timeout: TimeSpan.FromSeconds(30));
```

The `capabilities` result reports the protocol/ABI/core versions, supported
operations, features and limits. It is local and does not contact Google. Use it
to decide which client controls to expose; see [the JSON protocol](../../docs/protocol.md)
for requests and [the host guide](../../docs/winui-host.md) for ownership boundaries.

## Cancellation, deadlines and progress

Native operations run off the UI thread and serialize per client instance.
`CancellationToken` cancels both queued calls and an active native HTTP request or
JavaScript transform. Cancellation raises `OperationCanceledException`; the
wrapper waits for the native worker to terminate before releasing its gate or
returning the exception. No detached native request continues afterward.

The optional `timeout` is a total native-operation budget, including time waiting
for the instance gate. It defaults to 120 seconds and accepts 1–600000 milliseconds.
Queued and native timeouts both raise `MusicCoreException` with `Code == "timeout"`.
The config's `timeout_seconds` still limits individual HTTP requests. Native errors
also expose `Retryable`, `HttpStatus` and `RetryAfterSeconds`; account rejection,
rate limiting and validation errors can therefore have different UI actions.

```csharp
// Construct Progress<T> on the UI thread if its synchronization context should
// receive callbacks; otherwise dispatch UI changes with DispatcherQueue.
var progress = new Progress<MusicProgress>(p => ShowPhase(p.Phase));
using var trackChange = new CancellationTokenSource();
var stream = await music.CallAsync(
    """{"op":"stream","video_id":"4D7u5KF7SP8","format":"mp4"}""",
    cancellation: trackChange.Token, progress: progress,
    timeout: TimeSpan.FromSeconds(60));
// On a subsequent track change, cancel the previous operation's source.
```

Progress is a phase/state/elapsed-time snapshot, not a percentage or downloaded
media position. Reports are polled approximately every 100 milliseconds during a
call. Keep observers quick and avoid throwing. If a synchronous observer throws,
the wrapper cancels and waits for its native worker, then propagates the observer
exception. Exceptions raised later by a UI synchronization context belong to that
context's error handling. `DisposeAsync` waits for the active call; cancel the
call's token first when closing or switching accounts promptly.

## Playback preparation and URL lifetime

Retain one client per selected account to reuse HTTP connections, current Cookies,
prepared player code and recently verified URLs. Schedule preparation before the
first play action, and prefetch only the host's next one to three tracks:

```csharp
await music.CallAsync("""{"op":"prewarm"}""", cancellationToken);
await music.CallAsync(
    """{"op":"prefetch","video_ids":["4D7u5KF7SP8"],"format":"mp4"}""",
    cancellationToken);
var stream = await music.CallAsync(
    """{"op":"stream","video_id":"4D7u5KF7SP8","format":"mp4"}""",
    cancellationToken);
// If the media host rejects an old URL, force a fresh player response:
var replacement = await music.CallAsync(
    """{"op":"stream_refresh","video_id":"4D7u5KF7SP8","format":"mp4"}""",
    cancellationToken);
// Explicitly discard this client's player and URL caches if needed:
await music.CallAsync("""{"op":"playback_reset"}""", cancellationToken);
```

`prewarm` actually preprocesses the player; a cold preparation can still take tens
of seconds. The core retains at most eight verified streams for at most five
minutes and only reuses URLs with at least 90 seconds before expiry. This does not
guarantee a URL will remain valid for a whole long track. Respect `expires_at`,
re-resolve on recoverable playback failures, and avoid indefinitely retrying the
same rejected URL. Never log signed URLs. The WinUI host owns `MediaPlayer`,
buffering/seek, playback position, queue policy and SMTC/media keys.

## Account selection and secure persistence

Read `{"op":"accounts"}`, display the returned choices, and serialize the chosen
entry's non-null `selector` into `SelectAccountAsync(selectorJson, cancellationToken)`.
A missing selector means the Web response did not provide a reusable selection;
import that account from the browser instead.
Selection verifies and returns a **separate** client; it leaves the original
client unchanged. Dispose the old client when appropriate, and persist the selected
client's verified session under its own account/profile key. Do not guess account
indices or delegated channel IDs.

The host implements `Task SaveEncryptedSessionAsync(ReadOnlyMemory<byte> session)`.
It receives **secret UTF-8 BrowserSession JSON**, not the outer result envelope.
Encrypt it with Windows DPAPI or equivalent secure storage, atomically replace the
stored file, and complete the Task only after persistence succeeds. The buffer is
cleared after the callback completes. Do not retain the memory beyond the callback
or send it to logs or telemetry. Avoid conversion to immutable strings where
possible; .NET/native marshaling and JSON code may create temporary copies, so
buffer clearing cannot guarantee that every copy was erased.

Refresh/export timeouts include their gate wait and native work. The host storage
callback owns its own cancellation, timeout and atomicity; the wrapper awaits it
before clearing its buffer, even if the original token is cancelled meanwhile.
For multiple processes, coordinate writes or compare-and-swap the saved snapshot;
this wrapper serializes only within its own instance.

Use `ExportSessionToAsync` after successful authenticated operations to persist
response Cookie changes. An export verifies the current session; verification
failure propagates without calling storage. Anonymous export returns `false`.
The host chooses refresh frequency and prompts for official browser login/import
after `authentication_rejected`. There is no background timer or automatic login.

The [offline smoke executable](../dotnet-smoke/README.md) exercises actual native
interop through a local proxy sink, including cancellation, deadlines, progress
observer failure, handle cleanup and disposal. It requires no account or external
network access.
