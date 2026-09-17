use serde_json::{json, Value};
use youtube_music_core::{core_call, parse, Error, Request};

#[test]
fn real_song_search_extracts_relationships_and_duration() {
    let raw = serde_json::from_str(include_str!("fixtures/search.json")).unwrap();
    let page = parse::page(&raw).unwrap();
    let song = &page.sections[0].items[0];
    assert_eq!(page.sections[0].title, "Songs");
    assert_eq!(song.video_id.as_deref(), Some("4D7u5KF7SP8"));
    assert_eq!(
        song.title,
        "Get Lucky (feat. Pharrell Williams and Nile Rodgers)"
    );
    assert_eq!(song.duration_seconds, Some(370));
    assert_eq!(song.artists.len(), 3);
    assert_eq!(song.album.as_ref().unwrap().browse_id, "MPREb_K8qWMWVqXGi");
    assert!(!song.thumbnails.is_empty());
}

#[test]
fn real_queue_extracts_track_without_menu_duplicates() {
    let raw = serde_json::from_str(include_str!("fixtures/next.json")).unwrap();
    let page = parse::page(&raw).unwrap();
    // The second renderer is an automix link, not another track.
    assert_eq!(page.sections[0].items.len(), 1);
    assert_eq!(
        page.sections[0].items[0].video_id.as_deref(),
        Some("5NV6Rdv1a3I")
    );
    assert_eq!(page.sections[0].items[0].duration_seconds, Some(249));
}

#[test]
fn real_blocked_player_preserves_metadata_and_reason() {
    let raw = serde_json::from_str(include_str!("fixtures/player.json")).unwrap();
    let p = parse::player(&raw).unwrap();
    assert_eq!(p.status, "UNPLAYABLE");
    assert_eq!(p.reason.as_deref(), Some("Video unavailable"));
    assert_eq!(p.track.unwrap().duration_seconds, Some(249));
    assert!(p.audio_streams.is_empty());
}

#[test]
fn only_ready_audio_urls_are_exposed() {
    let p = parse::player(&json!({"playabilityStatus":{"status":"OK"},"streamingData":{
        "expiresInSeconds":"3600","adaptiveFormats":[
            {"itag":140,"mimeType":"audio/mp4","bitrate":128000,"url":"https://example.com/audio?expire=123"},
            {"itag":251,"mimeType":"audio/webm","bitrate":160000,"url":"https://example.com/audio"},
            {"itag":250,"mimeType":"audio/webm","signatureCipher":"s=encrypted&url=https%3A%2F%2Fexample.com"},
            {"itag":249,"mimeType":"audio/webm","url":"https://example.com/audio?n=challenge"},
            {"itag":248,"mimeType":"video/webm","url":"https://example.com/video"},
            {"itag":139,"mimeType":"audio/mp4","url":"file:///tmp/audio"}
        ]}})).unwrap();
    assert_eq!(p.audio_streams.len(), 2);
    assert_eq!(p.audio_streams[0].itag, 251);
    assert_eq!(p.audio_streams[1].expires_at, Some(123));
    assert!(p.audio_streams[0].verification.is_none());
    assert_eq!(p.unresolved_audio_formats, 3);
    assert_eq!(p.expires_in_seconds, Some(3600));
}

#[test]
fn section_tokens_do_not_collapse_into_one() {
    let p = parse::page(&json!({"contents":[
        {"musicShelfRenderer":{"title":{"simpleText":"Songs"},"contents":[],"continuations":[{"nextContinuationData":{"continuation":"songs-token"}}]}},
        {"musicShelfRenderer":{"title":{"simpleText":"Albums"},"contents":[],"continuations":[{"nextContinuationData":{"continuation":"albums-token"}}]}}
    ]})).unwrap();
    assert_eq!(p.sections[0].continuation.as_deref(), Some("songs-token"));
    assert_eq!(p.sections[1].continuation.as_deref(), Some("albums-token"));
}

#[test]
fn browse_continuation_and_two_row_album() {
    let p = parse::page(&json!({"continuationContents":{"musicShelfContinuation":{"contents":[
        {"musicTwoRowItemRenderer":{"title":{"runs":[{"text":"Album"}]},"navigationEndpoint":{"browseEndpoint":{"browseId":"MPRE123","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ALBUM"}}}}}}
    ],"continuations":[{"nextContinuationData":{"continuation":"next"}}]}}})).unwrap();
    assert_eq!(p.sections[0].items[0].kind, "album");
    assert_eq!(p.sections[0].items[0].browse_id.as_deref(), Some("MPRE123"));
    assert_eq!(p.sections[0].continuation.as_deref(), Some("next"));
}

#[test]
fn duration_rejects_malformed_and_overflow() {
    assert_eq!(parse::duration("1:02:03"), Some(3723));
    assert_eq!(parse::duration("90:01"), Some(5401));
    for s in [
        "live",
        "1:60",
        "1:2:3:4",
        "18446744073709551615:00",
        "-1:20",
    ] {
        assert_eq!(parse::duration(s), None, "{s}");
    }
}

