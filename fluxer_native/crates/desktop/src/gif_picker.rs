// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    ACCENT, BG, Fluxer, MUTED, PANEL, Pane, TextInput, button, controls::quiet_button,
    media_animation, muted, visible_image::VisibleImage,
};
use fluxer_client::{Command, GifFeed, GifResults, GifShare};
use fluxer_core::can_send_message;
use gpui::{prelude::*, *};
use std::time::Duration;

pub(super) struct GifPicker {
    pub(super) search: Entity<TextInput>,
    pub(super) open: bool,
    favorites: bool,
    request: u64,
    observed_query: String,
    feed: GifFeed,
    results: Option<Result<GifResults, String>>,
    debounce: Option<Task<()>>,
    scroll: ScrollHandle,
}

impl GifPicker {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            search: cx.new(|cx| TextInput::new("Search GIFs…", cx)),
            open: false,
            favorites: false,
            request: 0,
            observed_query: String::new(),
            feed: GifFeed::Featured,
            results: None,
            debounce: None,
            scroll: ScrollHandle::new(),
        }
    }

    pub(super) fn reset(&mut self) {
        self.open = false;
        self.favorites = false;
        self.debounce = None;
        self.results = None;
    }
}

impl Fluxer {
    pub(super) fn gif_picker_available(&self) -> bool {
        let Some(channel) = self
            .state
            .selected_channel
            .and_then(|id| self.state.channels.get(&id))
        else {
            return false;
        };
        !self.demo
            && self.editing.is_none()
            && self.state.current_user.as_ref().is_some_and(|user| {
                can_send_message(
                    channel.kind,
                    self.state.permissions_for(channel.id, user.id),
                )
            })
    }

    fn gif_picker_visible(&self) -> bool {
        self.gif_picker.open
            && self.gif_picker_available()
            && (self.pane == Pane::Chat || (self.pane == Pane::Call && self.viewer.chat_open))
    }

