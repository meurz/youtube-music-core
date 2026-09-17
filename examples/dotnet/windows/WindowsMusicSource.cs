using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Windows.Media.Core;
using Windows.Media.Streaming.Adaptive;
using Windows.Storage.Streams;

namespace YouTubeMusic.Interop;

/// <summary>Owns a fresh Windows adaptive source for one playback instance.</summary>
/// <remarks>Detach Source from MediaPlayer before disposal. Never log the manifest or signed URLs.</remarks>
public sealed class WindowsMusicSource : IDisposable
{
    private MediaSource? source;
    private readonly AdaptiveMediaSource adaptive;
    private readonly Windows.Web.Http.HttpClient http;
    private readonly InMemoryRandomAccessStream manifestStream;
    public MediaSource Source => source ?? throw new ObjectDisposedException(nameof(WindowsMusicSource));

    private WindowsMusicSource(MediaSource source, AdaptiveMediaSource adaptive,
        Windows.Web.Http.HttpClient http, InMemoryRandomAccessStream manifestStream)
    {
        this.source = source;
        this.adaptive = adaptive;
        this.http = http;
        this.manifestStream = manifestStream;
    }

    /// <summary>Resolve official Web AAC and initialize Windows' DASH pipeline.</summary>
    public static async Task<WindowsMusicSource> CreateAsync(MusicCoreClient client, string videoId,
        CancellationToken cancellation = default, IProgress<MusicProgress>? progress = null,
        TimeSpan? timeout = null)
    {
        ArgumentNullException.ThrowIfNull(client);
        TimeSpan budget = ValidateTimeout(timeout);
        var watch = Stopwatch.StartNew();
        JsonElement descriptor = await client.CallAsync(
            JsonSerializer.Serialize(new { op = "dash_manifest", video_id = videoId }),
            cancellation, progress, budget).ConfigureAwait(false);
        TimeSpan remaining = budget - watch.Elapsed;
        if (remaining.TotalMilliseconds < 1) throw Timeout();
        return await FromManifestAsync(descriptor, cancellation, remaining).ConfigureAwait(false);
    }

    /// <summary>Create from a core dash_manifest result, including its public media headers.</summary>
    /// <remarks>Every invocation creates a new AdaptiveMediaSource; do not reuse it across replacements.</remarks>
    public static async Task<WindowsMusicSource> FromManifestAsync(JsonElement descriptor,
        CancellationToken cancellation = default, TimeSpan? timeout = null)
    {
        TimeSpan budget = ValidateTimeout(timeout);
        cancellation.ThrowIfCancellationRequested();
        if (!AdaptiveMediaSource.IsContentTypeSupported("application/dash+xml"))
            throw new NotSupportedException("Windows DASH playback is unavailable");
        if (descriptor.GetProperty("mime_type").GetString() != "application/dash+xml")
            throw new ArgumentException("Expected a core DASH manifest result", nameof(descriptor));
        if (descriptor.TryGetProperty("expires_at", out JsonElement expires)
            && expires.ValueKind == JsonValueKind.Number
            && expires.GetInt64() <= DateTimeOffset.UtcNow.ToUnixTimeSeconds() + 30)
            throw new MusicCoreException("stream_expired", "Resolve a fresh DASH manifest before playback", retryable: true);
        string manifest = descriptor.GetProperty("manifest").GetString()
            ?? throw new ArgumentException("Missing DASH manifest", nameof(descriptor));
        if (manifest.Length > 1024 * 1024)
            throw new ArgumentException("DASH manifest exceeds size limit", nameof(descriptor));

        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        deadline.CancelAfter(budget);
        var memory = new InMemoryRandomAccessStream();
        var http = new Windows.Web.Http.HttpClient();
        AdaptiveMediaSource? adaptive = null;
        MediaSource? source = null;
        byte[] bytes = Encoding.UTF8.GetBytes(manifest);
        try
        {
            foreach (JsonProperty header in descriptor.GetProperty("http_headers").EnumerateObject())
            {
                // Account authentication must never enter the Windows/CDN pipeline.
                bool publicHeader = header.Name.Equals("User-Agent", StringComparison.OrdinalIgnoreCase)
                    || header.Name.Equals("Accept", StringComparison.OrdinalIgnoreCase)
                    || header.Name.Equals("Accept-Language", StringComparison.OrdinalIgnoreCase);
                if (!publicHeader || !http.DefaultRequestHeaders.TryAppendWithoutValidation(header.Name, header.Value.GetString()))
                    throw new ArgumentException("Invalid public media header", nameof(descriptor));
            }
            using (var writer = new DataWriter(memory.GetOutputStreamAt(0)))
            {
                writer.WriteBytes(bytes);
                await writer.StoreAsync().AsTask(deadline.Token).ConfigureAwait(false);
                writer.DetachStream();
            }
            memory.Seek(0);
            var result = await AdaptiveMediaSource.CreateFromStreamAsync(memory.GetInputStreamAt(0),
                new Uri("https://music.youtube.com/"), "application/dash+xml", http)
                .AsTask(deadline.Token).ConfigureAwait(false);
            if (result.Status != AdaptiveMediaSourceCreationStatus.Success)
                throw new MusicCoreException("media_source", $"Windows adaptive source creation failed ({result.Status})");
            adaptive = result.MediaSource;
            source = MediaSource.CreateFromAdaptiveMediaSource(adaptive);
            deadline.Token.ThrowIfCancellationRequested();
            return new WindowsMusicSource(source, adaptive, http, memory);
        }
        catch (OperationCanceledException) when (!cancellation.IsCancellationRequested && deadline.IsCancellationRequested)
        {
            source?.Dispose(); adaptive?.Dispose(); http.Dispose(); memory.Dispose();
            throw Timeout();
        }
        catch (COMException error)
        {
            source?.Dispose(); adaptive?.Dispose(); http.Dispose(); memory.Dispose();
            // Native exception text can contain media URLs; expose only the fixed category/code.
            throw new MusicCoreException("media_source", $"Windows media initialization failed (0x{error.HResult:X8})");
        }
        catch
        {
            source?.Dispose(); adaptive?.Dispose(); http.Dispose(); memory.Dispose();
            throw;
        }
        finally { CryptographicOperations.ZeroMemory(bytes); }
    }

    private static TimeSpan ValidateTimeout(TimeSpan? timeout)
    {
        TimeSpan budget = timeout ?? TimeSpan.FromSeconds(120);
        if (budget.TotalMilliseconds < 1 || budget.TotalMilliseconds > 600000)
            throw new ArgumentOutOfRangeException(nameof(timeout));
        return budget;
    }
    private static MusicCoreException Timeout() => new("timeout", "Windows media initialization timed out", retryable: true);

    public void Dispose()
    {
        MediaSource? previous = Interlocked.Exchange(ref source, null);
        if (previous is null) return;
        previous.Dispose();
        adaptive.Dispose();
        http.Dispose();
        manifestStream.Dispose();
    }
}
