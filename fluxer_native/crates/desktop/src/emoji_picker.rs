// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded expression popover: cached catalog results and visible grid rows only.
use super::*;
use std::{cell::Cell, rc::Rc, time::Instant};

pub(super) const COLUMNS: usize = 8;
const CELL: f32 = 40.;

pub(super) struct EmojiPickerState {
    catalog: Option<chat_helpers::EmojiCatalog>,
    custom_guild: Option<Snowflake>,
    custom_emojis: Option<serde_json::Value>,
    pub choices: Arc<Vec<Arc<chat_helpers::EmojiChoice>>>,
    tone: Option<String>,
    hovered: Option<usize>,
    anchor: Point<Pixels>,
    opened: Instant,
    prepared_ms: f64,
    perf: bool,
    painted: Rc<Cell<bool>>,
    visible_cells: Rc<Cell<usize>>,
}
impl EmojiPickerState {
    pub fn new() -> Self {
        Self {
            catalog: None,
            custom_guild: None,
            custom_emojis: None,
            choices: Arc::new(Vec::new()),
            tone: None,
            hovered: None,
            anchor: point(px(0.), px(0.)),
            opened: Instant::now(),
            prepared_ms: 0.,
            perf: std::env::var_os("FLUXER_EMOJI_PERF").is_some(),
            painted: Rc::new(Cell::new(false)),
            visible_cells: Rc::new(Cell::new(0)),
        }
    }
}

impl Fluxer {
    pub(super) fn open_emoji(
        &mut self,
        target: EmojiTarget,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.emoji_target == Some(target) {
            self.emoji_target = None;
        } else {
            self.emoji_picker.opened = Instant::now();
            self.emoji_picker.painted.set(false);
            self.emoji_picker.visible_cells.set(0);
            self.emoji_picker.anchor = anchor;
            self.rebuild_emoji_catalog();
            self.emoji_target = Some(target);
            self.forwarding = None;
            self.emoji_search
                .update(cx, |input, cx| input.set(String::new(), cx));
            self.refresh_emoji_results(cx);
            self.emoji_search.focus_handle(cx).focus(window);
        }
        cx.notify();
    }

    pub(super) fn refresh_emoji_results(&mut self, cx: &mut Context<Self>) {
        let query = self.emoji_search.read(cx).content.to_string();
        if let Some(catalog) = &self.emoji_picker.catalog {
            self.emoji_picker.choices = Arc::new(catalog.search_with_tone(
                &query,
                &self.emoji_category,
                self.emoji_picker.tone.as_deref(),
            ));
        }
        self.emoji_index = 0;
        self.emoji_picker.hovered = None;
        self.emoji_scroll.scroll_to_item(0, ScrollStrategy::Top);
        if !self.emoji_picker.painted.get() {
            self.emoji_picker.prepared_ms =
                self.emoji_picker.opened.elapsed().as_secs_f64() * 1000.;
        }
        cx.notify();
    }

    fn rebuild_emoji_catalog(&mut self) {
        let (guild, emojis) = custom_emoji_snapshot(&self.state, self.state.selected_channel);
        self.emoji_picker.custom_guild = guild;
        self.emoji_picker.custom_emojis = emojis.cloned();
        self.emoji_picker.catalog = Some(chat_helpers::EmojiCatalog::new(
            &self.state,
            self.state.selected_channel,
        ));
    }

    pub(super) fn refresh_open_emoji_catalog(&mut self, cx: &mut Context<Self>) {
        if self.emoji_target.is_none() {
            return;
        }
        let (guild, emojis) = custom_emoji_snapshot(&self.state, self.state.selected_channel);
        if guild != self.emoji_picker.custom_guild
            || emojis != self.emoji_picker.custom_emojis.as_ref()
        {
            self.rebuild_emoji_catalog();
            self.refresh_emoji_results(cx);
        }
    }

