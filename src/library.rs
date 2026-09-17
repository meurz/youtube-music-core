//! Authenticated, read-only Music library access. Pagination is explicit.
use crate::{
    auth::explicitly_signed_out, client::nonempty, model::Page, parse, Error, MusicClient, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, clap::ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LibrarySection {
    Playlists,
    Songs,
    Albums,
    Artists,
    Subscriptions,
    Likes,
}

impl LibrarySection {
    pub(crate) fn browse_id(self) -> &'static str {
        match self {
            Self::Playlists => "FEmusic_liked_playlists",
            Self::Songs => "FEmusic_liked_videos",
            Self::Albums => "FEmusic_liked_albums",
            Self::Artists => "FEmusic_library_corpus_track_artists",
            Self::Subscriptions => "FEmusic_library_corpus_artists",
            Self::Likes => "VLLM",
        }
    }
}

impl MusicClient {
    /// Includes both owned and saved playlists. Each call verifies the account,
    /// so a rejected session cannot become an apparently empty library.
    pub fn library(&self, section: LibrarySection, continuation: Option<&str>) -> Result<Page> {
        crate::operation::ensure(|| {
            if let Some(token) = continuation {
                nonempty(token, "continuation")?;
            }
            self.account()?;
            let mut body = json!({"browseId":section.browse_id()});
            if let Some(token) = continuation {
                body["continuation"] = token.into();
            }
            let value = self.post("browse", body).map_err(|e| {
                if matches!(e, Error::Http(401) | Error::Http(403)) {
                    Error::AuthenticationRejected
                } else {
                    e
                }
            })?;
            if explicitly_signed_out(&value) {
                return Err(Error::AuthenticationRejected);
            }
            parse_library_page(&value)
        })
    }
}

fn parse_library_page(value: &serde_json::Value) -> Result<Page> {
    let mut page = parse::page(value)?;
    if page.sections.is_empty() && parse::find(value, "messageRenderer").is_none() {
        return Err(Error::Protocol("unrecognized library layout".into()));
    }
    // Remove action tiles (e.g. "New playlist") without dropping the first real item.
    for group in &mut page.sections {
        group
            .items
            .retain(|item| item.video_id.is_some() || item.browse_id.is_some());
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_library_is_distinct_from_unknown_layout_and_action_tiles() {
        assert!(parse_library_page(
            &json!({"contents":{"messageRenderer":{"text":{"simpleText":"No albums yet"}}}})
        )
        .unwrap()
        .sections
        .is_empty());
        assert!(parse_library_page(&json!({"contents":{"newUnknownRenderer":{}}})).is_err());
        let page = parse_library_page(&json!({"contents":{"gridRenderer":{"items":[
            {"musicTwoRowItemRenderer":{"title":{"simpleText":"New playlist"}}},
            {"musicTwoRowItemRenderer":{"title":{"simpleText":"First playlist"},"navigationEndpoint":{"browseEndpoint":{"browseId":"VLPL_FIRST"}}}}
        ]}}})).unwrap();
        assert_eq!(page.sections[0].items.len(), 1);
        assert_eq!(
            page.sections[0].items[0].browse_id.as_deref(),
            Some("VLPL_FIRST")
        );
    }

    #[test]
    fn liked_songs_and_saved_songs_are_distinct() {
        assert_eq!(LibrarySection::Likes.browse_id(), "VLLM");
        assert_eq!(LibrarySection::Songs.browse_id(), "FEmusic_liked_videos");
        assert_eq!(
            LibrarySection::Playlists.browse_id(),
            "FEmusic_liked_playlists"
        );
    }
    #[test]
    fn library_grid_pagination_keeps_the_first_playlist() {
        let page = parse::page(&json!({"continuationContents":{"gridContinuation":{"items":[
            {"musicTwoRowItemRenderer":{"title":{"simpleText":"First playlist"},"navigationEndpoint":{"browseEndpoint":{"browseId":"VLPL_FIRST","browseEndpointContextSupportedConfigs":{"browseEndpointContextMusicConfig":{"pageType":"MUSIC_PAGE_TYPE_PLAYLIST"}}}}}}
        ],"continuations":[{"nextContinuationData":{"continuation":"next-library-page"}}]}}})).unwrap();
        assert_eq!(page.sections.len(), 1);
        assert_eq!(
            page.sections[0].items[0].browse_id.as_deref(),
            Some("VLPL_FIRST")
        );
        assert_eq!(
            page.sections[0].continuation.as_deref(),
            Some("next-library-page")
        );
    }
}
