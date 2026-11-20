// SPDX-License-Identifier: AGPL-3.0-or-later
//! Discovery directory requests mirror DiscoveryCommands and stay on the selected instance.
use crate::{Client, Result};
use reqwest::Method;
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscoverySearch {
    pub query: String,
    pub category: Option<u32>,
    pub language: String,
    pub tag: String,
    pub sort_by: String,
    pub limit: u32,
    pub offset: u32,
}
impl DiscoverySearch {
    fn path(&self) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("limit", &self.limit.to_string());
        query.append_pair("offset", &self.offset.to_string());
        if !self.query.is_empty() {
            query.append_pair("query", &self.query);
        }
        if let Some(category) = self.category {
            query.append_pair("category", &category.to_string());
        }
        if !self.language.is_empty() {
            query.append_pair("language", &self.language);
        }
        if !self.tag.is_empty() {
            query.append_pair("tag", &self.tag);
        }
        if !self.sort_by.is_empty() {
            query.append_pair("sort_by", &self.sort_by);
        }
        format!("/discovery/guilds?{}", query.finish())
    }
}
impl Client {
    pub async fn discovery_search(&self, params: &DiscoverySearch) -> Result<Value> {
        self.request(Method::GET, &params.path(), None).await
    }
    /// Direct join is a separate, explicit action; the server enforces discovery
    /// eligibility, account restrictions, bans and membership limits.
    pub async fn discovery_join(&self, guild: fluxer_core::Snowflake) -> Result<()> {
        self.request(
            Method::POST,
            &format!("/discovery/guilds/{}/join", guild.0),
            None,
        )
        .await?;
        Ok(())
    }
    pub async fn discovery_categories(&self) -> Result<Value> {
        self.request(Method::GET, "/discovery/categories", None)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_encodes_filters_and_keeps_zero_category() {
        let params = DiscoverySearch {
            query: "C++ & games".into(),
            category: Some(0),
            language: "en-US".into(),
            tag: "a/b".into(),
            sort_by: "member_count".into(),
            limit: 36,
            offset: 72,
        };
        let url = url::Url::parse(&format!("https://example.test{}", params.path())).unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["query"], "C++ & games");
        assert_eq!(pairs["category"], "0");
        assert_eq!(pairs["tag"], "a/b");
        assert_eq!(pairs["offset"], "72");
        assert_eq!(pairs["sort_by"], "member_count");
    }
}