    pub(super) fn schedule_emoji_demo(&self, cx: &mut Context<Self>) {
        cx.defer(|cx| {
            if let Some(window) = cx.windows().first().copied() {
                let _ = window.update(cx, |root, window, cx| {
                    if let Ok(entity) = root.downcast::<Fluxer>() {
                        entity.update(cx, |this, cx| {
                            let anchor = point(
                                window.viewport_size().width - px(32.),
                                window.viewport_size().height - px(94.),
                            );
                            let target = this
                                .state
                                .selected_channel
                                .and_then(|channel| {
                                    this.state
                                        .channel_messages(channel)
                                        .next()
                                        .map(|message| EmojiTarget::Reaction(message.id))
                                })
                                .unwrap_or(EmojiTarget::Composer);
                            this.open_emoji(target, anchor, window, cx);
                        });
                    }
                });
            }
        });
    }

    pub(super) fn emoji_panel(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.emoji_picker.choices.len().div_ceil(COLUMNS);
        let preview = self
            .emoji_picker
            .choices
            .get(self.emoji_picker.hovered.unwrap_or(self.emoji_index));
        let anchor = self.emoji_picker.anchor;
        let origin = picker_origin(
            anchor,
            self.emoji_target == Some(EmojiTarget::Composer),
            window.viewport_size(),
        );
        let choices = self.emoji_picker.choices.clone();
        let perf = self.emoji_picker.perf;
        let opened = self.emoji_picker.opened;
        let prepared_ms = self.emoji_picker.prepared_ms;
        let painted = self.emoji_picker.painted.clone();
        let visible = self.emoji_picker.visible_cells.clone();
        let count = choices.len();
        let popup = div().id("emoji-popover").w(px(410.)).h(px(456.).min(window.viewport_size().height-px(24.)))
            .occlude().flex().flex_col().overflow_hidden().rounded_lg().border_1().border_color(rgb(0x514861))
            .bg(rgb(PANEL)).shadow_lg()
            .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                // Let the trigger's own click toggle the picker, instead of
                // closing on mouse-down and reopening on click.
                if (event.position.x-anchor.x).abs() > px(24.) || (event.position.y-anchor.y).abs() > px(24.) {
                    this.emoji_target = None; cx.notify();
                }
            }))
            .child(div().h(px(34.)).px_3().flex().items_center().justify_between()
                .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child(if matches!(self.emoji_target,Some(EmojiTarget::Reaction(_))) { "Add a reaction" } else { "Emoji" }))
                .child(button("close-emoji", "×").px_2().py_0().on_click(cx.listener(|this, _, window, cx| {
                    this.emoji_target = None; this.composer.focus_handle(cx).focus(window); cx.notify();
                }))))
            .child(div().flex_1().min_h_0().flex()
                .child(div().id("emoji-category-rail").w(px(44.)).flex_shrink_0().overflow_y_scroll().py_1().flex().flex_col().items_center().gap_1().bg(rgb(0x1c1925))
                    .children(chat_helpers::categories().into_iter().enumerate().map(|(index, category)| {
                        button(("emoji-category",index),category_glyph(category)).size(px(34.)).p_0().flex().items_center().justify_center().bg(rgb(0x1c1925)).text_size(px(20.)).line_height(relative(1.))
                            .when(self.emoji_category == category, |view| view.bg(rgb(0x56438b)))
                            .tooltip(move |_, cx| cx.new(|_| PickerTooltip(category_label(category))).into())
                            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                if *hovered && this.emoji_picker.hovered.is_some() { this.emoji_picker.hovered = None; cx.notify(); }
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.emoji_category = category.into(); this.refresh_emoji_results(cx);
                            }))
                    })))
                .child(div().flex_1().min_w_0().min_h_0().flex().flex_col().px_2()
                    .child(self.emoji_search.clone())
                    .child(div().h(px(30.)).flex_shrink_0().flex().items_center().gap_1()
                        .child(div().mr_1().text_size(px(11.)).text_color(rgb(MUTED)).child("Tone"))
                        .children(["", "🏻", "🏼", "🏽", "🏾", "🏿"].into_iter().enumerate().map(|(index,tone)| {
                            button(("emoji-tone",index),format!("✋{tone}")).tooltip(move |_, cx| cx.new(|_| PickerTooltip(["Default tone", "Light tone", "Medium light tone", "Medium tone", "Medium dark tone", "Dark tone"][index])).into()).size(px(28.)).p_0().flex().items_center().justify_center().bg(rgb(PANEL)).text_size(px(20.)).line_height(relative(1.))
                                .when(self.emoji_picker.tone.as_deref().unwrap_or("") == tone, |view| view.bg(rgb(0x56438b)))
                                .on_click(cx.listener(move |this, _, _, cx| { this.emoji_picker.tone = (!tone.is_empty()).then(||tone.to_owned()); this.refresh_emoji_results(cx); }))
                        })))
                    .child(div().h(px(24.)).flex_shrink_0().text_size(px(11.)).text_color(rgb(MUTED)).font_weight(FontWeight::SEMIBOLD)
                        .child(format!("{} · {}", if self.emoji_search.read(cx).content.is_empty() { category_label(&self.emoji_category) } else { "Search results" }, count)))
                    .child(uniform_list("emoji-virtual-grid", rows, cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        let visible = range.len()*COLUMNS;
                        this.emoji_picker.visible_cells.set(this.emoji_picker.visible_cells.get().max(visible));
                        range.map(|row| {
                            div().id(("emoji-row",row)).h(px(CELL)).flex().items_center()
                                .children((row*COLUMNS..((row+1)*COLUMNS).min(this.emoji_picker.choices.len())).map(|index| {
                                    let choice = this.emoji_picker.choices[index].clone();
                                    let mut tile = div().id(SharedString::from(choice.key.clone())).size(px(CELL)).flex_shrink_0().flex().items_center().justify_center().rounded_md().cursor_pointer().text_size(px(28.)).line_height(relative(1.))
                                        .when(index == this.emoji_index, |view| view.bg(rgb(0x44365e)))
                                        .hover(|style| style.bg(rgb(0x504063)));
                                    tile = tile.child(this.emoji_art(&choice,28.));
                                    tile.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                        if *hovered && this.emoji_picker.hovered != Some(index) { this.emoji_picker.hovered=Some(index); cx.notify(); }
                                    })).on_click(cx.listener(move |this, _, window, cx| this.pick_emoji(&choice,window,cx)))
                                }))
                        }).collect::<Vec<_>>()
                    })).track_scroll(self.emoji_scroll.clone()).flex_1().min_h_0().w_full())
                    .when(count == 0, |view| view.child(div().h(px(48.)).flex().items_center().justify_center().text_color(rgb(MUTED)).child("No matching emoji")))))
            .child(div().h(px(52.)).flex_shrink_0().border_t_1().border_color(rgb(0x403749)).px_3().flex().items_center().gap_3()
                .when_some(preview, |view, choice| view.child(self.emoji_art(choice,32.)).child(div().flex_1().min_w_0().flex().flex_col()
                    .child(div().text_size(px(13.)).text_ellipsis().child(format!(":{}:",choice.name)))
                    .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child(if choice.animated { "Animated community emoji" } else if choice.id.is_some() { "Community emoji" } else { category_label(&choice.category) })))))
            .child(canvas(|_,_,_|{}, move |_,_,_,_| {
                if perf && !painted.replace(true) { eprintln!("emoji_perf first_paint_ms={:.3} prepare_ms={prepared_ms:.3} cached_choices={count} max_visible_cells={}",opened.elapsed().as_secs_f64()*1000.,visible.get()); }
            }).absolute().size_full());
        deferred(
            div().absolute().top_0().left_0().child(
                anchored()
                    .anchor(Corner::TopLeft)
                    .position(origin)
                    .snap_to_window_with_margin(px(12.))
                    .child(popup),
            ),
        )
        .into_any_element()
    }

    fn emoji_art(&self, choice: &chat_helpers::EmojiChoice, size: f32) -> AnyElement {
        if let Some(id) = choice.id {
            if let Some(endpoint) = self.media_endpoint.as_deref() {
                return img(SharedString::from(format!(
                    "{}/emojis/{id}.webp?size=48",
                    endpoint.trim_end_matches('/')
                )))
                .size(px(size))
                .object_fit(ObjectFit::Contain)
                .into_any_element();
            }
            return div()
                .size(px(size))
                .rounded_md()
                .bg(rgb(0x514174))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(14.))
                .child(choice.name.chars().take(2).collect::<String>())
                .into_any_element();
        }
        div()
            .size(px(size + 4.))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(size))
            .child(choice.label.clone())
            .into_any_element()
    }
}

