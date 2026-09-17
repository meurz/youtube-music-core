using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text.Json;

namespace YouTubeMusic.Interop;

/// <summary>A persistent blocking-native client wrapped for WinUI 3 / .NET hosts.</summary>
/// <remarks>
/// Native calls run on the thread pool and are serialized per instance. Cancellation
/// only cancels waiting for the gate; once started, a native request runs to completion.
/// Configure timeout_seconds to bound individual HTTP requests. DisposeAsync waits for
/// an active operation. Keep the native DLL loaded throughout this object's lifetime.
/// </remarks>
public sealed class MusicCoreClient : IAsyncDisposable
{
    private readonly SemaphoreSlim gate = new(1, 1);
    private ulong handle;

    private MusicCoreClient(ulong handle) => this.handle = handle;

    /// <param name="configJson">
    /// A Config object. Saved BrowserSession fields (cookie, auth_user,
    /// delegated_session_id, cookie_expirations) can be used directly as configuration.
    /// Credential strings are sensitive: never log configJson or native input/output.
    /// </param>
    public static async Task<MusicCoreClient> CreateAsync(string configJson)
    {
        return await Task.Run(() =>
        {
            JsonElement data = ReadResult(Native.Create(configJson));
            return new MusicCoreClient(data.GetProperty("handle").GetUInt64());
        }).ConfigureAwait(false);
    }

    /// <summary>Execute a Request object such as {"op":"library","section":"songs"}.</summary>
    public async Task<JsonElement> CallAsync(string requestJson, CancellationToken cancellation = default)
    {
        await gate.WaitAsync(cancellation).ConfigureAwait(false);
        try
        {
            ThrowIfDisposed();
            return await Task.Run(() => ReadResult(Native.Call(handle, requestJson))).ConfigureAwait(false);
        }
        finally { gate.Release(); }
    }

    /// <summary>Refresh the current Web session and securely persist its verified snapshot.</summary>
    /// <remarks>Returned metadata contains no Cookies; the save callback alone receives secrets.</remarks>
    public async Task<JsonElement> RefreshAndPersistAsync(
        Func<ReadOnlyMemory<byte>, Task> saveSession,
        CancellationToken cancellation = default)
    {
        ArgumentNullException.ThrowIfNull(saveSession);
        await gate.WaitAsync(cancellation).ConfigureAwait(false);
        try
        {
            ThrowIfDisposed();
            JsonElement metadata = await Task.Run(() =>
                ReadResult(Native.Call(handle, "{\"op\":\"auth_refresh\"}"))).ConfigureAwait(false);
            await ExportLockedAsync(saveSession).ConfigureAwait(false);
            return metadata;
        }
        finally { gate.Release(); }
    }

    /// <summary>Explicit secret export, for saving rotations observed during normal requests.</summary>
    /// <param name="saveSession">
    /// Receives UTF-8 BrowserSession JSON, valid only until its returned Task completes.
    /// Encrypt and persist with DPAPI or another secure store. Do not retain this memory,
    /// log it, display it, or forward it to telemetry. Storage failure propagates to caller.
    /// </param>
    /// <returns>False for an anonymous client, otherwise true after the save completes.</returns>
    public async Task<bool> ExportSessionToAsync(
        Func<ReadOnlyMemory<byte>, Task> saveSession,
        CancellationToken cancellation = default)
    {
        ArgumentNullException.ThrowIfNull(saveSession);
        await gate.WaitAsync(cancellation).ConfigureAwait(false);
        try
        {
            ThrowIfDisposed();
            return await ExportLockedAsync(saveSession).ConfigureAwait(false);
        }
        finally { gate.Release(); }
    }

    private async Task<bool> ExportLockedAsync(Func<ReadOnlyMemory<byte>, Task> saveSession)
    {
        byte[] result = await Task.Run(() => CopyAndFree(Native.Export(handle))).ConfigureAwait(false);
        byte[]? session = null;
        try
        {
            using JsonDocument document = JsonDocument.Parse(result);
            JsonElement data = GetData(document.RootElement);
            if (data.ValueKind == JsonValueKind.Null) return false;
            session = JsonSerializer.SerializeToUtf8Bytes(data);
            await saveSession(session).ConfigureAwait(false);
            return true;
        }
        finally
        {
            CryptographicOperations.ZeroMemory(result);
            if (session is not null) CryptographicOperations.ZeroMemory(session);
        }
    }

    public async ValueTask DisposeAsync()
    {
        await gate.WaitAsync().ConfigureAwait(false);
        try
        {
            ulong previous = handle;
            handle = 0;
            if (previous != 0)
                await Task.Run(() => ReadResult(Native.Destroy(previous))).ConfigureAwait(false);
            GC.SuppressFinalize(this);
        }
        finally { gate.Release(); }
    }

    ~MusicCoreClient()
    {
        // A fallback for a forgotten DisposeAsync; explicit async disposal is preferred.
        // There are no outstanding operations when this object becomes unreachable.
        if (handle == 0) return;
        try { Native.Free(Native.Destroy(handle)); }
        catch { /* A finalizer must not throw during application shutdown. */ }
    }

    private void ThrowIfDisposed() => ObjectDisposedException.ThrowIf(handle == 0, this);

    private static JsonElement ReadResult(nint pointer)
    {
        byte[] bytes = CopyAndFree(pointer);
        try
        {
            using JsonDocument document = JsonDocument.Parse(bytes);
            return GetData(document.RootElement).Clone();
        }
        finally { CryptographicOperations.ZeroMemory(bytes); }
    }

    private static JsonElement GetData(JsonElement root)
    {
        if (!root.GetProperty("ok").GetBoolean())
        {
            JsonElement error = root.GetProperty("error");
            throw new MusicCoreException(
                error.GetProperty("code").GetString() ?? "unknown",
                error.GetProperty("message").GetString() ?? "Native request failed");
        }
        return root.GetProperty("data");
    }

    private static byte[] CopyAndFree(nint pointer)
    {
        if (pointer == 0) throw new InvalidOperationException("Native library returned no result");
        try
        {
            int length = 0;
            while (Marshal.ReadByte(pointer, length) != 0) length = checked(length + 1);
            byte[] bytes = new byte[length];
            Marshal.Copy(pointer, bytes, 0, length);
            return bytes;
        }
        finally { Native.Free(pointer); }
    }

    private static class Native
    {
        private const string Library = "youtube_music_core";
        [DllImport(Library, EntryPoint = "ytmusic_client_create", CallingConvention = CallingConvention.Cdecl)]
        internal static extern nint Create([MarshalAs(UnmanagedType.LPUTF8Str)] string config);
        [DllImport(Library, EntryPoint = "ytmusic_client_call", CallingConvention = CallingConvention.Cdecl)]
        internal static extern nint Call(ulong handle, [MarshalAs(UnmanagedType.LPUTF8Str)] string request);
        [DllImport(Library, EntryPoint = "ytmusic_client_export_session", CallingConvention = CallingConvention.Cdecl)]
        internal static extern nint Export(ulong handle);
        [DllImport(Library, EntryPoint = "ytmusic_client_destroy", CallingConvention = CallingConvention.Cdecl)]
        internal static extern nint Destroy(ulong handle);
        [DllImport(Library, EntryPoint = "ytmusic_string_free", CallingConvention = CallingConvention.Cdecl)]
        internal static extern void Free(nint output);
    }
}

public sealed class MusicCoreException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}
