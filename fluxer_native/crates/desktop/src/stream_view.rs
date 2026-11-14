// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::controls::{Icon, icon_button, quiet_button, slider};
use crate::{stream_grid::StreamGrid, stream_layout::StreamLayout};

impl Fluxer {
    pub(super) fn watch_stream(&mut self, identity: String, watch: bool, cx: &mut Context<Self>) {
        let changed = self.viewer.desired.contains(&identity) != watch;
        if watch {
            self.viewer.watch(identity.clone());
            self.pane = Pane::Call;
        } else {
            // Drop pixels and desired intent before any transport round trip.
            self.retire_stream_image(&identity, cx);
            self.viewer.stop(&identity);
            if self.viewer.focused.is_none() {
                self.pane = Pane::Chat;
            }
        }
        if self.demo_stream
            && changed
            && watch
            && let Some(index) = identity
                .strip_prefix("preview-")
                .and_then(|index| index.parse::<usize>().ok())
                .filter(|index| *index < 4)
        {
            self.fixture_generation += 1;
            let frame = fixture_frame(index, self.fixture_generation);
            self.video_stats.image_added(frame.image.id.0);
            self.viewer.receive(identity.clone(), frame);
        }
        if let Some(animation) = &self.animation
            && identity == "preview-0"
        {
            animation.set_active(watch, self.fixture_generation);
        }
        if !self.demo_stream && changed {
            self.command(Command::WatchStream { identity, watch }, cx);
        }
        cx.notify();
    }

