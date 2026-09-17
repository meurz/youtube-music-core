# .NET / WinUI 3 interop example

This .NET 8 library builds with `dotnet build examples/dotnet`. Reference the project
from a WinUI 3 application and place the matching native `youtube_music_core.dll`
next to the application's executable (Windows x64 release). There are no NuGet
runtime dependencies in this wrapper.

```csharp
using YouTubeMusic.Interop;

// Load decrypted BrowserSession JSON from your own secure credential store.
// Anonymous clients can instead use "{}".
await using var music = await MusicCoreClient.CreateAsync(savedSessionJson);
var songs = await music.CallAsync("""{"op":"library","section":"songs"}""");

// Save rotations observed during requests, on a host-chosen schedule.
await music.ExportSessionToAsync(SaveEncryptedSessionAsync);
// Or explicitly refresh, verify, and save in one serialized operation:
var refresh = await music.RefreshAndPersistAsync(SaveEncryptedSessionAsync);
```

The host implements `Task SaveEncryptedSessionAsync(ReadOnlyMemory<byte> session)`.
It receives **secret UTF-8 BrowserSession JSON**, not the outer result envelope.
Encrypt it with Windows DPAPI or equivalent secure storage, atomically replace the
stored file, and complete the Task only after persistence succeeds. The buffer is
cleared after the callback completes. Avoid conversion to immutable strings where
possible; .NET/native marshaling and JSON code may still create temporary copies,
so buffer clearing is best effort, not a guarantee that every copy was erased.
Config JSON and playback URLs must not be logged.

Retain one client per signed-in account for HTTP connection reuse and evolving
session state. For multiple processes, coordinate writes or compare-and-swap the
saved snapshot; this minimal example serializes only within its own instance.
Use `ExportSessionToAsync` after successful authenticated operations to persist
response Cookie changes. An export verifies the current session; failures propagate
without calling your storage callback. The host chooses refresh frequency and
must prompt for official browser login/import after `authentication_rejected`.
No background timer or automatic re-login is installed by this wrapper.

Native operations run off the UI thread. A `CancellationToken` can cancel waiting
for a queued call but **cannot abort a native request already running**. Set
`timeout_seconds` in the configuration for individual HTTP timeouts; playback JS
has its own time limits. Dispose waits for any active operation. Binding results to
WinUI controls remains the application's responsibility; use `DispatcherQueue`
where required. Playback, SMTC, playlists writes and a complete WinUI app are outside
this example.
