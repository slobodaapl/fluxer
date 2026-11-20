// SPDX-License-Identifier: AGPL-3.0-or-later
//! Account-scoped directory search state. Responses from replaced requests are ignored.
use fluxer_protocol::DiscoverySearch;
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default)]
pub struct DiscoveryState {
    pub params: DiscoverySearch,
    pub guilds: Vec<Value>,
    pub total: u64,
    pub loaded_count: u64,
    pub loading: bool,
    pub error: Option<String>,
    generation: u64,
}
impl DiscoveryState {
    pub fn begin(&mut self, params: DiscoverySearch) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.params = params;
        self.loading = true;
        self.error = None;
        self.generation
    }
    pub fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }
    pub fn reset(&mut self) {
        let generation = self.generation.wrapping_add(1);
        *self = Self::default();
        self.generation = generation;
    }
    pub fn finish(&mut self, generation: u64, result: Result<Value, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match result {
            Err(error) => self.error = Some(error),
            Ok(body) => {
                let Some(guilds) = body.get("guilds").and_then(Value::as_array) else {
                    self.error = Some("Invalid discovery response".into());
                    return;
                };
                let Some(total) = body.get("total").and_then(Value::as_u64) else {
                    self.error = Some("Invalid discovery response".into());
                    return;
                };
                if self.params.offset == 0 {
                    self.guilds.clear();
                }
                let mut seen: BTreeSet<String> = self
                    .guilds
                    .iter()
                    .filter_map(|g| g.get("id")?.as_str().map(str::to_owned))
                    .collect();
                for guild in guilds {
                    if let Some(id) = guild.get("id").and_then(Value::as_str) {
                        if seen.insert(id.to_owned()) {
                            self.guilds.push(guild.clone());
                        }
                    }
                }
                self.loaded_count = self.params.offset as u64 + guilds.len() as u64;
                self.total = total;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn ignores_replaced_and_reset_requests_and_deduplicates_pages() {
        let mut state = DiscoveryState::default();
        let stale = state.begin(DiscoverySearch {
            limit: 36,
            ..Default::default()
        });
        let active = state.begin(DiscoverySearch {
            query: "games".into(),
            limit: 36,
            ..Default::default()
        });
        state.finish(stale, Ok(json!({"guilds":[{"id":"stale"}],"total":1})));
        assert!(state.loading);
        state.finish(
            active,
            Ok(json!({"guilds":[{"id":"1"},{"id":"2"}],"total":3})),
        );
        let next = state.begin(DiscoverySearch {
            offset: 2,
            limit: 36,
            ..state.params.clone()
        });
        state.finish(
            next,
            Ok(json!({"guilds":[{"id":"2"},{"id":"3"}],"total":3})),
        );
        assert_eq!(state.guilds.len(), 3);
        assert_eq!(state.loaded_count, 4);
        let pending = state.begin(state.params.clone());
        state.reset();
        state.finish(pending, Ok(json!({"guilds":[{"id":"old"}],"total":1})));
        assert!(state.guilds.is_empty());
    }
}
