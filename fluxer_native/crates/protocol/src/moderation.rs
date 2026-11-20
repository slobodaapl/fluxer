// SPDX-License-Identifier: AGPL-3.0-or-later
//! GuildMemberCommands and GuildCommands moderation wire contracts.
use crate::{Client, Result, http::segment};
use reqwest::Method;
use serde_json::{Value, json};

impl Client {
    /// Kick without an audit reason (GuildMemberCommands.kick).
    pub async fn kick_member(&self, guild: &str, user: &str) -> Result<()> {
        self.request(Method::DELETE, &member_path(guild, user), None)
            .await?;
        Ok(())
    }

    /// Permanently ban, preserving message history (GuildCommands.banMember defaults).
    pub async fn ban_member(&self, guild: &str, user: &str) -> Result<()> {
        self.request(
            Method::PUT,
            &format!("/guilds/{}/bans/{}", segment(guild), segment(user)),
            Some(&json!({"delete_message_seconds": 0, "reason": null})),
        )
        .await?;
        Ok(())
    }

    /// Set or clear the timeout; returns the updated guild member wire object.
    pub async fn set_member_timeout(
        &self,
        guild: &str,
        user: &str,
        until: Option<&str>,
    ) -> Result<Value> {
        self.request(
            Method::PATCH,
            &member_path(guild, user),
            Some(&json!({"communication_disabled_until": until})),
        )
        .await
    }
}
fn member_path(guild: &str, user: &str) -> String {
    format!("/guilds/{}/members/{}", segment(guild), segment(user))
}
