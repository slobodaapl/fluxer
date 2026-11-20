// SPDX-License-Identifier: AGPL-3.0-or-later
//! Explore the selected instance's community directory.
use super::*;
use fluxer_client::DiscoveryState;
use fluxer_protocol::DiscoverySearch;
use serde_json::Value;

pub(super) struct DiscoveryUi {
    input: Entity<TextInput>,
    language: Entity<TextInput>,
    pub available: bool,
    state: DiscoveryState,
    categories: Vec<Value>,
    join: DiscoveryJoin,
    join_generation: u64,
}
#[derive(Default)]
enum DiscoveryJoin {
    #[default]
    Idle,
    Confirm(Snowflake),
    Pending(Snowflake),
    Complete(Snowflake),
    Failed(Snowflake, String),
}
impl DiscoveryUi {
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        let input = cx.new(|cx| TextInput::new("Search communities", cx));
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        let language = cx.new(|cx| TextInput::new("Language code (optional)", cx));
        cx.observe(&language, |_, _, cx| cx.notify()).detach();
        Self {
            input,
            language,
            available: false,
            state: DiscoveryState::default(),
            categories: Vec::new(),
            join: DiscoveryJoin::Idle,
            join_generation: 0,
        }
    }
    pub fn reset(&mut self, cx: &mut Context<Fluxer>) {
        self.state.reset();
        self.join_generation = self.join_generation.wrapping_add(1);
        self.join = DiscoveryJoin::Idle;
        self.categories.clear();
        self.available = false;
        self.input
            .update(cx, |input, cx| input.set(String::new(), cx));
        self.language
            .update(cx, |input, cx| input.set(String::new(), cx));
    }
}
impl Fluxer {
    pub(super) fn search_discovery(&mut self, offset: u32, cx: &mut Context<Self>) {
        let params = DiscoverySearch {
            query: if offset == 0 {
                self.discovery_ui.input.read(cx).content.to_string()
            } else {
                self.discovery_ui.state.params.query.clone()
            },
            language: if offset == 0 {
                self.discovery_ui.language.read(cx).content.to_string()
            } else {
                self.discovery_ui.state.params.language.clone()
            },
            category: self.discovery_ui.state.params.category,
            sort_by: "member_count".into(),
            limit: 36,
            offset,
            ..Default::default()
        };
        let request = self.discovery_ui.state.begin(params.clone());
        self.command_if_live(Command::BrowseDiscovery { request, params }, cx);
        cx.notify();
    }
    pub(super) fn discovery_result(
        &mut self,
        request: u64,
        result: Result<(Value, Value), String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok((results, categories)) => {
                // Only the active response may replace category choices.
                if self.discovery_ui.state.is_current(request) {
                    self.discovery_ui.categories =
                        categories.as_array().cloned().unwrap_or_default();
                }
                self.discovery_ui.state.finish(request, Ok(results));
            }
            Err(error) => self.discovery_ui.state.finish(request, Err(error)),
        }
        cx.notify();
    }
    fn confirm_discovery_join(&mut self, guild_id: Snowflake, cx: &mut Context<Self>) {
        if !matches!(self.discovery_ui.join, DiscoveryJoin::Confirm(id) if id == guild_id) {
            return;
        }
        self.discovery_ui.join_generation = self.discovery_ui.join_generation.wrapping_add(1);
        let request = self.discovery_ui.join_generation;
        self.discovery_ui.join = DiscoveryJoin::Pending(guild_id);
        self.command_if_live(Command::JoinDiscovery { request, guild_id }, cx);
        cx.notify();
    }
    pub(super) fn discovery_join_result(
        &mut self,
        request: u64,
        guild_id: Snowflake,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        if request != self.discovery_ui.join_generation
            || !matches!(self.discovery_ui.join, DiscoveryJoin::Pending(id) if id == guild_id)
        {
            return;
        }
        self.discovery_ui.join = match result {
            Ok(()) => DiscoveryJoin::Complete(guild_id),
            Err(message) => DiscoveryJoin::Failed(guild_id, message),
        };
        cx.notify();
    }
    pub(super) fn discovery_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.discovery_ui.state;
        let mut filters = div().flex().flex_wrap().gap_2();
        let categories = std::iter::once((None, "All".to_owned())).chain(
            self.discovery_ui.categories.iter().filter_map(|category| {
                Some((
                    Some(category.get("id")?.as_u64()? as u32),
                    category.get("name")?.as_str()?.to_owned(),
                ))
            }),
        );
        for (category, name) in categories {
            let label = if category == state.params.category {
                format!("• {name}")
            } else {
                name
            };
            filters = filters.child(
                controls::quiet_button(
                    SharedString::from(format!(
                        "category-{}",
                        category.map_or("all".into(), |id| id.to_string())
                    )),
                    label,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.discovery_ui.state.params.category = category;
                    this.search_discovery(0, cx);
                })),
            );
        }
        let mut results = div()
            .id("discovery-results")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3();
        if state.loading {
            results = results.child("Loading communities…");
        }
        if let Some(error) = &state.error {
            results = results.child(format!("Could not load communities: {error}"));
        }
        if !state.loading && state.error.is_none() && state.guilds.is_empty() {
            results = results.child("No communities found.");
        }
        for guild in &state.guilds {
            let name = guild
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Community")
                .to_owned();
            let description = guild
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let members = guild
                .get("member_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let online = guild
                .get("online_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let id = guild
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| id.parse::<u64>().ok())
                .map(Snowflake);
            let member = id.is_some_and(|id| {
                self.state
                    .current_user
                    .as_ref()
                    .is_some_and(|user| self.state.members.contains_key(&(id, user.id)))
            });
            let mut actions = div().flex().flex_col().gap_2();
            if let Some(id) = id {
                actions = match &self.discovery_ui.join {
                    DiscoveryJoin::Confirm(selected) if *selected == id => {
                        actions.child("Join this community?").child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    controls::quiet_button(
                                        ("discovery-confirm-join", id.0),
                                        "Confirm join",
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| this.confirm_discovery_join(id, cx),
                                    )),
                                )
                                .child(
                                    controls::quiet_button(
                                        ("discovery-cancel-join", id.0),
                                        "Cancel",
                                    )
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.discovery_ui.join = DiscoveryJoin::Idle;
                                            cx.notify();
                                        },
                                    )),
                                ),
                        )
                    }
                    DiscoveryJoin::Pending(selected) if *selected == id => {
                        actions.child("Joining…")
                    }
                    DiscoveryJoin::Complete(selected) if *selected == id => {
                        actions.child("Joined. The community will appear in your sidebar.")
                    }
                    DiscoveryJoin::Failed(selected, message) if *selected == id => {
                        actions.child(format!("Could not join: {message}")).child(
                            controls::quiet_button(("discovery-retry-join", id.0), "Try again")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.discovery_ui.join = DiscoveryJoin::Confirm(id);
                                    cx.notify();
                                })),
                        )
                    }
                    _ if member => actions.child("Already a member"),
                    _ if matches!(self.discovery_ui.join, DiscoveryJoin::Pending(_)) => actions,
                    _ => actions.child(
                        controls::quiet_button(("discovery-join", id.0), "Join").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.discovery_ui.join = DiscoveryJoin::Confirm(id);
                                cx.notify();
                            }),
                        ),
                    ),
                };
            }
            results = results.child(
                div()
                    .p_4()
                    .rounded_md()
                    .bg(rgb(PANEL))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().font_weight(FontWeight::BOLD).child(name))
                    .child(description)
                    .child(
                        div()
                            .text_color(rgb(MUTED))
                            .child(format!("{members} members · {online} online")),
                    )
                    .child(actions),
            );
        }
        let has_more = state.loaded_count < state.total && !state.loading;
        let next = state.loaded_count.min(u32::MAX as u64) as u32;
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .p_5()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .child("Explore communities"),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(self.discovery_ui.input.clone())
                    .child(
                        controls::quiet_button("discovery-search", "Search")
                            .on_click(cx.listener(|this, _, _, cx| this.search_discovery(0, cx))),
                    ),
            )
            .child(self.discovery_ui.language.clone())
            .child(filters)
            .child(results)
            .when(has_more, |view| {
                view.child(
                    controls::quiet_button("discovery-more", "Load more").on_click(
                        cx.listener(move |this, _, _, cx| this.search_discovery(next, cx)),
                    ),
                )
            })
            .into_any_element()
    }
}
