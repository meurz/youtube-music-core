using System.Collections.Concurrent;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Threading.Channels;
using YouTubeMusic.Interop;

// All requests terminate in this local CONNECT sink. No Google account/network required.
await using var proxy = new StallProxy();
string config = JsonSerializer.Serialize(new { client_version = "test", proxy = proxy.Url });
string bootstrap = JsonSerializer.Serialize(new { proxy = proxy.Url });
using (var cancelled = new CancellationTokenSource())
{
    cancelled.Cancel();
    await Expect<OperationCanceledException>(() => MusicCoreClient.CreateAsync(config, cancelled.Token));
}
await Expect<MusicCoreException>(() => MusicCoreClient.CreateAsync("{\"unknown\":true}"), e => e.Code == "invalid_input");
using (var cancel = new CancellationTokenSource())
{
    var create = MusicCoreClient.CreateAsync(bootstrap, cancel.Token);
    var connection = await proxy.NextAsync();
    cancel.Cancel();
    await Expect<OperationCanceledException>(async () => await create);
    await connection.WaitAsync(TimeSpan.FromSeconds(5));
}
var client = await MusicCoreClient.CreateAsync(config);
Assert((await client.CallAsync("{\"op\":\"capabilities\"}")).ValueKind == JsonValueKind.Object);
Assert(!await client.ExportSessionToAsync(_ => throw new Exception("Anonymous export called storage")));
await Expect<MusicCoreException>(() => client.CallAsync("{}"), e => e.Code == "invalid_input");

using (var cancel = new CancellationTokenSource())
{
    var phases = new ConcurrentQueue<string>();
    var first = client.CallAsync("{\"op\":\"prewarm\"}", cancel.Token, new Observer(p => phases.Enqueue(p.Phase)));
    var connection = await proxy.NextAsync();
    using var queuedCancel = new CancellationTokenSource();
    var queued = client.CallAsync("{\"op\":\"capabilities\"}", queuedCancel.Token);
    queuedCancel.Cancel();
    await Expect<OperationCanceledException>(async () => await queued);
    await Expect<MusicCoreException>(() => client.CallAsync("{\"op\":\"capabilities\"}", timeout: TimeSpan.FromMilliseconds(25)), e => e.Code == "timeout");
    cancel.Cancel();
    await Expect<OperationCanceledException>(async () => await first);
    await connection.WaitAsync(TimeSpan.FromSeconds(5));
    Assert(!phases.IsEmpty);
}
Console.WriteLine("active_cancel queued_cancel queue_timeout progress: passed");

var deadline = client.CallAsync("{\"op\":\"prewarm\"}", timeout: TimeSpan.FromMilliseconds(250));
var deadlineConnection = await proxy.NextAsync();
await Expect<MusicCoreException>(async () => await deadline, e => e.Code == "timeout" && e.Retryable);
await deadlineConnection.WaitAsync(TimeSpan.FromSeconds(5));

int observerConnected = 0;
var observerFailure = client.CallAsync("{\"op\":\"prewarm\"}", progress: new Observer(p =>
{
    // Phase names are informational; exercise failure only after real I/O starts.
    if (Volatile.Read(ref observerConnected) != 0) throw new ObserverFailure();
}));
var observerConnection = await proxy.NextAsync();
Volatile.Write(ref observerConnected, 1);
await Expect<ObserverFailure>(async () => await observerFailure);
await observerConnection.WaitAsync(TimeSpan.FromSeconds(5));
await client.CallAsync("{\"op\":\"capabilities\"}");
Console.WriteLine("native_timeout observer_exception_worker_cleanup: passed");

using (var cancel = new CancellationTokenSource())
{
    var running = client.CallAsync("{\"op\":\"prewarm\"}", cancel.Token);
    var connection = await proxy.NextAsync();
    var disposal = client.DisposeAsync().AsTask();
    Assert(!disposal.IsCompleted);
    cancel.Cancel();
    await Expect<OperationCanceledException>(async () => await running);
    await disposal.WaitAsync(TimeSpan.FromSeconds(5));
    await connection.WaitAsync(TimeSpan.FromSeconds(5));
}
await client.DisposeAsync();
await Expect<ObjectDisposedException>(() => client.CallAsync("{\"op\":\"capabilities\"}"));

