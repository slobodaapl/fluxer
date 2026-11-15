//! Detached server search/pins and contiguous visible message-history windows.
use fluxer_core::{Channel, Message, Snowflake};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchScope {
    #[default]
    Current,
    AllDms,
    OpenDms,
    All,
    OpenDmsAndAllGuilds,
    AllGuilds,
}
impl SearchScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::AllDms => "all_dms",
            Self::OpenDms => "open_dms",
            Self::All => "all",
            Self::OpenDmsAndAllGuilds => "open_dms_and_all_guilds",
            Self::AllGuilds => "all_guilds",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchSort {
    #[default]
    Newest,
    Oldest,
    Relevant,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchStatus {
    Loading,
    Indexing,
    Ready,
    Failed,
}
#[derive(Clone, Debug)]
pub struct SearchSnapshot {
    pub channel_id: Snowflake,
    pub query: String,
    pub scope: SearchScope,
    pub sort: SearchSort,
    pub status: SearchStatus,
    pub messages: Vec<Message>,
    pub channels: Vec<Channel>,
    pub total: u64,
    pub page: u32,
    pub hits_per_page: u32,
    pub poll_count: u32,
    pub http_status: Option<u16>,
    pub error: Option<String>,
}
#[derive(Clone, Debug)]
pub struct PinnedMessage {
    pub message: Message,
    pub pinned_at: String,
}
#[derive(Clone, Debug)]
pub struct PinsSnapshot {
    pub channel_id: Snowflake,
    pub items: Vec<PinnedMessage>,
    pub loading: bool,
    pub fetched: bool,
    pub has_more: bool,
    pub can_manage: bool,
    pub error: Option<String>,
}
#[derive(Clone, Debug)]
pub struct HistorySnapshot {
    pub channel_id: Snowflake,
    /// Sorted oldest to newest; cached messages outside this contiguous window
    /// must not be rendered as if no gap existed.
    pub message_ids: Vec<Snowflake>,
    pub target: Option<Snowflake>,
    pub loading: bool,
    pub has_older: bool,
    pub has_newer: bool,
    pub latest: bool,
    pub error: Option<String>,
}

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct MentionFilters {pub everyone:bool,pub roles:bool,pub guilds:bool}
impl Default for MentionFilters {fn default()->Self{Self{everyone:true,roles:true,guilds:true}}}
#[derive(Clone,Debug,Default)]
pub struct MentionsSnapshot {
    pub account_id:Option<Snowflake>,
    pub messages:Vec<Message>,pub loading:bool,pub fetched:bool,pub has_more:bool,pub error:Option<String>,pub filters:MentionFilters,
}
