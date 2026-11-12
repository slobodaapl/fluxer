// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

impl Fluxer {
    pub(super) fn watch_stream(&mut self, identity: String, watch: bool, cx: &mut Context<Self>) {
        let changed = self.viewer.desired.contains(&identity) != watch;
        if watch {
            self.viewer.watch(identity.clone());
            self.pane = Pane::Call;
        } else {
            // Drop pixels and desired intent before any transport round trip.
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
                .filter(|index| *index < 2)
        {
            self.fixture_generation += 1;
            self.viewer.receive(
                identity.clone(),
                fixture_frame(index, self.fixture_generation),
            );
        }
        if !self.demo_stream && changed {
            self.command(Command::WatchStream { identity, watch }, cx);
        }
        cx.notify();
    }

    fn stream_name(&self, identity: &str) -> String {
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
        let user = identity
            .strip_prefix("user_")
            .and_then(|part| part.split('_').next())
            .and_then(|id| id.parse::<u64>().ok())
            .and_then(|id| self.state.users.get(&Snowflake(id)));
        user.map(|user| user.display_name().to_owned())
            .unwrap_or_else(|| "Screen share".into())
    }

    fn change_stream_audio(
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

    pub(super) fn call_view(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(identity) = self.viewer.focused.clone() else {
            return self.chat(cx);
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
        let members: Vec<_> = if let Some(channel) = self.state.voice.channel_id {
            self.state
                .voice_participants(channel)
                .into_iter()
                .map(|participant| {
                    let speaking = self.participant_speaking(&participant, channel);
                    (participant.display_name, participant.user_id, speaking)
                })
                .collect()
        } else {
            self.participants
                .iter()
                .enumerate()
                .map(|(index, (identity, (name, _)))| {
                    (
                        name.clone(),
                        Snowflake(index as u64),
                        self.active_speakers.contains(identity),
                    )
                })
                .collect()
        };
        let stop_identity = identity.clone();
        let retry_identity = identity.clone();
        let mute_identity = identity.clone();
        let down_identity = identity.clone();
        let up_identity = identity.clone();
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
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(200.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .bg(rgb(if ended { 0x4a404b } else { 0xb82c50 }))
                                    .text_size(px(10.))
                                    .font_weight(FontWeight::BOLD)
                                    .child(if ended { "ENDED" } else { "LIVE" }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_ellipsis()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{name}'s screen")),
                            )
                            .when_some(frame, |view, frame| {
                                view.child(muted(format!("{} × {}", frame.width, frame.height)))
                            }),
                    )
                    .child(
                        button(
                            "viewer-chat",
                            if self.viewer.chat_open {
                                "Hide chat"
                            } else {
                                "Chat"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.viewer.chat_open = !this.viewer.chat_open;
                            cx.notify();
                        })),
                    )
                    .child(
                        button(
                            "viewer-expand",
                            if self.viewer.expanded {
                                "Restore"
                            } else {
                                "Expand"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.viewer.expanded = !this.viewer.expanded;
                            cx.notify();
                        })),
                    )
                    .child(
                        button(
                            "viewer-fullscreen",
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
                    .child(button("viewer-back", "Back to chat").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.pane = Pane::Chat;
                            cx.notify();
                        },
                    )))
                    .child(
                        button("viewer-minimize", "Minimize")
                            .on_click(cx.listener(|_, _, window, _| window.minimize_window())),
                    )
                    .child(
                        button("viewer-stop", "Stop watching")
                            .bg(rgb(0xb92e50))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.watch_stream(stop_identity.clone(), false, cx);
                            })),
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
                            .child(
                                div()
                                    .id("focused-stream-stage")
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
                                                .child(
                                                    div().font_weight(FontWeight::SEMIBOLD).child(
                                                        if ended {
                                                            "This stream has ended"
                                                        } else if stream_error.is_some() {
                                                            "Could not load this stream"
                                                        } else {
                                                            "Connecting to the stream…"
                                                        },
                                                    ),
                                                )
                                                .child(
                                                    div()
                                                        .max_w(px(440.))
                                                        .px_4()
                                                        .text_center()
                                                        .child(muted(
                                                            (if ended {
                                                                "You are still connected to voice."
                                                            } else if let Some(error) = stream_error
                                                            {
                                                                error.as_str()
                                                            } else {
                                                                "Waiting for the first video frame."
                                                            })
                                                            .to_owned(),
                                                        )),
                                                )
                                                .when(stream_error.is_some(), |view| {
                                                    view.child(
                                                        button(
                                                            "retry-viewer-stream",
                                                            "Retry stream",
                                                        )
                                                        .bg(rgb(0x6f56d7))
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.watch_stream(
                                                                    retry_identity.clone(),
                                                                    false,
                                                                    cx,
                                                                );
                                                                this.watch_stream(
                                                                    retry_identity.clone(),
                                                                    true,
                                                                    cx,
                                                                );
                                                            },
                                                        )),
                                                    )
                                                }),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .id("stream-filmstrip")
                                    .h(px(if short { 82. } else { 112. }))
                                    .flex_shrink_0()
                                    .overflow_x_scroll()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .py_2()
                                    .bg(rgb(0x19161f))
                                    .children(streams.iter().enumerate().map(
                                        |(index, identity)| {
                                            let name = self.stream_name(identity);
                                            let watching = self.viewer.desired.contains(identity);
                                            let active =
                                                self.viewer.focused.as_ref() == Some(identity);
                                            let identity = identity.clone();
                                            let click_identity = identity.clone();
                                            div()
                                                .id(("stream-tile", index))
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
                                                    this.watch_stream(
                                                        click_identity.clone(),
                                                        true,
                                                        cx,
                                                    );
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
                                    .children(members.iter().map(|(name, user_id, speaking)| {
                                        div()
                                            .w(px(72.))
                                            .flex_shrink_0()
                                            .flex()
                                            .flex_col()
                                            .items_center()
                                            .gap_2()
                                            .child(voice_avatar(name, *user_id, *speaking, 30.))
                                            .child(
                                                div()
                                                    .w_full()
                                                    .text_center()
                                                    .text_ellipsis()
                                                    .text_size(px(10.))
                                                    .text_color(rgb(MUTED))
                                                    .child(name.clone()),
                                            )
                                    })),
                            ),
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
                                .child(self.chat(cx)),
                        )
                    }),
            )
            .child(
                div()
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .border_t_1()
                    .border_color(rgb(PANEL))
                    .child(
                        button(
                            "stream-audio-mute",
                            if audio.muted {
                                "Unmute stream"
                            } else {
                                "Mute stream"
                            },
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.change_stream_audio(
                                mute_identity.clone(),
                                |audio| audio.muted = !audio.muted,
                                cx,
                            );
                        })),
                    )
                    .child(button("stream-volume-down", "−").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.change_stream_audio(
                                down_identity.clone(),
                                |audio| audio.gain = (audio.gain - 0.1).max(0.),
                                cx,
                            );
                        },
                    )))
                    .child(muted(format!("Stream {:.0}%", audio.gain * 100.)))
                    .child(button("stream-volume-up", "+").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.change_stream_audio(
                                up_identity.clone(),
                                |audio| audio.gain = (audio.gain + 0.1).min(2.),
                                cx,
                            );
                        },
                    )))
                    .child(div().flex_1())
                    .when(self.demo_stream, |view| {
                        view.child(muted("Offline preview · No voice connection"))
                    })
                    .when(!self.demo_stream, |view| {
                        view.child(
                            button(
                                "viewer-mic",
                                if self.state.voice.muted {
                                    "Unmute mic"
                                } else {
                                    "Mute mic"
                                },
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.command(Command::SetMute(!this.state.voice.muted), cx);
                            })),
                        )
                        .child(
                            button(
                                "viewer-deafen",
                                if self.state.voice.deafened {
                                    "Undeafen"
                                } else {
                                    "Deafen"
                                },
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.command(Command::SetDeaf(!this.state.voice.deafened), cx);
                            })),
                        )
                        .child(
                            button("viewer-leave", "Leave voice")
                                .bg(rgb(0x68303c))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.viewer.clear();
                                    this.pane = Pane::Chat;
                                    this.command(Command::LeaveVoice, cx);
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn install_stream_fixture(&mut self) {
        self.state = ClientState::demo();
        for (index, name) in ["Alex", "Morgan"].into_iter().enumerate() {
            let identity = format!("preview-{index}");
            self.participants.insert(
                identity.clone(),
                (name.into(), ParticipantAudioSettings::default()),
            );
            self.available_streams.insert(identity.clone());
            self.viewer.watch(identity.clone());
            self.viewer
                .receive(identity, fixture_frame(index, self.fixture_generation));
        }
        self.viewer.focused = Some("preview-0".into());
        self.pane = Pane::Call;
    }
}

fn fixture_frame(index: usize, generation: u64) -> StreamFrame<Arc<RenderImage>> {
    let (width, height) = if index == 0 { (1600, 900) } else { (900, 1600) };
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
