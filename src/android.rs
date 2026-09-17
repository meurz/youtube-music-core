//! Android Music transport and normalization of native renderer models.
use crate::{
    client::{checked_response, header, network},
    model::Page,
    parse, Error, MusicClient, Result,
};
use reqwest::header::HeaderMap;
use serde_json::{json, Value};

pub(crate) const VERSION: &str = "9.36.50";
pub(crate) const AUDIO_VERSION: &str = "6.49.53";
pub(crate) const AUDIO_UA: &str =
    "com.google.android.apps.youtube.music/6.49.53 (Linux; U; Android 14) gzip";

impl MusicClient {
    pub(crate) fn android_post(
        &self,
        endpoint: &str,
        body: Value,
        version: &str,
        sdk: u32,
    ) -> Result<Value> {
        let response = self
            .android_request(endpoint, body, version, sdk)?
            .send()
            .map_err(network)?;
        let value: Value = serde_json::from_str(&checked_response(response)?)
            .map_err(|_| Error::Protocol("Android Music API returned invalid JSON".into()))?;
        if value.get("error").is_some() {
            return Err(Error::Protocol(
                "Android Music API returned an error".into(),
            ));
        }
        Ok(value)
    }

    fn android_request(
        &self,
        endpoint: &str,
        mut body: Value,
        version: &str,
        sdk: u32,
    ) -> Result<reqwest::blocking::RequestBuilder> {
        let ua = format!(
            "com.google.android.apps.youtube.music/{version} (Linux; U; Android {}) gzip",
            if sdk == 34 { "14" } else { "16" }
        );
        let mut headers = HeaderMap::new();
        header(
            &mut headers,
            "authorization",
            &format!("Bearer {}", self.music_access_token()?),
        )?;
        header(&mut headers, "user-agent", &ua)?;
        header(&mut headers, "x-goog-api-format-version", "1")?;
        body["context"] = json!({"client":{"clientName":"ANDROID_MUSIC","clientVersion":version,
            "hl":self.config.language,"gl":self.config.country,"androidSdkVersion":sdk,
            "osName":"Android","osVersion":if sdk == 34 { "14" } else { "16" },"userAgent":ua},
            "user":{"lockedSafetyMode":false}});
        Ok(self
            .http
            .post(format!(
                "https://youtubei.googleapis.com/youtubei/v1/{endpoint}?prettyPrint=false"
            ))
            .headers(headers)
            .json(&body))
    }

    pub(crate) fn catalog_page(&self, value: &Value) -> Result<Page> {
        if self.is_android_music() {
            page(value)
        } else {
            parse::page(value)
        }
    }
}

fn native_item(v: &Value) -> Value {
    json!({"musicTwoRowItemRenderer":{
        "title":v["title"],"subtitle":v["subtitle"],
        "navigationEndpoint":v["onTap"]["innertubeCommand"],
        "thumbnail":{"thumbnails":v["thumbnail"]["image"]["sources"]},
        "badges":v["musicInlineBadges"]
    }})
}

fn normalize(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            if let Some(shelf) = m.get("itemSectionRenderer") {
                return json!({"musicShelfRenderer":{
                    "contents":normalize(&shelf["contents"]),
                    "continuations":shelf["continuations"],"header":shelf["header"]
                }});
            }
            if let Some(item) = m.get("musicTwoColumnItemRenderer") {
                return json!({"musicTwoRowItemRenderer":normalize(item)});
            }
            if let Some(item) = m.get("musicListItemWrapperModel") {
                return native_item(&item["musicListItemData"]);
            }
            if let Some(shelf) = m.get("musicTopResultCardShelfModel") {
                let data = &shelf["shelfData"];
                let mut items = vec![native_item(&data["musicTopResultCardHeaderData"])];
                if let Some(rows) = data["items"].as_array() {
                    items.extend(rows.iter().map(native_item));
                }
                return json!({"musicShelfRenderer":{"contents":items}});
            }
            if let Some(tab) = m.get("tabRenderer") {
                if tab["selected"] == false {
                    return Value::Null;
                }
            }
            Value::Object(
                m.iter()
                    .filter(|(k, _)| {
                        !matches!(
                            k.as_str(),
                            "menu"
                                | "menuCommand"
                                | "onLongPress"
                                | "loggingDirectives"
                                | "frameworkUpdates"
                                | "trackingParams"
                                | "clickTrackingParams"
                        )
                    })
                    .map(|(k, v)| (k.clone(), normalize(v)))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.iter().map(normalize).collect()),
        _ => v.clone(),
    }
}

