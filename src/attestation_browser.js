async function mintMusicProofOfOrigin(videoId) {
    // Protocol observations: https://github.com/LuanRT/BgUtils (MIT).
    // This executes Google's own VM in a real official-page browser environment.
    if (location.origin !== 'https://music.youtube.com' ||
        !/^[A-Za-z0-9_-]{11}$/.test(videoId) ||
        globalThis.ytcfg?.get('LOGGED_IN') !== true) {
        return { error: 'official_music_login_required' };
    }
    const playerConfig = document.getElementById('movie_player')?.getWebPlayerContextConfig?.();
    const dataSyncId = playerConfig?.datasyncId || ytcfg.get('DATASYNC_ID');
    const configuredDataSyncId = ytcfg.get('DATASYNC_ID');
    if (ytcfg.get('INNERTUBE_CONTEXT')?.client?.clientName !== 'WEB_REMIX' ||
        (typeof configuredDataSyncId === 'string' && configuredDataSyncId !== dataSyncId)) {
        return { error: 'official_player_context_mismatch' };
    }
    if (typeof dataSyncId !== 'string' || !dataSyncId || dataSyncId.length > 256 ||
        typeof playerConfig?.serializedExperimentFlags !== 'string') {
        return { error: 'official_player_context_required' };
    }
    const flags = new URLSearchParams(playerConfig.serializedExperimentFlags);
    const gvsBinding = flags.get('html5_generate_content_po_token') === 'true' ? videoId : dataSyncId;
    const requestKey = flags.get('html5_web_po_request_key') || 'O43z0dpjhgX20SCx4KAo';
    const account = JSON.stringify([
        ytcfg.get('SESSION_INDEX'), ytcfg.get('DELEGATED_SESSION_ID')
    ]);
    const abort = new AbortController();
    const timeout = setTimeout(() => abort.abort(), 25000);
    let shutdown;
    let stage = 'challenge';
    const bounded = promise => Promise.race([
        promise,
        new Promise((_, reject) => {
            if (abort.signal.aborted) reject(new Error('timeout'));
            else abort.signal.addEventListener('abort', () => reject(new Error('timeout')), { once: true });
        })
    ]);
    const request = async (path, body, headers) => {
        const response = await fetch(path, {
            method: 'POST', credentials: 'same-origin', redirect: 'error',
            signal: abort.signal, headers, body: JSON.stringify(body)
        });
        if (!response.ok) throw new Error('request_failed');
        const text = await response.text();
        if (text.length > 1024 * 1024) throw new Error('response_limit');
        return JSON.parse(text);
    };
    try {
        const challenge = await request('/youtubei/v1/att/get?prettyPrint=false', {
            context: ytcfg.get('INNERTUBE_CONTEXT'), engagementType: 'ENGAGEMENT_TYPE_UNBOUND'
        }, { 'content-type': 'application/json' });
        const c = challenge.bgChallenge;
        if (!c || typeof c.program !== 'string' || typeof c.globalName !== 'string') {
            throw new Error('challenge_unavailable');
        }
        stage = 'interpreter';
        if (typeof globalThis[c.globalName]?.a !== 'function') {
            const url = new URL(c.interpreterUrl?.privateDoNotAccessOrElseTrustedResourceUrlWrappedValue, location.href);
            if (url.protocol !== 'https:' || url.hostname !== 'www.google.com' ||
                !url.pathname.startsWith('/js/bg/') || url.username || url.password || url.port) {
                throw new Error('interpreter_source_rejected');
            }
            const script = document.createElement('script');
            let src = url.href;
            if (globalThis.trustedTypes) {
                const expected = src;
                const policy = trustedTypes.createPolicy('ytmusic-po-' + crypto.randomUUID(), {
                    createScriptURL(value) {
                        if (value !== expected) throw new Error('interpreter_source_rejected');
                        return value;
                    }
                });
                src = policy.createScriptURL(src);
            }
            script.src = src;
            try {
                await bounded(new Promise((resolve, reject) => {
                    script.onload = resolve;
                    script.onerror = () => reject(new Error('interpreter_load_failed'));
                    document.head.appendChild(script);
                }));
            } finally { script.remove(); }
        }
        const vm = globalThis[c.globalName];
        if (typeof vm?.a !== 'function') throw new Error('interpreter_unavailable');
        stage = 'snapshot';
        const functions = await bounded(new Promise(resolve => {
            vm.a(c.program, (snapshot, close) => {
                shutdown = close;
                // Late initialization after a deadline must not leak a live VM.
                if (abort.signal.aborted && typeof close === 'function') close();
                resolve({ snapshot });
            }, true, undefined, () => {}, [[], []], undefined, false,
            [() => {}, () => {}, () => {}, () => {}, () => {}]);
        }));
        const signals = [];
        const response = await bounded(new Promise(resolve => {
            functions.snapshot(resolve, [undefined, undefined, signals, undefined]);
        }));
        stage = 'integrity';
        const integrity = await request('/api/jnn/v1/GenerateIT', [
            requestKey, response
        ], {
            'content-type': 'application/json+protobuf',
            'x-goog-api-key': 'AIzaSyDyT5W0Jh49F30Pqqtyfdf7pDLFKLJoAnw',
            'x-user-agent': 'grpc-web-javascript/0.1'
        });
        if (typeof integrity[0] !== 'string' || !Number.isFinite(integrity[1]) || integrity[1] <= 30 ||
            typeof signals[0] !== 'function') throw new Error('integrity_unavailable');
        stage = 'mint';
        const raw = atob(integrity[0].replace(/-/g, '+').replace(/_/g, '/'));
        const mint = await bounded(Promise.resolve(signals[0](Uint8Array.from(raw, c => c.charCodeAt(0)))));
        if (typeof mint !== 'function') throw new Error('minter_unavailable');
        const encode = async binding => {
            const bytes = await bounded(Promise.resolve(mint(new TextEncoder().encode(binding))));
            if (!(bytes instanceof Uint8Array) || bytes.length < 24 || bytes.length > 6144) {
                throw new Error('invalid_minted_token');
            }
            return btoa(String.fromCharCode(...bytes)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
        };
        // WEB_REMIX player requests bind to the full Data Sync ID. GVS
        // content binding follows the official player experiment, not PLAYER.
        const playerToken = await encode(dataSyncId);
        const gvsToken = await encode(gvsBinding);
        if (location.origin !== 'https://music.youtube.com' || ytcfg.get('LOGGED_IN') !== true ||
            JSON.stringify([ytcfg.get('SESSION_INDEX'), ytcfg.get('DELEGATED_SESSION_ID')]) !== account ||
            (document.getElementById('movie_player')?.getWebPlayerContextConfig?.()?.datasyncId || ytcfg.get('DATASYNC_ID')) !== dataSyncId ||
            ytcfg.get('DATASYNC_ID') !== configuredDataSyncId) {
            throw new Error('account_changed');
        }
        return {
            video_id: videoId, player_token: playerToken, gvs_token: gvsToken,
            expires_at: Math.floor(Date.now() / 1000) + Math.min(Math.floor(integrity[1]), 3600)
        };
    } catch (_) {
        // Exceptions may contain challenge data, account IDs or tokens.
        return { error: abort.signal.aborted ? 'attestation_timeout' : 'attestation_failed', stage };
    } finally {
        clearTimeout(timeout);
        abort.abort();
        try { if (typeof shutdown === 'function') shutdown(); } catch (_) {}
    }
}
