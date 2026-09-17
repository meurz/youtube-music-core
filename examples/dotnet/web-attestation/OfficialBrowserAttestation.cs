using System.Text.Json;
using System.Text.RegularExpressions;
using System.Runtime.InteropServices;
using Microsoft.Web.WebView2.Core;

namespace YouTubeMusic.Interop;

/// <summary>Proof-of-origin attestation using a host-owned official Music WebView2.</summary>
/// <remarks>
/// Construct and call on the WebView's UI thread, after EnsureCoreWebView2Async.
/// Use a dedicated persistent user-data folder protected by the Windows user account.
/// </remarks>
public sealed partial class OfficialBrowserAttestation
{
    private readonly CoreWebView2 web;
    private readonly int uiThread = Environment.CurrentManagedThreadId;
    private bool loading;

    public OfficialBrowserAttestation(CoreWebView2 web)
    {
        this.web = web ?? throw new ArgumentNullException(nameof(web));
    }

    /// <summary>Import an explicitly exported core session, only into the Music host.</summary>
    /// <remarks>
    /// Call before navigation in a dedicated profile; never log or persist the JSON.
    /// Cookie header imports cannot reconstruct all browser cookie attributes. Values
    /// stay host-only, secure, and session-only unless a known future expiry exists.
    /// Signing cookies are page-readable because the official page requires them.
    /// Brand-account selection is performed in the official page; it is not inferred.
    /// </remarks>
    public async Task ImportMusicSessionAsync(JsonElement session)
    {
        CheckThread();
        if (loading) throw new InvalidOperationException("Wait for navigation before replacing a session");
        if ((session.TryGetProperty("auth_user", out JsonElement authUser) && authUser.GetUInt32() != 0)
            || (session.TryGetProperty("delegated_session_id", out JsonElement delegated) && delegated.ValueKind != JsonValueKind.Null))
            throw new MusicCoreException("attestation_account_selection", "Select this account in the official browser page; cookie import supports only the primary non-delegated account");
        string header = session.GetProperty("cookie").GetString() ?? "";
        if (header.Length is 0 or > 65536 || header.Any(c => c < 32 || c == 127))
            throw new ArgumentException("Invalid Music cookie header", nameof(session));
        var parsed = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (string part in header.Split(';'))
        {
            int separator = part.IndexOf('=');
            if (separator < 1) throw new ArgumentException("Invalid Music cookie header", nameof(session));
            string name = part[..separator].Trim();
            string value = part[(separator + 1)..].Trim();
            if (!CookieName().IsMatch(name) || !parsed.TryAdd(name, value))
                throw new ArgumentException("Ambiguous Music cookie header", nameof(session));
        }
        // Validate the entire import before touching the browser cookie store.
        var prepared = new List<CoreWebView2Cookie>();
        bool hasExpiry = session.TryGetProperty("cookie_expirations", out JsonElement expirations)
            && expirations.ValueKind == JsonValueKind.Object;
        long now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        foreach ((string name, string value) in parsed)
        {
            var cookie = web.CookieManager.CreateCookie(name, value, "music.youtube.com", "/");
            cookie.IsSecure = true;
            cookie.IsHttpOnly = name is not ("SAPISID" or "APISID" or "__Secure-1PAPISID" or "__Secure-3PAPISID"
                or "PREF" or "YSC" or "VISITOR_INFO1_LIVE" or "VISITOR_PRIVACY_METADATA");
            cookie.SameSite = CoreWebView2CookieSameSiteKind.Lax;
            if (hasExpiry && expirations.TryGetProperty(name, out JsonElement expiry))
            {
                if (!expiry.TryGetInt64(out long timestamp) || timestamp < 0 || timestamp > 253402300799)
                    throw new ArgumentException("Invalid Music cookie expiry", nameof(session));
                if (timestamp <= now) continue;
#if WEBVIEW2_WINRT
                cookie.Expires = timestamp;
#else
                cookie.Expires = DateTimeOffset.FromUnixTimeSeconds(timestamp).UtcDateTime;
#endif
            }
            prepared.Add(cookie);
        }
        // Replace only cookies visible to the Music origin. Use a dedicated profile
        // so stale account cookies cannot combine with the imported session.
        loading = true;
        try
        {
            var previous = await web.CookieManager.GetCookiesAsync("https://music.youtube.com/");
            foreach (CoreWebView2Cookie cookie in previous) web.CookieManager.DeleteCookie(cookie);
            foreach (CoreWebView2Cookie cookie in prepared) web.CookieManager.AddOrUpdateCookie(cookie);
        }
        finally { loading = false; }
    }