#[test]
fn lyrics_preserve_linebreaks_and_source() {
    let v = json!({"contents":{"musicDescriptionShelfRenderer":{"description":{"runs":[{"text":"Line one\n"},{"text":"Line two"}]},"footer":{"simpleText":"Lyrics provider"}}}});
    let lyrics = parse::lyrics(&v, "MPLY123").unwrap();
    assert_eq!(lyrics.text, "Line one\nLine two");
    assert_eq!(lyrics.source.as_deref(), Some("Lyrics provider"));
    assert!(matches!(
        parse::lyrics(&json!({}), "none"),
        Err(Error::LyricsUnavailable)
    ));
}

#[test]
fn malformed_responses_are_errors() {
    assert!(parse::player(&json!({})).is_err());
    assert!(parse::page(&json!({"responseContext":{}})).is_err());
    assert!(parse::page(&json!([])).is_err());
    assert!(parse::page(&json!({"contents":{}}))
        .unwrap()
        .sections
        .is_empty());
}

#[test]
fn invalid_calls_fail_before_network_and_do_not_echo_secrets() {
    for input in [
        "not-json",
        r#"{"request":{"op":"song","video_id":"bad"}}"#,
        r#"{"config":{"cookie":"test-private-value"},"request":{"op":"unknown"}}"#,
    ] {
        let output = core_call(input);
        let v: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "invalid_input");
        assert!(!output.contains("test-private-value"));
    }
}

#[test]
fn request_validation_is_strict() {
    let legacy: Request =
        serde_json::from_value(json!({"op":"stream","video_id":"4D7u5KF7SP8"})).unwrap();
    assert!(legacy.validate().is_ok());
    assert!(serde_json::from_value::<Request>(
        json!({"op":"song","video_id":"5NV6Rdv1a3I","typo":true})
    )
    .is_err());
    assert!(Request::Search {
        query: "  ".into(),
        filter: Default::default()
    }
    .validate()
    .is_err());
    assert!(Request::Song {
        video_id: "5NV6Rdv1a3I".into()
    }
    .validate()
    .is_ok());
}

#[test]
fn new_protocol_requests_accept_documented_flat_fields_and_reject_extras() {
    for source in [
        r#"{"op":"create_playlist","title":"Fixture","privacy":"private","video_ids":[]}"#,
        r#"{"op":"edit_playlist","playlist_id":"PLfixture","title":"Renamed"}"#,
        r#"{"op":"queue_context","video_id":"4D7u5KF7SP8"}"#,
        r#"{"op":"home","params":"opaque"}"#,
        r#"{"op":"dash_manifest","video_id":"4D7u5KF7SP8"}"#,
        r#"{"op":"rate_song","video_id":"4D7u5KF7SP8","rating":"like"}"#,
    ] {
        let request: youtube_music_core::Request = serde_json::from_str(source).unwrap();
        request.validate().unwrap();
        let mut value: serde_json::Value = serde_json::from_str(source).unwrap();
        value["unexpected"] = true.into();
        assert!(serde_json::from_value::<youtube_music_core::Request>(value).is_err());
    }
}

#[test]
fn invalid_mutations_fail_before_bootstrap_and_hide_opaque_values() {
    let requests = [
        json!({"op":"rate_song","video_id":"bad-private-video","rating":"like"}),
        json!({"op":"rate_playlist","playlist_id":"private/id","rating":"like"}),
        json!({"op":"delete_playlist","playlist_id":"VL"}),
        json!({"op":"edit_library","feedback_tokens":[]}),
        json!({"op":"edit_library","feedback_tokens":["private-token\ninvalid"]}),
        json!({"op":"subscribe","channel_id":"private-channel","subscribed":true}),
        json!({"op":"create_playlist","title":"<private-title>"}),
        json!({"op":"edit_playlist","playlist_id":"PLfixture"}),
        json!({"op":"add_playlist_items","playlist_id":"PLfixture","video_ids":[]}),
        json!({"op":"add_playlist_items","playlist_id":"PLfixture","video_ids":["private-invalid"]}),
        json!({"op":"remove_playlist_items","playlist_id":"PLfixture","entries":[{"video_id":"4D7u5KF7SP8","set_video_id":""}]}),
        json!({"op":"move_playlist_item","playlist_id":"PLfixture","set_video_id":"private-entry","before_set_video_id":"private-entry"}),
    ];
    for request in requests {
        // A dead proxy makes accidental bootstrap return network, not invalid_input.
        let input = json!({"config":{"proxy":"http://127.0.0.1:1","cookie":"SAPISID=private-cookie"},"request":request});
        let output = core_call(&input.to_string());
        let result: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            result["error"]["code"], "invalid_input",
            "{}",
            input["request"]["op"]
        );
        assert!(!output.contains("private-"));
    }
}