    pub(super) fn stop_focused_stream(&mut self, cx: &mut Context<Self>) {
        self.video_stats.stopped();
        if let Some(identity) = self.viewer.focused.clone() {
            self.watch_stream(identity, false, cx);
        }
    }
    pub(super) fn schedule_perf_stop(&self, window: &Window, cx: &mut Context<Self>) {
        let Some(delay) = std::env::var("FLUXER_VIDEO_PERF_STOP_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
        else {
            return;
        };
        let delay = Duration::from_millis(delay);
        let deadline = std::time::Instant::now() + delay;
        self.video_stats.request_stop(deadline);
        let window = window.window_handle();
        cx.spawn(async move |_, cx| {
            Timer::after(delay).await;
            let _ = window.update(cx, |_, window, cx| {
                window.dispatch_action(Box::new(StopWatching), cx)
            });
        })
        .detach();
    }

    pub(super) fn stream_name(&self, identity: &str) -> String {
        if let Some((name, _)) = self.participants.get(identity) {
            return name.clone();
        }
        if let Some(channel) = self.state.voice.channel_id
            && let Some(participant) = self
                .state
                .voice_participants(channel)
                .iter()
                .find(|participant| participant.livekit_identity.as_deref() == Some(identity))
        {
            return participant.display_name.clone();
        }
        let user = self
            .cached_stream_user(identity)
            .and_then(|id| self.state.users.get(&id));
        user.map(|user| user.display_name().to_owned())
            .unwrap_or_else(|| "Screen share".into())
    }

    fn cached_stream_user(&self, identity: &str) -> Option<Snowflake> {
        if let Some(channel) = self.state.voice.channel_id
            && let Some(participant) = self
                .state
                .voice_participants(channel)
                .into_iter()
                .find(|participant| participant.livekit_identity.as_deref() == Some(identity))
        {
            return Some(participant.user_id);
        }
        let id = sdk_identity_user(identity)?;
        (self.state.users.contains_key(&id)
            || self
                .state
                .current_user
                .as_ref()
                .is_some_and(|user| user.id == id))
        .then_some(id)
    }

    pub(super) fn change_stream_audio(
        &mut self,
        identity: String,
        change: impl FnOnce(&mut ParticipantAudioSettings),
        cx: &mut Context<Self>,
    ) {
        let settings = self.stream_audio.entry(identity.clone()).or_default();
        change(settings);
        let settings = *settings;
        if !self.demo_stream {
            self.command(Command::SetStreamAudio { identity, settings }, cx);
        }
        cx.notify();
    }

    fn tiled_stream_stage(&self, short: bool, cx: &mut Context<Self>) -> AnyElement {
        let tiles = self
            .viewer
            .desired
            .iter()
            .map(|identity| {
                let ratio = self
                    .viewer
                    .frames
                    .get(identity)
                    .map(|frame| frame.width.max(1) as f32 / frame.height.max(1) as f32)
                    .unwrap_or(16. / 9.);
                (ratio, self.watched_stream_tile(identity, cx))
            })
            .collect();
        div()
            .id("tiled-stream-stage")
            .flex_1()
            .min_h(px(if short { 80. } else { 120. }))
            .min_w_0()
            .relative()
            .p_2()
            .bg(rgb(0x050506))
            .overflow_hidden()
            .child(StreamGrid::new(tiles, 8.))
            .into_any_element()
    }

    fn watched_stream_tile(&self, identity: &str, cx: &mut Context<Self>) -> AnyElement {
        let frame = self.viewer.frames.get(identity);
        let name = self.stream_name(identity);
        let selected = self.viewer.focused.as_deref() == Some(identity);
        let ended = self.viewer.ended.contains(identity);
        let error = self.viewer.errors.get(identity);
        let audio = self.stream_audio.get(identity).copied().unwrap_or_default();
        let focus_identity = identity.to_owned();
        let stop_identity = identity.to_owned();
        let mute_identity = identity.to_owned();
        let retry_identity = identity.to_owned();
        div()
            .id(SharedString::from(format!("watched-tile-{identity}")))
            .map(|view| {
                crate::context_actions::trigger(
                    view,
                    crate::context_actions::Target::Stream(identity.to_owned()),
                    cx,
                )
            })
            .size_full()
            .min_w_0()
            .min_h_0()
            .relative()
            .rounded_md()
            .border_2()
            .border_color(rgb(if selected { ACCENT } else { 0x373140 }))
            .bg(rgb(0x08070a))
            .overflow_hidden()
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.viewer.focused = Some(focus_identity.clone());
                if event.click_count() > 1 {
                    this.viewer.layout = StreamLayout::MainStage;
                }
                cx.notify();
            }))
            .when_some(frame, |tile, frame| {
                tile.child(
                    img(frame.image.clone())
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
            })
            .when(frame.is_none(), |tile| {
                tile.child(
                    div()
                        .absolute()
                        .inset_0()
                        .px_3()
                        .pt(px(32.))
                        .pb_2()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .text_center()
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .child(if ended {
                                    "This stream has ended"
                                } else if error.is_some() {
                                    "Could not load this stream"
                                } else {
                                    "Connecting to the stream…"
                                }),
                        )
                        .when_some(error, |tile, error| {
                            tile.child(
                                div()
                                    .max_w(px(340.))
                                    .text_size(px(11.))
                                    .text_color(rgb(MUTED))
                                    .child(error.clone()),
                            )
                        })
                        .when(error.is_some(), |tile| {
                            tile.child(
                                quiet_button(
                                    SharedString::from(format!("retry-tiled-{identity}")),
                                    "Retry stream",
                                )
                                .bg(rgb(0x6f56d7))
                                .text_color(rgb(TEXT))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.watch_stream(retry_identity.clone(), false, cx);
                                        this.watch_stream(retry_identity.clone(), true, cx);
                                    },
                                )),
                            )
                        }),
                )
            })
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(32.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .bg(rgba(0x15111cdd))
                    .child(div().size(px(5.)).rounded_full().bg(rgb(if ended {
                        MUTED
                    } else {
                        0x6bc99d
                    })))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_ellipsis()
                            .text_size(px(11.))
                            .text_color(rgb(TEXT))
                            .child(name),
                    )
                    .when(selected, |tile| {
                        tile.child(
                            div()
                                .text_size(px(9.))
                                .text_color(rgb(0xc4b7ff))
                                .child("Selected"),
                        )
                    })
                    .child(
                        icon_button(
                            SharedString::from(format!("mute-tiled-{identity}")),
                            if audio.muted {
                                Icon::VolumeOff
                            } else {
                                Icon::Volume
                            },
                            if audio.muted {
                                "Unmute this stream"
                            } else {
                                "Mute this stream"
                            },
                        )
                        .when(audio.muted, |button| button.text_color(rgb(0xe79a9e)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.change_stream_audio(
                                mute_identity.clone(),
                                |audio| audio.muted = !audio.muted,
                                cx,
                            );
                        })),
                    )
                    .child(
                        icon_button(
                            SharedString::from(format!("stop-tiled-{identity}")),
                            Icon::Stop,
                            "Stop watching this stream",
                        )
                        .text_color(rgb(0xe79a9e))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.watch_stream(stop_identity.clone(), false, cx);
                        })),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn call_view(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(identity) = self.viewer.focused.clone() else {
            return self.chat(window, cx);
        };
        let name = self.stream_name(&identity);
        let frame = self.viewer.frames.get(&identity);
        let ended = self.viewer.ended.contains(&identity);
        let stream_error = self.viewer.errors.get(&identity);
        let audio = self
            .stream_audio
            .get(&identity)
            .copied()
            .unwrap_or_default();
        let narrow = window.viewport_size().width
            < px(if self.viewer.expanded || window.is_fullscreen() {
                1000.
            } else {
                1260.
            });
        let short = window.viewport_size().height < px(700.);
        let mut streams = self.available_streams.clone();
        streams.extend(self.viewer.desired.iter().cloned());
        let voice_guild = self
            .state
            .voice
            .channel_id
            .and_then(|id| self.state.channels.get(&id))
            .and_then(|channel| channel.guild_id);
        let members: Vec<_> = if let Some(channel) = self.state.voice.channel_id {
            self.state
                .voice_participants(channel)
                .into_iter()
                .map(|participant| {
                    let speaking = self.participant_speaking(&participant, channel);
                    (
                        participant.display_name,
                        Some(participant.user_id),
                        speaking,
                        participant.livekit_identity,
                    )
                })
                .collect()
        } else {
            self.participants
                .iter()
                .map(|(identity, (name, _))| {
                    (
                        name.clone(),
                        self.cached_stream_user(identity),
                        self.active_speakers.contains(identity),
                        Some(identity.clone()),
                    )
                })
                .collect()
        };
        let retry_identity = identity.clone();
        let mute_identity = identity.clone();
        let gain_identity = identity.clone();
        let stage = if self.viewer.layout == StreamLayout::Tiled && !self.viewer.desired.is_empty()
        {
            self.tiled_stream_stage(short, cx)
        } else {
            div()
                .id("focused-stream-stage")
                .map(|view| {
                    crate::context_actions::trigger(
                        view,
                        crate::context_actions::Target::Stream(identity.clone()),
                        cx,
                    )
                })
                .relative()
                .flex_1()
                .min_h(px(if short { 80. } else { 120. }))
                .min_w_0()
                .bg(rgb(0x050506))
                .overflow_hidden()
                .when_some(frame, |view, frame| {
                    view.child(
                        img(frame.image.clone())
                            .absolute()
                            .inset_0()
                            .size_full()
                            .object_fit(ObjectFit::Contain),
                    )
                })
                .when(frame.is_none(), |view| {
                    view.child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_size(px(30.))
                                    .text_color(rgb(0x777082))
                                    .child("▣"),
                            )
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(if ended {
                                "This stream has ended"
                            } else if stream_error.is_some() {
                                "Could not load this stream"
                            } else {
                                "Connecting to the stream…"
                            }))
                            .child(
                                div().max_w(px(440.)).px_4().text_center().child(muted(
                                    (if ended {
                                        "You are still connected to voice."
                                    } else if let Some(error) = stream_error {
                                        error.as_str()
                                    } else {
                                        "Waiting for the first video frame."
                                    })
                                    .to_owned(),
                                )),
                            )
                            .when(stream_error.is_some(), |view| {
                                view.child(
                                    quiet_button("retry-viewer-stream", "Retry stream")
                                        .bg(rgb(0x6f56d7))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.watch_stream(retry_identity.clone(), false, cx);
                                            this.watch_stream(retry_identity.clone(), true, cx);
                                        })),
                                )
                            }),
                    )
                })
                .into_any_element()
        };
        let strip_streams = streams
            .iter()
            .filter(|identity| {
                self.viewer.layout == StreamLayout::MainStage
                    || !self.viewer.desired.contains(*identity)
            })
            .cloned()
            .collect::<Vec<_>>();
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x111015))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(6.)).rounded_full().bg(rgb(if ended {
                                MUTED
                            } else {
                                0x6bc99d
                            })))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_ellipsis()
                                    .child(
                                        if self.viewer.layout == StreamLayout::Tiled
                                            && !self.viewer.desired.is_empty()
                                        {
                                            format!(
                                                "{} stream{} · {name} selected",
                                                self.viewer.desired.len(),
                                                if self.viewer.desired.len() == 1 {
                                                    ""
                                                } else {
                                                    "s"
                                                }
                                            )
                                        } else {
                                            format!("{name}'s screen")
                                        },
                                    ),
                            )
                            .when_some(frame, |view, frame| {
                                view.when(!narrow, |view| {
                                    view.child(
                                        div()
                                            .text_size(px(10.))
                                            .text_color(rgb(MUTED))
                                            .child(format!("{} × {}", frame.width, frame.height)),
                                    )
                                })
                            }),
                    )
                    .child(
                        div()
                            .id("stream-layout-options")
                            .p(px(2.))
                            .rounded_md()
                            .bg(rgb(0x292333))
                            .flex()
                            .items_center()
                            .gap_0()
                            .child(
                                quiet_button("stream-layout-tiled", "Tile all")
                                    .when(self.viewer.layout == StreamLayout::Tiled, |button| {
                                        button.bg(rgb(0x514365)).text_color(rgb(TEXT))
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.viewer.layout = StreamLayout::Tiled;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                quiet_button("stream-layout-main", "Main stage")
                                    .when(self.viewer.layout == StreamLayout::MainStage, |button| {
                                        button.bg(rgb(0x514365)).text_color(rgb(TEXT))
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.viewer.layout = StreamLayout::MainStage;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        icon_button(
                            "viewer-chat",
                            Icon::Chat,
                            if self.viewer.chat_open {
                                "Hide chat"
                            } else {
                                "Show chat"
                            },
                        )
                        .when(self.viewer.chat_open, |view| view.text_color(rgb(ACCENT)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.viewer.chat_open = !this.viewer.chat_open;
                            cx.notify();
                        })),
                    )
                    .child(
                        icon_button(
                            "viewer-expand",
                            if self.viewer.expanded {
                                Icon::Minimize
                            } else {
                                Icon::Expand
                            },
                            if self.viewer.expanded {
                                "Restore sidebar"
                            } else {
                                "Expand stream"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.viewer.expanded = !this.viewer.expanded;
                            cx.notify();
                        })),
                    )
                    .child(
                        icon_button(
                            "viewer-fullscreen",
                            Icon::Fullscreen,
                            if window.is_fullscreen() {
                                "Exit fullscreen"
                            } else {
                                "Fullscreen"
                            },
                        )
                        .on_click(cx.listener(|_, _, window, cx| {
                            window.toggle_fullscreen();
                            cx.notify();
                        })),
                    )
                    .child(
                        icon_button("viewer-back", Icon::Back, "Back to chat; keep watching")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pane = Pane::Chat;
                                cx.notify();
                            })),
                    )
                    .child(
                        icon_button("viewer-minimize", Icon::Minimize, "Minimize window")
                            .on_click(cx.listener(|_, _, window, _| window.minimize_window())),
                    )
                    .child(
                        icon_button("viewer-stop", Icon::Stop, "Stop watching")
                            .text_color(rgb(0xe79a9e))
                            .on_click(cx.listener(|this, _, _, cx| this.stop_focused_stream(cx))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .when(narrow, |view| view.flex_col())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(stage)
                            .when(!strip_streams.is_empty() || !members.is_empty(), |view| {
                                view.child(
                                    div()
                                        .id("stream-filmstrip")
                                        .h(px(
                                            if self.viewer.layout == StreamLayout::Tiled
                                                && strip_streams.is_empty()
                                            {
                                                72.
                                            } else if short {
                                                82.
                                            } else {
                                                112.
                                            },
                                        ))
                                        .flex_shrink_0()
                                        .overflow_x_scroll()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .px_3()
                                        .py_2()
                                        .bg(rgb(0x19161f))
                                        .children(strip_streams.iter().enumerate().map(
                                            |(index, identity)| {
                                                let name = self.stream_name(identity);
                                                let watching =
                                                    self.viewer.desired.contains(identity);
                                                let active =
                                                    self.viewer.focused.as_ref() == Some(identity);
                                                let identity = identity.clone();
                                                let click_identity = identity.clone();
                                                div()
                                                    .id(("stream-tile", index))
                                                    .map(|view| {
                                                        crate::context_actions::trigger(
                                                            view,
                                                            crate::context_actions::Target::Stream(
                                                                identity.clone(),
                                                            ),
                                                            cx,
                                                        )
                                                    })
                                                    .w(px(140.))
                                                    .flex_shrink_0()
                                                    .rounded_md()
                                                    .overflow_hidden()
                                                    .border_2()
                                                    .border_color(rgb(if active {
                                                        ACCENT
                                                    } else {
                                                        0x373140
                                                    }))
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        if this
                                                            .viewer
                                                            .desired
                                                            .contains(&click_identity)
                                                        {
                                                            this.viewer.focused =
                                                                Some(click_identity.clone());
                                                            cx.notify();
                                                        } else {
                                                            this.watch_stream(
                                                                click_identity.clone(),
                                                                true,
                                                                cx,
                                                            );
                                                        }
                                                    }))
                                                    .child(
                                                        div()
                                                            .h(px(if short { 38. } else { 62. }))
                                                            .relative()
                                                            .bg(rgb(0x09080b))
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .when_some(
                                                                self.viewer.frames.get(&identity),
                                                                |view, frame| {
                                                                    view.child(
                                                                        img(frame.image.clone())
                                                                            .size_full()
                                                                            .object_fit(
                                                                                ObjectFit::Contain,
                                                                            ),
                                                                    )
                                                                },
                                                            )
                                                            .when(
                                                                !self
                                                                    .viewer
                                                                    .frames
                                                                    .contains_key(&identity),
                                                                |view| {
                                                                    view.child(muted(if watching {
                                                                        "Loading…"
                                                                    } else {
                                                                        "Watch stream"
                                                                    }))
                                                                },
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .px_2()
                                                            .py_1()
                                                            .text_size(px(11.))
                                                            .text_ellipsis()
                                                            .child(name),
                                                    )
                                            },
                                        ))
                                        .children(members.iter().map(
                                            |(name, user_id, speaking, identity)| {
                                                let menu_target = self.voice_context_target(
                                                    self.state.voice.channel_id,
                                                    *user_id,
                                                    identity.clone(),
                                                );
                                                div()
                                                    .id(SharedString::from(format!(
                                                        "filmstrip-person-{}",
                                                        identity.as_deref().unwrap_or(name)
                                                    )))
                                                    .when_some(menu_target, |view, target| {
                                                        crate::context_actions::trigger(
                                                            view, target, cx,
                                                        )
                                                    })
                                                    .w(px(72.))
                                                    .flex_shrink_0()
                                                    .flex()
                                                    .flex_col()
                                                    .items_center()
                                                    .gap_2()
                                                    .child(if let Some(user_id) = user_id {
                                                        self.profile_trigger(div().id(SharedString::from(format!("filmstrip-profile-avatar-{}",identity.as_deref().unwrap_or(name)))).size(px(30.)).child(crate::avatar_images::member_avatar(
                                                            &self.state,
                                                            *user_id,
                                                            voice_guild,
                                                            name,
                                                            self.media_endpoint.as_deref(),
                                                            *speaking,
                                                            30.,
                                                        )),self.profile_user(*user_id,name),voice_guild,false,cx).into_any_element()
                                                    } else {
                                                        // SDK-only/offline preview names do not establish an
                                                        // account ID. Retain the neutral initials fallback.
                                                        voice_avatar(
                                                            name,
                                                            Snowflake::default(),
                                                            *speaking,
                                                            30.,
                                                        )
                                                    })
                                                    .child(
                                                        div().id(SharedString::from(format!("filmstrip-profile-name-{}",identity.as_deref().unwrap_or(name))))
                                                            .max_w_full()
                                                            .text_center()
                                                            .text_ellipsis()
                                                            .text_size(px(10.))
                                                            .text_color(rgb(MUTED))
                                                            .child(name.clone())
                                                            .when_some(*user_id,|view,id|self.profile_trigger(view,self.profile_user(id,name),voice_guild,false,cx)),
                                                    )
                                            },
                                        )),
                                )
                            }),
                    )
                    .when(self.viewer.chat_open, |view| {
                        view.child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .min_h_0()
                                .border_l_1()
                                .border_color(rgb(PANEL))
                                .when(!narrow, |view| view.w(px(340.)).h_full().flex_shrink_0())
                                .when(narrow, |view| {
                                    view.w_full()
                                        .h(px(if short { 150. } else { 220. }))
                                        .flex_shrink_0()
                                })
                                .child(self.chat(window, cx)),
                        )
                    }),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(rgb(PANEL))
                    .child(
                        icon_button(
                            "stream-audio-mute",
                            if audio.muted {
                                Icon::VolumeOff
                            } else {
                                Icon::Volume
                            },
                            if audio.muted {
                                "Unmute stream audio"
                            } else {
                                "Mute stream audio"
                            },
                        )
                        .when(audio.muted, |view| view.text_color(rgb(0xe79a9e)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.change_stream_audio(
                                mute_identity.clone(),
                                |audio| audio.muted = !audio.muted,
                                cx,
                            )
                        })),
                    )
                    .child(div().w(px(if narrow { 90. } else { 150. })).child(slider(
                        "stream-volume",
                        audio.gain,
                        0.,
                        2.,
                        0.01,
                        "Stream audio volume",
                        cx.listener(move |this, value: &f32, _, cx| {
                            this.change_stream_audio(
                                gain_identity.clone(),
                                |audio| audio.gain = *value,
                                cx,
                            );
                        }),
                    )))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(MUTED))
                            .child(format!("{:.0}%", audio.gain * 100.)),
                    )
                    .child(div().flex_1())
                    .when(self.demo_stream, |view| {
                        view.child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(MUTED))
                                .child("Offline preview"),
                        )
                    })
                    .when(!self.demo_stream, |view| {
                        view.child(
                            icon_button(
                                "viewer-mic",
                                if self.state.voice.muted {
                                    Icon::MicOff
                                } else {
                                    Icon::Mic
                                },
                                if self.state.voice.muted {
                                    "Unmute microphone"
                                } else {
                                    "Mute microphone"
                                },
                            )
                            .when(self.state.voice.muted, |view| {
                                view.text_color(rgb(0xe79a9e))
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.command(Command::SetMute(!this.state.voice.muted), cx)
                            })),
                        )
                        .child(
                            icon_button(
                                "viewer-deafen",
                                if self.state.voice.deafened {
                                    Icon::Deafen
                                } else {
                                    Icon::Headphones
                                },
                                if self.state.voice.deafened {
                                    "Undeafen"
                                } else {
                                    "Deafen"
                                },
                            )
                            .when(self.state.voice.deafened, |view| {
                                view.text_color(rgb(0xe79a9e))
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.command(Command::SetDeaf(!this.state.voice.deafened), cx)
                            })),
                        )
                        .child(
                            icon_button("viewer-leave", Icon::PhoneOff, "Disconnect from voice")
                                .text_color(rgb(0xe79a9e))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.clear_viewer(cx);
                                    this.pane = Pane::Chat;
                                    this.command(Command::LeaveVoice, cx);
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn install_stream_fixture(&mut self, cx: &mut Context<Self>) {
        self.clear_viewer(cx);
        self.state = ClientState::demo();
        let count = if std::env::args().any(|argument| argument == "--demo-stream-many") {
            4
        } else {
            2
        };
        for (index, name) in ["Alex", "Morgan", "Robin", "Taylor"]
            .into_iter()
            .take(count)
            .enumerate()
        {
            let identity = format!("preview-{index}");
            self.participants.insert(
                identity.clone(),
                (name.into(), ParticipantAudioSettings::default()),
            );
            self.available_streams.insert(identity.clone());
            self.viewer.watch(identity.clone());
            let frame = fixture_frame(index, self.fixture_generation);
            self.video_stats.image_added(frame.image.id.0);
            self.viewer.receive(identity, frame);
        }
        self.viewer.focused = Some("preview-0".into());
        self.pane = Pane::Call;
    }
}