fn category_glyph(category: &str) -> &'static str {
    match category {
        "all" => "⌕",
        "community" => "◇",
        "people" => "☺",
        "nature" => "☘",
        "food" => "🍎",
        "activity" => "⚽",
        "travel" => "🚀",
        "objects" => "💡",
        "symbols" => "♥",
        "flags" => "⚑",
        _ => "·",
    }
}
fn category_label(category: &str) -> &'static str {
    match category {
        "all" => "All emoji",
        "community" => "Community",
        "people" => "Smileys & people",
        "nature" => "Animals & nature",
        "food" => "Food & drink",
        "activity" => "Activities",
        "travel" => "Travel & places",
        "objects" => "Objects",
        "symbols" => "Symbols",
        "flags" => "Flags",
        _ => "Emoji",
    }
}

struct PickerTooltip(&'static str);
impl Render for PickerTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(0x15121c))
            .text_color(rgb(TEXT))
            .text_size(px(12.))
            .child(self.0)
    }
}

fn picker_origin(anchor: Point<Pixels>, composer: bool, viewport: Size<Pixels>) -> Point<Pixels> {
    let width = px(410.);
    let height = px(456.).min(viewport.height - px(24.));
    let left = if composer {
        anchor.x - width + px(20.)
    } else {
        anchor.x - px(24.)
    };
    let below = anchor.y + px(20.);
    let top = if !composer && below + height <= viewport.height - px(12.) {
        below
    } else {
        anchor.y - height - px(8.)
    };
    point(
        left.max(px(12.)).min(viewport.width - width - px(12.)),
        top.max(px(12.)).min(viewport.height - height - px(12.)),
    )
}