    /// <summary>Load the Music home page for official consent, login and attestation.</summary>
    public async Task LoadAsync(CancellationToken cancellation = default, TimeSpan? timeout = null)
    {
        CheckThread();
        if (loading) throw new InvalidOperationException("A navigation is already running");
        var uri = new Uri("https://music.youtube.com/");
        TimeSpan budget = timeout ?? TimeSpan.FromSeconds(60);
        if (budget.TotalMilliseconds is < 1 or > 600000) throw new ArgumentOutOfRangeException(nameof(timeout));
        cancellation.ThrowIfCancellationRequested();
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        deadline.CancelAfter(budget);
        var completion = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        ulong? navigationId = null;
        void Starting(object? sender, CoreWebView2NavigationStartingEventArgs args)
        {
            if (navigationId is null && args.Uri == uri.AbsoluteUri) navigationId = args.NavigationId;
        }
        void Completed(object? sender, CoreWebView2NavigationCompletedEventArgs args)
        {
            if (args.NavigationId != navigationId) return;
            if (args.IsSuccess) completion.TrySetResult();
            else completion.TrySetException(new MusicCoreException("attestation_navigation",
                $"Official Music navigation failed ({args.WebErrorStatus})", retryable: true));
        }
        loading = true;
        web.NavigationStarting += Starting;
        web.NavigationCompleted += Completed;
        try
        {
            web.Navigate(uri.AbsoluteUri);
            await completion.Task.WaitAsync(deadline.Token);
        }
        catch (OperationCanceledException)
        {
            web.Stop();
            if (!cancellation.IsCancellationRequested)
                throw new MusicCoreException("timeout", "Official Music navigation timed out", retryable: true);
            throw;
        }
        finally
        {
            web.NavigationStarting -= Starting;
            web.NavigationCompleted -= Completed;
            loading = false;
        }
    }

    private async Task<JsonElement> EvaluateAsync(string expression, int timeoutMilliseconds = 15000)
    {
        CheckThread();
        if (!Uri.TryCreate(web.Source, UriKind.Absolute, out Uri? source)
            || source.Scheme != "https" || source.Host != "music.youtube.com" || !source.IsDefaultPort)
            throw new MusicCoreException("attestation_origin", "Open the official Music page before requesting attestation");
        string guarded = "(() => { if (location.origin !== 'https://music.youtube.com') throw new Error('attestation_origin'); return (" + expression + "); })()";
        string result;
        try
        {
            result = await web.CallDevToolsProtocolMethodAsync("Runtime.evaluate", JsonSerializer.Serialize(new {
                expression = guarded, awaitPromise = true, returnByValue = true, timeout = timeoutMilliseconds
            }));
        }
        catch (COMException error)
        {
            throw new MusicCoreException("attestation_script", $"Browser action failed (0x{error.HResult:X8})");
        }
        using var document = JsonDocument.Parse(result);
        if (document.RootElement.TryGetProperty("exceptionDetails", out _)
            || !document.RootElement.TryGetProperty("result", out JsonElement response)
            || !response.TryGetProperty("value", out JsonElement value))
            throw new MusicCoreException("attestation_script", "Official Music action did not complete");
        return value.Clone();
    }

    private void CheckThread()
    {
        if (Environment.CurrentManagedThreadId != uiThread)
            throw new InvalidOperationException("Use the WebView UI thread");
    }

    [GeneratedRegex("^[A-Za-z0-9_-]{11}$", RegexOptions.CultureInvariant)]
    private static partial Regex VideoId();
    [GeneratedRegex("^[!#$%&'*+.^_`|~0-9A-Za-z-]+$", RegexOptions.CultureInvariant)]
    private static partial Regex CookieName();
}