fn fixture_frame(index: usize, generation: u64) -> StreamFrame<Arc<RenderImage>> {
    let (width, height) = if index.is_multiple_of(2) {
        (1600, 900)
    } else {
        (900, 1600)
    };
    let buffer = image::RgbaImage::from_fn(width, height, |x, y| {
        let edge = x < 4 || y < 4 || x + 4 >= width || y + 4 >= height;
        let panel = x < width / 5 || y < height / 12;
        let grid = x % 160 < 2 || y % 90 < 2;
        let stripe = y > height / 4
            && y < height * 3 / 4
            && (y / 20).is_multiple_of(3)
            && x > width / 3
            && x < width * 4 / 5;
        image::Rgba(if edge {
            [235, 235, 235, 255]
        } else if panel {
            [42, 35, 58, 255]
        } else if stripe {
            [165, 128, 91, 255]
        } else if grid {
            [82, 67, 100, 255]
        } else {
            [32 + (x % 80) as u8, 24 + (y % 60) as u8, 24, 255]
        })
    });
    let image = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
        buffer
    )]));
    StreamFrame {
        track_id: format!("preview-track-{index}"),
        watch_generation: generation,
        width,
        height,
        image,
    }
}

// VoiceParticipantIdentity.ts: the connection suffix may itself contain `_`.
pub(super) fn sdk_identity_user(identity: &str) -> Option<Snowflake> {
    let (user, connection) = identity.strip_prefix("user_")?.split_once('_')?;
    if connection.is_empty() || user.is_empty() || !user.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    user.parse::<u64>().ok().filter(|id| *id > 0).map(Snowflake)
}

#[cfg(test)]
mod avatar_identity_tests {
    use super::sdk_identity_user;
    use fluxer_core::Snowflake;
    #[test]
    fn sdk_identity_requires_real_user_prefix_and_nonempty_connection() {
        assert_eq!(
            sdk_identity_user("user_42_conn_with_underscores"),
            Some(Snowflake(42))
        );
        for identity in [
            "preview-0",
            "42_connection",
            "user_42",
            "user_42_",
            "user__connection",
            "user_0_connection",
            "user_abc_connection",
            "user_+42_connection",
        ] {
            assert_eq!(sdk_identity_user(identity), None, "{identity}");
        }
    }
}