fn custom_emoji_snapshot(
    state: &ClientState,
    channel: Option<Snowflake>,
) -> (Option<Snowflake>, Option<&serde_json::Value>) {
    let guild = channel
        .and_then(|id| state.channels.get(&id))
        .and_then(|channel| channel.guild_id);
    (
        guild,
        guild
            .and_then(|id| state.guilds.get(&id))
            .and_then(|guild| guild.extra.get("emojis")),
    )
}

#[cfg(test)]
mod tests {
    use super::picker_origin;
    use gpui::{point, px, size};
    #[test]
    fn reaction_popup_stays_beside_trigger_and_flips_above_near_bottom() {
        let viewport = size(px(1280.), px(820.));
        assert_eq!(
            picker_origin(point(px(420.), px(209.)), false, viewport),
            point(px(396.), px(229.))
        );
        let above = picker_origin(point(px(1000.), px(750.)), false, viewport);
        assert!(above.y + px(456.) < px(750.));
        assert!(above.x + px(410.) <= viewport.width - px(12.));
    }
    #[test]
    fn composer_popup_fits_minimum_window_and_stays_above_composer() {
        let viewport = size(px(880.), px(600.));
        let origin = picker_origin(point(px(846.), px(540.)), true, viewport);
        assert!(origin.x >= px(12.) && origin.y >= px(12.));
        assert!(origin.x + px(410.) <= viewport.width - px(12.));
        assert!(origin.y + px(456.) <= px(540.));
    }
}
