using System.Text.Json;

namespace YouTubeMusic.Interop;

public sealed partial class OfficialWebPlayer
{
    /// <summary>Mint real official-page PO tokens and install them into the same core session.</summary>
    /// <remarks>
    /// Tokens, signing cookies and session bindings are excluded from this method's
    /// return value, which contains only the video ID, expiry and success flag. A
    /// cancellation during the browser VM phase waits for its own bounded cleanup
    /// (25 seconds) before returning; native context/export/install calls cancel normally.
    /// </remarks>
    public async Task<JsonElement> RefreshPoTokensAsync(MusicCoreClient client, string videoId,
        CancellationToken cancellation = default)
    {
        CheckThread();
        ArgumentNullException.ThrowIfNull(client);
        _ = OfficialWatchUri(videoId);
        cancellation.ThrowIfCancellationRequested();
        JsonElement context = await client.CallAsync(JsonSerializer.Serialize(new {
            op = "attestation_context", video_id = videoId
        }), cancellation);
        var expected = new Dictionary<string, string>(StringComparer.Ordinal);
        bool exported = await client.ExportSessionToAsync(bytes => {
            using JsonDocument document = JsonDocument.Parse(bytes);
            string header = document.RootElement.GetProperty("cookie").GetString() ?? "";
            foreach (string part in header.Split(';'))
            {
                int separator = part.IndexOf('=');
                if (separator < 1) continue;
                string name = part[..separator].Trim();
                if (!SigningCookie(name)) continue;
                string value = part[(separator + 1)..].Trim();
                if (expected.TryGetValue(name, out string? previous) && previous != value)
                    throw IdentityChanged();
                expected[name] = value;
            }
            return Task.CompletedTask;
        }, cancellation);
        if (!exported || expected.Count == 0) throw IdentityChanged();
        try
        {
            await VerifyAttestationIdentityAsync(context, expected);
            cancellation.ThrowIfCancellationRequested();
            string script = context.GetProperty("script").GetString() ?? throw IdentityChanged();
            JsonElement token = await EvaluateAsync(script, 35000);
            cancellation.ThrowIfCancellationRequested();
            if (token.TryGetProperty("error", out _))
                throw new MusicCoreException("attestation_failed", "The official browser could not issue proof-of-origin tokens", retryable: true);
            await VerifyAttestationIdentityAsync(context, expected);
            JsonElement after = await client.CallAsync(JsonSerializer.Serialize(new {
                op = "attestation_context", video_id = videoId
            }), cancellation);
            if (after.GetProperty("session_binding").GetString() != context.GetProperty("session_binding").GetString())
                throw IdentityChanged();
            string? mintedVideo = token.GetProperty("video_id").GetString();
            if (mintedVideo != videoId) throw IdentityChanged();
            long expires = Math.Min(token.GetProperty("expires_at").GetInt64(), context.GetProperty("max_expires_at").GetInt64());
            await client.CallAsync(JsonSerializer.Serialize(new {
                op = "set_po_tokens", tokens = new[] { new {
                    video_id = videoId,
                    player_token = token.GetProperty("player_token").GetString(),
                    gvs_token = token.GetProperty("gvs_token").GetString(),
                    expires_at = expires,
                    session_binding = context.GetProperty("session_binding").GetString()
                }}
            }), cancellation);
            return JsonSerializer.SerializeToElement(new { updated = true, video_id = videoId, expires_at = expires });
        }
        finally { expected.Clear(); }
    }

    private async Task VerifyAttestationIdentityAsync(JsonElement context, Dictionary<string, string> expected)
    {
        JsonElement account = await EvaluateAsync("""
            (()=>({logged_in:globalThis.ytcfg?.get('LOGGED_IN')===true,
              auth_user:String(globalThis.ytcfg?.get('SESSION_INDEX')??'0'),
              delegated_session_id:globalThis.ytcfg?.get('DELEGATED_SESSION_ID')||null}))()
            """);
        if (!account.GetProperty("logged_in").GetBoolean()
            || account.GetProperty("auth_user").GetString() != context.GetProperty("auth_user").GetUInt32().ToString(System.Globalization.CultureInfo.InvariantCulture)
            || account.GetProperty("delegated_session_id").GetString() != context.GetProperty("delegated_session_id").GetString())
            throw IdentityChanged();
        var actual = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var cookie in await web.CookieManager.GetCookiesAsync("https://music.youtube.com/"))
        {
            if (!SigningCookie(cookie.Name)) continue;
            if (actual.TryGetValue(cookie.Name, out string? previous) && previous != cookie.Value)
                throw IdentityChanged();
            actual[cookie.Name] = cookie.Value;
        }
        if (actual.Count != expected.Count || expected.Any(pair => !actual.TryGetValue(pair.Key, out string? value) || pair.Value != value))
            throw IdentityChanged();
    }

    private static bool SigningCookie(string name) => name is "SAPISID" or "__Secure-3PAPISID" or "__Secure-1PAPISID";
    private static MusicCoreException IdentityChanged() => new("attestation_session_mismatch",
        "The official browser and core must use the same unchanged signed-in Music identity");
}
