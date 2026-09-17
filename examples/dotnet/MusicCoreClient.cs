using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text.Json;

namespace YouTubeMusic.Interop;

public sealed record MusicProgress(string Phase, string State, long ElapsedMilliseconds);

/// <summary>Persistent native Music client with actual I/O/JS cancellation and total deadlines.</summary>
/// <remarks>Use one instance per selected account. Keep the native library loaded until disposal.</remarks>
public sealed class MusicCoreClient : IAsyncDisposable
{
    private readonly SemaphoreSlim gate = new(1, 1);
    private ulong handle;
    private MusicCoreClient(ulong handle) => this.handle = handle;

    /// <summary>Config and saved session JSON contain secrets; never log them.</summary>
    public static async Task<MusicCoreClient> CreateAsync(string configJson,
        CancellationToken cancellation = default, IProgress<MusicProgress>? progress = null,
        TimeSpan? timeout = null)
    {
        return await RunNativeAsync(id => new MusicCoreClient(ReadResult(Native.Create(configJson, id)).GetProperty("handle").GetUInt64()),
            cancellation, progress, timeout, null, client => client.DestroyUnpublished()).ConfigureAwait(false);
    }

    public Task<JsonElement> CallAsync(string requestJson, CancellationToken cancellation = default,
        IProgress<MusicProgress>? progress = null, TimeSpan? timeout = null) =>
        RunNativeAsync(id => { ThrowIfDisposed(); return ReadResult(Native.Call(handle, requestJson, id)); },
            cancellation, progress, timeout, gate);

    /// <summary>Return a separately verified client for an account from the accounts operation.</summary>
    public Task<MusicCoreClient> SelectAccountAsync(string selectorJson, CancellationToken cancellation = default,
        TimeSpan? timeout = null) => RunNativeAsync(id =>
        {
            ThrowIfDisposed();
            return new MusicCoreClient(ReadResult(Native.SelectAccount(handle, selectorJson, id)).GetProperty("handle").GetUInt64());
        }, cancellation, null, timeout, gate, client => client.DestroyUnpublished());

    /// <summary>Refresh and persist while serializing against other operations on this instance.</summary>
    public async Task<JsonElement> RefreshAndPersistAsync(Func<ReadOnlyMemory<byte>, Task> saveSession,
        CancellationToken cancellation = default, TimeSpan? timeout = null)
    {
        ArgumentNullException.ThrowIfNull(saveSession);
        TimeSpan budget = ValidateTimeout(timeout);
        var watch = System.Diagnostics.Stopwatch.StartNew();
        if (!await gate.WaitAsync(budget, cancellation).ConfigureAwait(false)) throw new MusicCoreException("timeout", "Music operation timed out", retryable: true);
        try
        {
            ThrowIfDisposed();
            JsonElement metadata = await RunNativeAsync(id => ReadResult(Native.Call(handle, "{\"op\":\"auth_refresh\"}", id)),
                cancellation, null, Remaining(budget, watch), null).ConfigureAwait(false);
            await ExportLockedAsync(saveSession, cancellation, Remaining(budget, watch)).ConfigureAwait(false);
            return metadata;
        }
        finally { gate.Release(); }
    }

    /// <summary>Only this explicit export callback receives secret session JSON. Encrypt before saving.</summary>
    /// <remarks>Memory is valid until the callback completes. Do not retain it or log it.</remarks>
    public async Task<bool> ExportSessionToAsync(Func<ReadOnlyMemory<byte>, Task> saveSession,
        CancellationToken cancellation = default, TimeSpan? timeout = null)
    {
        ArgumentNullException.ThrowIfNull(saveSession);
        TimeSpan budget = ValidateTimeout(timeout);
        var watch = System.Diagnostics.Stopwatch.StartNew();
        if (!await gate.WaitAsync(budget, cancellation).ConfigureAwait(false)) throw new MusicCoreException("timeout", "Music operation timed out", retryable: true);
        try { ThrowIfDisposed(); return await ExportLockedAsync(saveSession, cancellation, Remaining(budget, watch)).ConfigureAwait(false); }
        finally { gate.Release(); }
    }

