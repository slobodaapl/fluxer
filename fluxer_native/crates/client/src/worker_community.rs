//! Community mutations follow GuildCommands; gateway events own the roster.
use super::*;
use fluxer_protocol::Method;

impl Worker {
    pub(super) async fn create_community(&mut self, name: String) {
        let name = name.trim();
        let result = async {
            if name.is_empty() || name.chars().count() > 100 {
                return Err("Community name must be 1–100 characters".to_string());
            }
            if self.demo {
                return Err("Community creation is unavailable in the offline demo".to_string());
            }
            let client = self.client.as_ref().ok_or("Not signed in")?;
            let value = client
                .request(Method::POST, "/guilds", Some(&json!({"name": name})))
                .await
                .map_err(error)?;
            let id: Snowflake =
                serde_json::from_value(value.get("id").cloned().ok_or("Missing community ID")?)
                    .map_err(error)?;
            Ok(id)
        }
        .await;
        self.emit(Event::CommunityMutation {
            created: result.as_ref().ok().copied(),
            left: None,
            error: result.err(),
        });
    }
    pub(super) async fn leave_community(&mut self, guild_id: Snowflake, delete_messages: bool) {
        let result = async {
            if self.demo {
                return Err("Leaving communities is unavailable in the offline demo".to_string());
            }
            let guild = self
                .state
                .guilds
                .get(&guild_id)
                .ok_or("Community no longer available")?;
            if self
                .state
                .current_user
                .as_ref()
                .is_some_and(|u| u.id == guild.owner_id)
            {
                return Err("Transfer ownership before leaving this community".to_string());
            }
            let client = self.client.as_ref().ok_or("Not signed in")?;
            let path = format!(
                "/users/@me/guilds/{guild_id}{}",
                if delete_messages {
                    "?delete_messages=true"
                } else {
                    ""
                }
            );
            client
                .request(Method::DELETE, &path, None)
                .await
                .map_err(error)?;
            Ok(())
        }
        .await;
        if result.is_ok() && self.selected_guild == Some(guild_id) {
            self.selected_guild = None;
            self.persist_selection().await;
        }
        self.emit(Event::CommunityMutation {
            created: None,
            left: result.as_ref().ok().map(|_| guild_id),
            error: result.err(),
        });
    }
}
