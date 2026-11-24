// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    ACCENT, BG, Fluxer, MUTED, PANEL, Pane, TEXT, TextInput, button,
    controls::{composer_popover, quiet_button, tooltip},
    media_animation, muted,
    visible_image::VisibleImage,
};
use fluxer_client::{Command, GifFeed, GifResults, GifShare};
use fluxer_core::{FavoriteGif, can_send_message};
use gpui::{prelude::*, *};
use std::{cell::Cell, rc::Rc, time::Duration};

pub(super) struct GifPicker {
    pub(super) search: Entity<TextInput>,
    open: bool,
    button_bounds: Rc<Cell<Bounds<Pixels>>>,
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
            button_bounds: Rc::default(),
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

    pub(super) fn gif_picker_visible(&self) -> bool {
        self.gif_picker.open
            && self.gif_picker_available()
            && (self.pane == Pane::Chat || (self.pane == Pane::Call && self.viewer.chat_open))
    }

    pub(super) fn gif_picker_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let bounds = self.gif_picker.button_bounds.clone();
        quiet_button("composer-gif", "GIF")
            .relative()
            .child(
                canvas(move |rect, _, _| bounds.set(rect), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .on_click(cx.listener(|this, _, window, cx| this.toggle_gif_picker(window, cx)))
    }

    fn toggle_gif_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn gif_search_changed(&mut self, debounce: bool, cx: &mut Context<Self>) {
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
        if self.gif_picker.favorites || self.gif_picker.feed != feed {
            self.load_gif_feed(feed, debounce, cx);
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

    pub(super) fn gif_picker_popover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let picker = &self.gif_picker;
        let popup = div()
            .id("gif-popover")
            .occlude()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .overflow_hidden()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0x514861))
            .bg(rgb(PANEL))
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                // Let the GIF button handle its own toggle on click.
                if !this
                    .gif_picker
                    .button_bounds
                    .get()
                    .contains(&event.position)
                    && !this.gif_picker.search.read(cx).context_menu_open(cx)
                {
                    this.dismiss_gif_picker(window, cx);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(picker.search.clone()))
                    .child(
                        gif_tab(
                            "gif-featured",
                            "Featured",
                            !picker.favorites && picker.feed == GifFeed::Featured,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.gif_picker
                                .search
                                .update(cx, |input, cx| input.set(String::new(), cx));
                            this.gif_picker.observed_query.clear();
                            this.load_gif_feed(GifFeed::Featured, false, cx);
                        })),
                    )
                    .child(
                        gif_tab(
                            "gif-trending",
                            "Trending",
                            !picker.favorites && picker.feed == GifFeed::Trending,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.gif_picker
                                .search
                                .update(cx, |input, cx| input.set(String::new(), cx));
                            this.gif_picker.observed_query.clear();
                            this.load_gif_feed(GifFeed::Trending, false, cx);
                        })),
                    )
                    .child(
                        gif_tab("gif-favorites", "Favorites", picker.favorites).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.gif_picker
                                    .search
                                    .update(cx, |input, cx| input.set(String::new(), cx));
                                this.gif_picker.observed_query.clear();
                                this.gif_picker.debounce = None;
                                this.gif_picker.request = this.gif_picker.request.wrapping_add(1);
                                this.gif_picker.favorites = true;
                                this.gif_picker.scroll.set_offset(point(px(0.), px(0.)));
                                cx.notify();
                            }),
                        ),
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
            });
        composer_popover(popup, size(px(430.), px(324.)))
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
                    let remove_entry = entry.clone();
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
                        .relative()
                        .size(px(96.))
                        .overflow_hidden()
                        .rounded_md()
                        .bg(rgb(BG))
                        .border_1()
                        .border_color(transparent_black())
                        .hover(|view| view.border_color(rgb(0x625474)))
                        .focus(|view| view.border_color(rgb(ACCENT)))
                        .when_some(preview, |view, source| {
                            view.child(
                                VisibleImage::new(
                                    media_animation::image_id("favorite-gif-preview", &source),
                                    96.,
                                    96.,
                                    move || img(source.clone()).object_fit(ObjectFit::Cover),
                                )
                                .enabled(animate)
                                .autoplay(autoplay),
                            )
                        })
                        .child(gif_favorite_button(remove_entry, true, cx))
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
                    .child(if picker.feed == GifFeed::Featured {
                        self.featured_gif_tiles(results, cx).into_any_element()
                    } else if results.gifs.is_empty() {
                        muted("No GIFs found.").into_any_element()
                    } else {
                        self.gif_result_tiles(results, cx).into_any_element()
                    }),
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

    fn featured_gif_tiles(&self, results: &GifResults, cx: &mut Context<Self>) -> Div {
        let trending_preview = results.gifs.first().and_then(|gif| gif.image_preview());
        div()
            .grid()
            .grid_cols(2)
            .gap_2()
            .child(
                gif_browse_tile("gif-trending-preview", "Trending GIFs", trending_preview)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.load_gif_feed(GifFeed::Trending, false, cx);
                    })),
            )
            .children(
                results
                    .categories
                    .iter()
                    .enumerate()
                    .map(|(index, category)| {
                        let query = category.name.clone();
                        gif_browse_tile(
                            ("gif-category", index),
                            query.clone(),
                            category.image_preview(),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.gif_picker
                                    .search
                                    .update(cx, |input, cx| input.set(query.clone(), cx));
                                this.gif_search_changed(false, cx);
                                this.gif_picker.search.focus_handle(cx).focus(window);
                            },
                        ))
                    }),
            )
    }

    fn gif_result_tiles(&self, results: &GifResults, cx: &mut Context<Self>) -> Div {
        let animate = !(self.image_viewer.is_open() || self.playback.modal.is_open());
        let autoplay = media_animation::effective_gif_autoplay(
            &self.state.user_settings,
            self.interface.reduced_motion(),
        );
        let favorites = fluxer_core::favorite_gifs(&self.state.user_settings);
        let can_add_favorite =
            fluxer_core::favorite_gifs_saved_media(&self.state.user_settings) == Some(false);
        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .children(results.gifs.iter().enumerate().map(|(index, gif)| {
                let title = if gif.title.is_empty() {
                    "GIF"
                } else {
                    &gif.title
                };
                let preview = gif
                    .image_preview()
                    .and_then(|source| media_animation::animated_proxy_url(source, false))
                    .map(SharedString::from);
                let share = gif.share();
                let favorite = favorites.as_ref().and_then(|entries| {
                    share.as_ref()?;
                    let url = fluxer_core::favorite_gif_url(&gif.url)?;
                    let is_favorite = entries.iter().any(|entry| {
                        fluxer_core::favorite_gif_url(&entry.url).as_ref() == Some(&url)
                    });
                    (is_favorite || can_add_favorite).then(|| {
                        let proxy_url = gif.image_preview().unwrap_or("");
                        let content_type = if proxy_url.to_ascii_lowercase().contains(".webp") {
                            "image/webp"
                        } else {
                            "image/gif"
                        };
                        (
                            FavoriteGif {
                                url: gif.url.clone(),
                                proxy_url: proxy_url.to_owned(),
                                content_type: content_type.into(),
                                ..Default::default()
                            },
                            is_favorite,
                        )
                    })
                });
                div()
                    .id(("gif-result", index))
                    .focusable()
                    .tab_stop(true)
                    .cursor_pointer()
                    .relative()
                    .size(px(96.))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded_md()
                    .bg(rgb(BG))
                    .border_1()
                    .border_color(transparent_black())
                    .hover(|view| view.border_color(rgb(0x625474)))
                    .focus(|view| view.border_color(rgb(ACCENT)))
                    .child(
                        div()
                            .h(px(75.))
                            .flex_shrink_0()
                            .when_some(preview, |view, source| {
                                view.child(
                                    VisibleImage::new(
                                        media_animation::image_id("gif-preview", &source),
                                        96.,
                                        75.,
                                        move || img(source.clone()).object_fit(ObjectFit::Cover),
                                    )
                                    .enabled(animate)
                                    .autoplay(autoplay),
                                )
                            }),
                    )
                    .child(
                        div()
                            .px_1()
                            .text_size(px(11.))
                            .truncate()
                            .child(title.to_owned()),
                    )
                    .when_some(favorite, |view, (entry, is_favorite)| {
                        view.child(gif_favorite_button(entry, is_favorite, cx))
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if let Some(share) = &share {
                            this.pick_gif(share.clone(), event.modifiers().shift, window, cx);
                        }
                    }))
            }))
    }
}