    private async Task<bool> ExportLockedAsync(Func<ReadOnlyMemory<byte>, Task> saveSession,
        CancellationToken cancellation, TimeSpan timeout)
    {
        byte[] result = await RunNativeAsync(id => CopyAndFree(Native.Export(handle, id)), cancellation, null, timeout, null).ConfigureAwait(false);
        byte[]? session = null;
        try
        {
            using JsonDocument document = JsonDocument.Parse(result);
            JsonElement data = GetData(document.RootElement);
            if (data.ValueKind == JsonValueKind.Null) return false;
            session = JsonSerializer.SerializeToUtf8Bytes(data);
            cancellation.ThrowIfCancellationRequested();
            // Host storage work owns its own cancellation/atomicity. Await it before clearing memory.
            await saveSession(session).ConfigureAwait(false);
            return true;
        }
        finally
        {
            CryptographicOperations.ZeroMemory(result);
            if (session is not null) CryptographicOperations.ZeroMemory(session);
        }
    }

    private static async Task<T> RunNativeAsync<T>(Func<ulong, T> call, CancellationToken cancellation,
        IProgress<MusicProgress>? progress, TimeSpan? timeout, SemaphoreSlim? gate, Action<T>? abandon = null)
    {
        TimeSpan budget = ValidateTimeout(timeout);
        cancellation.ThrowIfCancellationRequested();
        ulong operation = ReadResult(Native.OperationCreate(JsonSerializer.Serialize(new { timeout_ms = (long)budget.TotalMilliseconds })))
            .GetProperty("handle").GetUInt64();
        bool entered = false;
        try
        {
            using var registration = cancellation.Register(() => Native.Free(Native.OperationCancel(operation)));
            if (gate is not null)
            {
                if (!await gate.WaitAsync(budget, cancellation).ConfigureAwait(false)) throw new MusicCoreException("timeout", "Music operation timed out", retryable: true);
                entered = true;
            }
            Task<T> work = Task.Run(() => call(operation));
            try
            {
                while (!work.IsCompleted && progress is not null)
                {
                    JsonElement state = ReadResult(Native.OperationStatus(operation));
                    progress.Report(new MusicProgress(state.GetProperty("phase").GetString()!, state.GetProperty("state").GetString()!, state.GetProperty("elapsed_ms").GetInt64()));
                    await Task.WhenAny(work, Task.Delay(100)).ConfigureAwait(false);
                }
                // Never abandon the native worker: cancellation interrupts it before this completes.
                return await work.ConfigureAwait(false);
            }
            catch (Exception error)
            {
                Native.Free(Native.OperationCancel(operation));
                // Report() may throw exactly as a successful create returns. Observe all
                // completions and release unpublished handles before releasing the gate.
                try { T result = await work.ConfigureAwait(false); abandon?.Invoke(result); } catch { }
                if (error is MusicCoreException { Code: "cancelled" })
                    throw new OperationCanceledException("Music operation cancelled", error, cancellation);
                throw;
            }
        }
        finally
        {
            Native.Free(Native.OperationDestroy(operation));
            if (entered) gate!.Release();
        }
    }

    private static TimeSpan ValidateTimeout(TimeSpan? timeout)
    {
        TimeSpan value = timeout ?? TimeSpan.FromSeconds(120);
        if (value.TotalMilliseconds < 1 || value.TotalMilliseconds > 600000) throw new ArgumentOutOfRangeException(nameof(timeout));
        return value;
    }
    private static TimeSpan Remaining(TimeSpan budget, System.Diagnostics.Stopwatch watch)
    {
        TimeSpan value = budget - watch.Elapsed;
        if (value.TotalMilliseconds < 1) throw new MusicCoreException("timeout", "Music operation timed out", retryable: true);
        return value;
    }

    private void DestroyUnpublished()
    {
        ulong previous = handle;
        handle = 0;
        if (previous != 0) ReadResult(Native.Destroy(previous));
        GC.SuppressFinalize(this);
    }