pub(crate) fn page(value: &Value) -> Result<Page> {
    parse::page(&normalize(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_transport_scopes_credentials_and_skips_web_bootstrap() {
        let session = crate::music_oauth::MusicOAuthSession {
            access_token: "synthetic-native-access".into(),
            refresh_token: "synthetic-native-refresh".into(),
            expires_at: u64::MAX,
            client_id: "123-synthetic.apps.googleusercontent.com".into(),
        };
        let config = crate::Config {
            music_oauth: Some(session.clone()),
            ..Default::default()
        };
        let client = MusicClient::new(config.clone()).unwrap();
        let request = client
            .android_request(
                "browse",
                json!({"browseId":"FEmusic_liked_videos"}),
                VERSION,
                36,
            )
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(request.url().host_str(), Some("youtubei.googleapis.com"));
        assert_eq!(
            request.headers()["authorization"],
            "Bearer synthetic-native-access"
        );
        assert!(request.headers()["authorization"].is_sensitive());
        assert!(!request.headers().contains_key("cookie"));
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["context"]["client"]["clientName"], "ANDROID_MUSIC");
        let cdn = client
            .http
            .get("https://rr1.googlevideo.com/videoplayback")
            .build()
            .unwrap();
        assert!(!cdn.headers().contains_key("authorization"));
        assert!(!cdn.headers().contains_key("cookie"));
        assert!(MusicClient::new(crate::Config {
            cookie: Some("SAPISID=synthetic".into()),
            ..config.clone()
        })
        .is_err());
        assert!(MusicClient::new(crate::Config {
            auth_user: 1,
            ..config
        })
        .is_err());
        let encoded =
            serde_json::to_string(&crate::auth::Session::MusicOAuth(session.clone())).unwrap();
        let decoded: crate::auth::Session = serde_json::from_str(&encoded).unwrap();
        assert!(matches!(decoded, crate::auth::Session::MusicOAuth(s) if s == session));
    }
    #[test]
    fn native_cards_preserve_identity_and_do_not_turn_menus_into_results() {
        let v = json!({"contents":{"musicShelfRenderer":{"contents":[
            {"musicTwoColumnItemRenderer":{"title":{"simpleText":"Saved song"},"navigationEndpoint":{"watchEndpoint":{"videoId":"abcdefghijk"}},
            "menu":{"musicTwoColumnItemRenderer":{"title":{"simpleText":"Menu duplicate"}}}}}
        ],"continuations":[{"nextContinuationData":{"continuation":"next-native-page"}}]}}});
        let p = page(&v).unwrap();
        assert_eq!(p.sections[0].items.len(), 1);
        assert_eq!(
            p.sections[0].items[0].video_id.as_deref(),
            Some("abcdefghijk")
        );
        assert_eq!(
            p.sections[0].continuation.as_deref(),
            Some("next-native-page")
        );
    }
    #[test]
    fn element_list_models_keep_thumbnail_and_browse_endpoint() {
        let model = json!({
            "musicListItemWrapperModel":{"musicListItemData":{"title":"An artist","thumbnail":{"image":{"sources":[{"url":"https://example.test/art.jpg","width":100,"height":100}]}},
            "onTap":{"innertubeCommand":{"browseEndpoint":{"browseId":"UC_artist","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_ARTIST"}}}}}}}
        });
        let v = json!({"contents":{"elementRenderer":{"newElement":{"type":{"componentType":{"model":model}}}}}});
        let p = page(&v).unwrap();
        let i = &p.sections[0].items[0];
        assert_eq!(i.kind, "artist");
        assert_eq!(i.browse_id.as_deref(), Some("UC_artist"));
        assert_eq!(i.thumbnails.len(), 1);
    }
}