fn gif_tab(id: &'static str, label: &'static str, selected: bool) -> Stateful<Div> {
    quiet_button(id, label).when(selected, |view| {
        view.bg(rgb(0x3b3549)).text_color(rgb(TEXT))
    })
}

fn gif_browse_tile(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    preview: Option<&str>,
) -> Stateful<Div> {
    let preview = preview
        .and_then(crate::image_gallery::safe_image_url)
        .map(SharedString::from);
    let has_preview = preview.is_some();
    div()
        .id(id)
        .relative()
        .min_w_0()
        .h(px(80.))
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(rgb(0x51465f))
        .bg(rgb(0x322b40))
        .hover(|view| view.bg(rgb(0x463c54)).border_color(rgb(0x8d7aab)))
        .focus(|view| view.border_color(rgb(ACCENT)))
        .focusable()
        .tab_stop(true)
        .cursor_pointer()
        .when_some(preview, |view, source| {
            view.child(
                VisibleImage::new(
                    media_animation::image_id("gif-category-preview", &source),
                    204.,
                    80.,
                    move || img(source.clone()).object_fit(ObjectFit::Cover),
                )
                .enabled(false),
            )
        })
        .child(
            div()
                .when(has_preview, |view| {
                    view.absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .bg(rgba(0x211c2bed))
                })
                .when(!has_preview, |view| {
                    view.size_full().flex().items_center().justify_center()
                })
                .px_2()
                .py_1()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT))
                .child(div().truncate().child(title.into())),
        )
}

fn gif_favorite_button(
    entry: FavoriteGif,
    is_favorite: bool,
    cx: &mut Context<Fluxer>,
) -> Stateful<Div> {
    let label = if is_favorite {
        "Remove from favorites"
    } else {
        "Add to favorites"
    };
    div()
        .id("toggle-gif-favorite")
        .absolute()
        .top_1()
        .right_1()
        .size(px(24.))
        .rounded_md()
        .bg(rgba(0x211c2bed))
        .border_1()
        .border_color(rgb(0x5d526c))
        .text_size(px(16.))
        .text_color(rgb(if is_favorite { 0xffce72 } else { TEXT }))
        .cursor_pointer()
        .focusable()
        .tab_stop(true)
        .hover(|view| view.bg(rgb(0x473b55)))
        .focus(|view| view.border_color(rgb(ACCENT)))
        .child(
            div()
                .id(("gif-favorite-tooltip", usize::from(is_favorite)))
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .tooltip(tooltip(label))
                .child(if is_favorite { "★" } else { "☆" }),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.save_display_setting(
                fluxer_core::DisplaySetting::FavoriteGif {
                    entry: entry.clone(),
                    add: !is_favorite,
                },
                cx,
            );
        }))
}