    public async ValueTask DisposeAsync()
    {
        await gate.WaitAsync().ConfigureAwait(false);
        try
        {
            ulong previous = handle; handle = 0;
            if (previous != 0) await Task.Run(() => ReadResult(Native.Destroy(previous))).ConfigureAwait(false);
            GC.SuppressFinalize(this);
        }
        finally { gate.Release(); }
    }
    ~MusicCoreClient()
    {
        if (handle == 0) return;
        try { Native.Free(Native.Destroy(handle)); } catch { }
    }
    private void ThrowIfDisposed() => ObjectDisposedException.ThrowIf(handle == 0, this);
    private static JsonElement ReadResult(nint pointer)
    {
        byte[] bytes = CopyAndFree(pointer);
        try { using JsonDocument document = JsonDocument.Parse(bytes); return GetData(document.RootElement).Clone(); }
        finally { CryptographicOperations.ZeroMemory(bytes); }
    }
    private static JsonElement GetData(JsonElement root)
    {
        if (!root.GetProperty("ok").GetBoolean())
        {
            JsonElement error = root.GetProperty("error");
            throw new MusicCoreException(error.GetProperty("code").GetString() ?? "unknown", error.GetProperty("message").GetString() ?? "Native request failed",
                error.TryGetProperty("retryable", out var retry) && retry.GetBoolean(),
                error.TryGetProperty("http_status", out var status) ? status.GetInt32() : null,
                error.TryGetProperty("retry_after_seconds", out var delay) ? delay.GetInt64() : null);
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
            byte[] bytes = new byte[length]; Marshal.Copy(pointer, bytes, 0, length); return bytes;
        }
        finally { Native.Free(pointer); }
    }
    private static class Native
    {
        private const string Library = "youtube_music_core";
        [DllImport(Library, EntryPoint = "ytmusic_client_create_with_operation", CallingConvention = CallingConvention.Cdecl)]
        internal static extern nint Create([MarshalAs(UnmanagedType.LPUTF8Str)] string config, ulong operation);
        [DllImport(Library, EntryPoint = "ytmusic_client_call_with_operation", CallingConvention = CallingConvention.Cdecl)]
        internal static extern nint Call(ulong handle, [MarshalAs(UnmanagedType.LPUTF8Str)] string request, ulong operation);
        [DllImport(Library, EntryPoint = "ytmusic_client_select_account", CallingConvention = CallingConvention.Cdecl)]
        internal static extern nint SelectAccount(ulong handle, [MarshalAs(UnmanagedType.LPUTF8Str)] string selector, ulong operation);
        [DllImport(Library, EntryPoint = "ytmusic_client_export_session_with_operation", CallingConvention = CallingConvention.Cdecl)]
        internal static extern nint Export(ulong handle, ulong operation);
        [DllImport(Library, EntryPoint = "ytmusic_client_destroy", CallingConvention = CallingConvention.Cdecl)]
        internal static extern nint Destroy(ulong handle);
        [DllImport(Library, EntryPoint = "ytmusic_operation_create", CallingConvention = CallingConvention.Cdecl)]
        internal static extern nint OperationCreate([MarshalAs(UnmanagedType.LPUTF8Str)] string options);
        [DllImport(Library, EntryPoint = "ytmusic_operation_cancel", CallingConvention = CallingConvention.Cdecl)]
        internal static extern nint OperationCancel(ulong handle);
        [DllImport(Library, EntryPoint = "ytmusic_operation_status", CallingConvention = CallingConvention.Cdecl)]
        internal static extern nint OperationStatus(ulong handle);
        [DllImport(Library, EntryPoint = "ytmusic_operation_destroy", CallingConvention = CallingConvention.Cdecl)]
        internal static extern nint OperationDestroy(ulong handle);
        [DllImport(Library, EntryPoint = "ytmusic_string_free", CallingConvention = CallingConvention.Cdecl)]
        internal static extern void Free(nint pointer);
    }
}

public sealed class MusicCoreException(string code, string message, bool retryable = false, int? httpStatus = null, long? retryAfterSeconds = null) : Exception(message)
{
    public string Code { get; } = code;
    public bool Retryable { get; } = retryable;
    public int? HttpStatus { get; } = httpStatus;
    public long? RetryAfterSeconds { get; } = retryAfterSeconds;
}