    pub(super) fn toggle_gif_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dismiss_gif_picker(window, cx) || !self.gif_picker_available() {
            return;
        }
        self.emoji_target = None;
        self.sticker_picker_open = false;
        self.forwarding = None;
        self.gif_picker
            .search
            .update(cx, |input, cx| input.set(String::new(), cx));
        self.gif_picker.observed_query.clear();
        self.gif_picker.open = true;
        self.load_gif_feed(GifFeed::Featured, false, cx);
        self.gif_picker.search.focus_handle(cx).focus(window);
    }

    pub(super) fn dismiss_gif_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.gif_picker.open {
            return false;
        }
        self.gif_picker.reset();
        self.composer.focus_handle(cx).focus(window);
        cx.notify();
        true
    }

    pub(super) fn gif_search_changed(&mut self, cx: &mut Context<Self>) {
        if !self.gif_picker.open {
            return;
        }
        let term: String = self
            .gif_picker
            .search
            .read(cx)
            .content
            .trim()
            .chars()
            .take(256)
            .collect();
        if self.gif_picker.observed_query == term {
            return;
        }
        self.gif_picker.observed_query.clone_from(&term);
        let feed = if term.is_empty() {
            GifFeed::Featured
        } else {
            GifFeed::Search(term)
        };
        if self.gif_picker.feed != feed {
            self.load_gif_feed(feed, true, cx);
        }
    }

    fn load_gif_feed(&mut self, feed: GifFeed, debounce: bool, cx: &mut Context<Self>) {
        self.gif_picker.favorites = false;
        self.gif_picker.debounce = None;
        self.gif_picker.request = self.gif_picker.request.wrapping_add(1);
        self.gif_picker.feed = feed.clone();
        self.gif_picker.results = None;
        self.gif_picker.scroll.set_offset(point(px(0.), px(0.)));
        let request = self.gif_picker.request;
        if debounce {
            self.gif_picker.debounce = Some(cx.spawn(async move |entity, cx| {
                Timer::after(Duration::from_millis(300)).await;
                let _ = entity.update(cx, |this, cx| {
                    if this.gif_picker.open && this.gif_picker.request == request {
                        this.command(Command::BrowseGifs { request, feed }, cx);
                    }
                });
            }));
        } else {
            self.command(Command::BrowseGifs { request, feed }, cx);
        }
        cx.notify();
    }

    pub(super) fn show_gif_results(&mut self, request: u64, result: Result<GifResults, String>) {
        if self.gif_picker.open && self.gif_picker.request == request {
            self.gif_picker.results = Some(result);
        }
    }

    fn pick_gif(
        &mut self,
        share: GifShare,
        insert: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.gif_picker_visible() {
            return;
        }
        let query = self.gif_picker.feed.query().to_owned();
        if insert {
            self.command(
                Command::RegisterGifShare {
                    share_id: share.id,
                    query,
                },
                cx,
            );
            self.composer.update(cx, |input, cx| {
                let space = if input.content.is_empty() { "" } else { " " };
                input.append_text(&format!("{space}{} ", share.url), window, cx);
            });
            return;
        }
        if self.message_pending() || self.slowmode_remaining() > 0 {
            return;
        }
        self.dismiss_gif_picker(window, cx);
        self.send_gif_optimistically(share, query, cx);
    }

    pub(super) fn gif_picker_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.gif_picker_visible() {
            return div().into_any_element();
        }
        let picker = &self.gif_picker;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .w_full()
            .max_w(px(430.))
            .h(px(264.))
            .rounded_lg()
            .border_1()
            .border_color(rgb(0x40394d))
            .bg(rgb(PANEL))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(picker.search.clone()))
                    .child(
                        quiet_button("gif-featured", "Featured").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.gif_picker
                                    .search
                                    .update(cx, |input, cx| input.set(String::new(), cx));
                                this.gif_picker.observed_query.clear();
                                this.load_gif_feed(GifFeed::Featured, false, cx);
                            },
                        )),
                    )
                    .child(
                        quiet_button("gif-trending", "Trending").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.gif_picker
                                    .search
                                    .update(cx, |input, cx| input.set(String::new(), cx));
                                this.gif_picker.observed_query.clear();
                                this.load_gif_feed(GifFeed::Trending, false, cx);
                            },
                        )),
                    )
                    .child(
                        quiet_button("gif-favorites", "Favorites").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.gif_picker.debounce = None;
                                this.gif_picker.request = this.gif_picker.request.wrapping_add(1);
                                this.gif_picker.favorites = true;
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        crate::controls::icon_button(
                            "gif-close",
                            crate::controls::Icon::Close,
                            "Close GIF picker",
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.dismiss_gif_picker(window, cx);
                        })),
                    ),
            )
            .child(if picker.favorites {
                self.gif_favorites(cx)
            } else {
                match &picker.results {
                    None => muted("Loading GIFs…").into_any_element(),
                    Some(Err(_)) => div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .child(muted("GIFs are unavailable."))
                        .child(button("gif-retry", "Retry").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.load_gif_feed(this.gif_picker.feed.clone(), false, cx);
                            },
                        )))
                        .into_any_element(),
                    Some(Ok(results)) => self.gif_results(results, cx),
                }
            })
            .into_any_element()
    }

    fn gif_favorites(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(entries) = fluxer_core::favorite_gifs(&self.state.user_settings) else {
            return muted("Favorites are unavailable.").into_any_element();
        };
        if entries.is_empty() {
            return muted("No favorite GIFs yet.").into_any_element();
        }
        let animate = !(self.image_viewer.is_open() || self.playback.modal.is_open());
        let autoplay = media_animation::effective_gif_autoplay(
            &self.state.user_settings,
            self.interface.reduced_motion(),
        );
        div()
            .id("gif-favorites-scroll")
            .track_scroll(&self.gif_picker.scroll)
            .overflow_y_scroll()
            .min_h_0()
            .flex_1()
            .child(div().flex().flex_wrap().gap_1().children(
                entries.into_iter().rev().enumerate().map(|(index, entry)| {
                    let url = entry.url.clone();
                    let preview = [
                        "tinywebp",
                        "tinygif",
                        "mediumwebp",
                        "mediumgif",
                        "webp",
                        "gif",
                        "nanowebp",
                        "nanogif",
                    ]
                    .iter()
                    .filter_map(|key| entry.media.get(*key).map(|f| f.proxy_src.as_str()))
                    .chain(
                        std::iter::once(entry.proxy_url.as_str())
                            .filter(|_| entry.content_type.starts_with("image/")),
                    )
                    .find_map(|source| media_animation::animated_proxy_url(source, false))
                    .map(SharedString::from);
                    div()
                        .id(("favorite-gif", index))
                        .focusable()
                        .tab_stop(true)
                        .cursor_pointer()
                        .size(px(96.))
                        .overflow_hidden()
                        .rounded_md()
                        .bg(rgb(BG))
                        .when_some(preview, |view, source| {
                            view.child(
                                VisibleImage::new(
                                    ("favorite-gif-preview", index),
                                    96.,
                                    96.,
                                    move || img(source.clone()).object_fit(ObjectFit::Cover),
                                )
                                .enabled(animate)
                                .autoplay(autoplay),
                            )
                        })
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            if !this.gif_picker_visible() {
                                return;
                            }
                            if event.modifiers().shift {
                                this.composer.update(cx, |input, cx| {
                                    let space = if input.content.is_empty() { "" } else { " " };
                                    input.append_text(&format!("{space}{url} "), window, cx);
                                });
                            } else if !this.message_pending() && this.slowmode_remaining() == 0 {
                                this.dismiss_gif_picker(window, cx);
                                this.send_favorite_gif_optimistically(url.clone(), cx);
                            }
                        }))
                }),
            ))
            .into_any_element()
    }

    fn gif_results(&self, results: &GifResults, cx: &mut Context<Self>) -> AnyElement {
        let picker = &self.gif_picker;
        let animate = !(self.image_viewer.is_open() || self.playback.modal.is_open());
        let autoplay = media_animation::effective_gif_autoplay(
            &self.state.user_settings,
            self.interface.reduced_motion(),
        );
        div()
            .flex()
            .flex_col()
            .gap_2()
            .min_h_0()
            .flex_1()
            .child(
                div()
                    .id("gif-scroll")
                    .track_scroll(&picker.scroll)
                    .overflow_y_scroll()
                    .min_h_0()
                    .flex_1()
                    .child(
                        div().flex().flex_wrap().gap_1().children(
                            results
                                .categories
                                .iter()
                                .enumerate()
                                .map(|(index, category)| {
                                    let query = category.name.clone();
                                    quiet_button(("gif-category", index), query.clone()).on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            this.gif_picker.search.update(cx, |input, cx| {
                                                input.set(query.clone(), cx)
                                            });
                                            this.gif_search_changed(cx);
                                        }),
                                    )
                                }),
                        ),
                    )
                    .when(
                        results.gifs.is_empty() && results.categories.is_empty(),
                        |view| view.child(muted("No GIFs found.")),
                    )
                    .child(div().flex().flex_wrap().gap_1().children(
                        results.gifs.iter().enumerate().map(|(index, gif)| {
                            let title = if gif.title.is_empty() {
                                "GIF"
                            } else {
                                &gif.title
                            };
                            let preview = gif
                                .image_preview()
                                .and_then(|source| {
                                    media_animation::animated_proxy_url(source, false)
                                })
                                .map(SharedString::from);
                            let share = gif.share();
                            let id = ("gif-preview", picker.request);
                            div()
                                .id(("gif-result", index))
                                .focusable()
                                .tab_stop(true)
                                .cursor_pointer()
                                .size(px(96.))
                                .flex()
                                .flex_col()
                                .overflow_hidden()
                                .rounded_md()
                                .bg(rgb(BG))
                                .border_1()
                                .border_color(transparent_black())
                                .hover(|view| view.bg(rgb(0x40394d)))
                                .focus(|view| view.border_color(rgb(ACCENT)))
                                .when(animate && !autoplay, |view| {
                                    view.on_hover(cx.listener(|_, _, _, cx| cx.notify()))
                                })
                                .when_some(preview, |view, source| {
                                    view.child(
                                        VisibleImage::new(id, 96., 74., move || {
                                            img(source.clone()).object_fit(ObjectFit::Cover)
                                        })
                                        .enabled(animate)
                                        .autoplay(autoplay),
                                    )
                                })
                                .child(
                                    div()
                                        .px_1()
                                        .text_size(px(11.))
                                        .truncate()
                                        .child(title.to_owned()),
                                )
                                .on_click(cx.listener(
                                    move |this, event: &ClickEvent, window, cx| {
                                        cx.stop_propagation();
                                        if let Some(share) = &share {
                                            this.pick_gif(
                                                share.clone(),
                                                event.modifiers().shift,
                                                window,
                                                cx,
                                            );
                                        }
                                    },
                                ))
                        }),
                    )),
            )
            .when(results.provider.attribution_required, |view| {
                view.child(
                    div()
                        .px_1()
                        .text_size(px(11.))
                        .text_color(rgb(MUTED))
                        .child(format!("Powered by {}", results.provider.display_name)),
                )
            })
            .into_any_element()
    }
}
