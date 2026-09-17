using System.Text.Json;
using System.Text.RegularExpressions;
using System.Runtime.InteropServices;
using Microsoft.Web.WebView2.Core;

namespace YouTubeMusic.Interop;

/// <summary>Official Music page playback in a host-owned WebView2.</summary>
/// <remarks>
/// Construct and call on the WebView's UI thread, after EnsureCoreWebView2Async.
/// Use a dedicated persistent user-data folder protected by the Windows user account.
/// The official page owns EME sessions, CDM selection and license acquisition.
/// WebView2 versions/devices may lack Widevine: inspect ProbeKeySystemsAsync, and
/// use a full supported browser when required. Never claim encrypted playback
/// from a successful clear-track test or from CDM availability alone.
/// </remarks>
public sealed partial class OfficialWebPlayer
{
    private readonly CoreWebView2 web;
    private readonly int uiThread = Environment.CurrentManagedThreadId;
    private bool loading;

    public OfficialWebPlayer(CoreWebView2 web)
    {
        this.web = web ?? throw new ArgumentNullException(nameof(web));
    }

    /// <summary>Import an explicitly exported core session, only into the Music host.</summary>
    /// <remarks>
    /// Call before navigation in a dedicated profile; never log or persist the JSON.
    /// Cookie header imports cannot reconstruct all browser cookie attributes. Values
    /// stay host-only, secure, and session-only unless a known future expiry exists.
    /// Signing cookies are page-readable because the official player requires them.
    /// Brand-account selection is performed in the official page; it is not inferred.
    /// </remarks>
    public async Task ImportMusicSessionAsync(JsonElement session)
    {
        CheckThread();
        if (loading) throw new InvalidOperationException("Wait for navigation before replacing a session");
        if ((session.TryGetProperty("auth_user", out JsonElement authUser) && authUser.GetUInt32() != 0)
            || (session.TryGetProperty("delegated_session_id", out JsonElement delegated) && delegated.ValueKind != JsonValueKind.Null))
            throw new MusicCoreException("official_player_account_selection", "Select this account in the official browser page; cookie import supports only the primary non-delegated account");
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

    public static Uri OfficialWatchUri(string videoId)
    {
        if (!VideoId().IsMatch(videoId ?? "")) throw new ArgumentException("Invalid video ID", nameof(videoId));
        return new Uri($"https://music.youtube.com/watch?v={videoId}");
    }

    /// <summary>Load the real watch page; the visible official UI handles consent and login.</summary>
    public async Task LoadAsync(string videoId, CancellationToken cancellation = default,
        TimeSpan? timeout = null)
    {
        CheckThread();
        if (loading) throw new InvalidOperationException("A navigation is already running");
        Uri uri = OfficialWatchUri(videoId);
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
            else completion.TrySetException(new MusicCoreException("official_player_navigation",
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
                throw new MusicCoreException("timeout", "Official player navigation timed out", retryable: true);
            throw;
        }
        finally
        {
            web.NavigationStarting -= Starting;
            web.NavigationCompleted -= Completed;
            loading = false;
        }
    }

    /// <summary>Probe licensed key systems in the current Music origin; does not request any license.</summary>
    public Task<JsonElement> ProbeKeySystemsAsync() => EvaluateAsync("""
        (async () => {
          const result = {};
          for (const key of ['com.widevine.alpha', 'com.microsoft.playready.recommendation', 'com.microsoft.playready']) {
            try {
              await navigator.requestMediaKeySystemAccess(key, [{initDataTypes:['cenc'],
                audioCapabilities:[{contentType:'audio/mp4; codecs="mp4a.40.2"'}]}]);
              result[key] = true;
            } catch { result[key] = false; }
          }
          return result;
        })()
        """);

    /// <summary>Determine whether a required DRM key system is available in this WebView.</summary>
    /// <remarks>This is capability routing, not a successful track-license assertion.</remarks>
    public async Task<JsonElement> DrmRouteAsync(JsonElement descriptor)
    {
        string videoId = descriptor.GetProperty("video_id").GetString() ?? "";
        Uri official = OfficialWatchUri(videoId);
        JsonElement systems = await ProbeKeySystemsAsync();
        var available = new List<string>();
        foreach (JsonElement required in descriptor.GetProperty("key_systems").EnumerateArray())
        {
            string? key = required.GetString();
            if (key is not null && systems.TryGetProperty(key, out JsonElement supported) && supported.GetBoolean())
                available.Add(key);
        }
        return JsonSerializer.SerializeToElement(new {
            route = available.Count > 0 ? "official_webview" : "full_browser_required",
            available_key_systems = available,
            official_watch_url = official.AbsoluteUri,
            license_verified = false
        });
    }

    public Task<JsonElement> StateAsync() => EvaluateAsync("""
        (() => {
          const media = document.querySelector('video, audio');
          return media ? {available:true, paused:media.paused, ended:media.ended,
            position_seconds:media.currentTime, duration_seconds:Number.isFinite(media.duration)?media.duration:null,
            ready_state:media.readyState, encrypted:media.mediaKeys!==null,
            is_ad:document.getElementById('movie_player')?.classList.contains('ad-showing')??false,
            error_code:media.error?.code??null} : {available:false};
        })()
        """);

    /// <summary>Set userInitiated only when handling the host's explicit Play action.</summary>
    /// <remarks>Automatic playback can still require a user interaction with the official page.</remarks>
    public Task<JsonElement> PlayAsync(bool userInitiated = false) => EvaluateAsync("""
        (async () => {const media=document.querySelector('video, audio');
          if(!media)return {accepted:false, reason:'media_unavailable'};
          try {await media.play();return {accepted:true};}
          catch {return {accepted:false,reason:'official_player_interaction_required'};}})()
        """, userInitiated: userInitiated);

    public Task<JsonElement> PauseAsync() => EvaluateAsync("""
        (() => {const media=document.querySelector('video, audio');if(!media)return {accepted:false};
          media.pause();return {accepted:true};})()
        """);

    public Task<JsonElement> SeekAsync(double seconds)
    {
        if (!double.IsFinite(seconds) || seconds < 0) throw new ArgumentOutOfRangeException(nameof(seconds));
        string position = JsonSerializer.Serialize(seconds);
        return EvaluateAsync($"(() => {{if(document.getElementById('movie_player')?.classList.contains('ad-showing'))return {{accepted:false,reason:'ad_playback'}};const m=document.querySelector('video, audio');if(!m||!Number.isFinite(m.duration))return {{accepted:false}};m.currentTime=Math.min({position},m.duration);return {{accepted:true}};}})()");
    }

    private async Task<JsonElement> EvaluateAsync(string expression, int timeoutMilliseconds = 15000,
        bool userInitiated = false)
    {
        CheckThread();
        if (!Uri.TryCreate(web.Source, UriKind.Absolute, out Uri? source)
            || source.Scheme != "https" || source.Host != "music.youtube.com" || !source.IsDefaultPort)
            throw new MusicCoreException("official_player_origin", "Open the official Music page before controlling playback");
        string guarded = "(() => { if (location.origin !== 'https://music.youtube.com') throw new Error('official_player_origin'); return (" + expression + "); })()";
        string result;
        try
        {
            result = await web.CallDevToolsProtocolMethodAsync("Runtime.evaluate", JsonSerializer.Serialize(new {
                expression = guarded, awaitPromise = true, returnByValue = true, timeout = timeoutMilliseconds,
                userGesture = userInitiated
            }));
        }
        catch (COMException error)
        {
            throw new MusicCoreException("official_player_script", $"Browser action failed (0x{error.HResult:X8})");
        }
        using var document = JsonDocument.Parse(result);
        if (document.RootElement.TryGetProperty("exceptionDetails", out _)
            || !document.RootElement.TryGetProperty("result", out JsonElement response)
            || !response.TryGetProperty("value", out JsonElement value))
            throw new MusicCoreException("official_player_script", "Official player action did not complete");
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
