# Offline native interop checks

This executable tests the actual native library through the .NET wrapper. It uses
a local HTTP CONNECT sink to stall requests deterministically; it never contacts
Google or loads account credentials.

```sh
cargo build --release --locked
dotnet build examples/dotnet-smoke -c Release
LD_LIBRARY_PATH="$PWD/target/release" dotnet examples/dotnet-smoke/bin/Release/net8.0/InteropSmoke.dll
```

On Windows, copy `target/release/youtube_music_core.dll` beside `InteropSmoke.dll`
and run `dotnet examples/dotnet-smoke/bin/Release/net8.0/InteropSmoke.dll`.

Checks include cancellation during create and HTTP, queued cancellation and total
deadlines, progress notification, throwing-observer cleanup, native worker/socket
termination, disposal during a call, repeated disposal, and client handle capacity
following abandoned creation. Any failed check exits nonzero.