// Exercise a throwing observer racing an otherwise successful native create.
for (int i = 0; i < 20; i++)
{
    try
    {
        await using var raced = await MusicCoreClient.CreateAsync(config, progress: new Observer(_ =>
        {
            Thread.Sleep(10);
            throw new ObserverFailure();
        }));
    }
    catch (ObserverFailure) { }
}
// The native registry has 128 client slots. All must still be available: a
// successful create abandoned by a throwing observer must not occupy a slot.
var clients = new List<MusicCoreClient>();
try
{
    for (int i = 0; i < 128; i++) clients.Add(await MusicCoreClient.CreateAsync(config));
    await Expect<MusicCoreException>(() => MusicCoreClient.CreateAsync(config), e => e.Code == "invalid_input");
}
finally { foreach (var live in clients) await live.DisposeAsync(); }
Console.WriteLine("create_handles dispose double_dispose disposed_call: passed");

static void Assert(bool condition) { if (!condition) throw new Exception("Contract assertion failed"); }
static async Task Expect<T>(Func<Task> action, Func<T, bool>? predicate = null) where T : Exception
{
    try { await action().WaitAsync(TimeSpan.FromSeconds(10)); }
    catch (T error) when (predicate is null || predicate(error)) { return; }
    throw new Exception($"Expected {typeof(T).Name}");
}
sealed class Observer(Action<MusicProgress> report) : IProgress<MusicProgress>
{
    public void Report(MusicProgress value) => report(value);
}
sealed class ObserverFailure : Exception;

sealed class StallProxy : IAsyncDisposable
{
    private readonly TcpListener listener = new(IPAddress.Loopback, 0);
    private readonly CancellationTokenSource stop = new();
    private readonly Channel<Task> arrivals = Channel.CreateUnbounded<Task>();
    private readonly ConcurrentBag<Task> connections = [];
    private readonly Task accept;
    public string Url { get; }
    public StallProxy()
    {
        listener.Start();
        Url = $"http://127.0.0.1:{((IPEndPoint)listener.LocalEndpoint).Port}";
        accept = AcceptAsync();
    }
    public Task<Task> NextAsync() => arrivals.Reader.ReadAsync().AsTask().WaitAsync(TimeSpan.FromSeconds(5));
    private async Task AcceptAsync()
    {
        try
        {
            while (true)
            {
                var client = await listener.AcceptTcpClientAsync(stop.Token);
                connections.Add(HoldAsync(client));
            }
        }
        catch (OperationCanceledException) { }
    }
    private async Task HoldAsync(TcpClient client)
    {
        using (client)
        {
            var closed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            try
            {
                var stream = client.GetStream();
                var buffer = new byte[4096];
                int bytes = await stream.ReadAsync(buffer, stop.Token);
                string request = Encoding.ASCII.GetString(buffer, 0, bytes);
                if (!request.StartsWith("CONNECT music.youtube.com:443 ", StringComparison.Ordinal)
                    && !request.StartsWith("CONNECT www.youtube.com:443 ", StringComparison.Ordinal))
                    throw new Exception("Unexpected proxy target");
                await arrivals.Writer.WriteAsync(closed.Task, stop.Token);
                while (await stream.ReadAsync(buffer, stop.Token) != 0) { }
                closed.SetResult();
            }
            catch (OperationCanceledException) { closed.TrySetCanceled(); }
            catch (Exception error) { closed.TrySetException(error); throw; }
        }
    }
    public async ValueTask DisposeAsync()
    {
        stop.Cancel();
        await accept;
        listener.Stop();
        await Task.WhenAll(connections);
        stop.Dispose();
    }
}
