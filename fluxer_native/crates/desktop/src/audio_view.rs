// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

impl Fluxer {
    pub(super) fn voice_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .p_3()
            .bg(rgb(0x292438))
            .flex()
            .flex_col()
            .gap_2()
            .child(muted(self.voice_status.clone()))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "mute",
                            if self.state.voice.muted {
                                "Unmute"
                            } else {
                                "Mute"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.command(Command::SetMute(!this.state.voice.muted), cx)
                        })),
                    )
                    .child(
                        button(
                            "deaf",
                            if self.state.voice.deafened {
                                "Undeafen"
                            } else {
                                "Deafen"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.command(Command::SetDeaf(!this.state.voice.deafened), cx)
                        })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button("share", if self.sharing { "Stop share" } else { "Share" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.sharing {
                                    this.command(Command::StopShare, cx);
                                } else {
                                    this.pane = Pane::Screens;
                                    this.command(Command::RefreshScreenSources, cx);
                                    this.command(Command::RefreshAudioApplications, cx);
                                }
                            })),
                    )
                    .child(button("leave", "Leave").on_click(
                        cx.listener(|this, _, _, cx| this.command(Command::LeaveVoice, cx)),
                    )),
            )
            .when(self.sharing, |view| {
                view.child(muted(if self.shared_audio {
                    if self.shared_audio_settings.muted {
                        "Shared audio muted"
                    } else {
                        "Sharing screen and audio"
                    }
                } else {
                    "Sharing screen without audio"
                }))
                .when(self.shared_audio, |view| {
                    view.child(
                        button(
                            "active-share-audio-mute",
                            if self.shared_audio_settings.muted {
                                "Unmute stream audio"
                            } else {
                                "Mute stream audio"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.shared_audio_settings.muted = !this.shared_audio_settings.muted;
                            this.command(Command::SetSharedAudio(this.shared_audio_settings), cx);
                        })),
                    )
                })
            })
            .into_any_element()
    }

    pub(super) fn participant_speaking(
        &self,
        participant: &VoiceParticipant,
        channel: Snowflake,
    ) -> bool {
        self.state.voice.channel_id == Some(channel)
            && !participant.self_mute
            && !participant.mute
            && !participant.suppress
            && !participant.self_deaf
            && !participant.deaf
            && participant
                .livekit_identity
                .as_ref()
                .is_some_and(|identity| self.active_speakers.contains(identity))
    }

    pub(super) fn voice_sidebar_participants(
        &self,
        channel: Snowflake,
    ) -> Vec<(VoiceParticipant, bool)> {
        // VoiceParticipantsList groups multiple connections by account; self
        // mute/deaf aggregates across all devices, while live/camera/speaking
        // flags are shown when any connection has them.
        let mut grouped = Vec::<(VoiceParticipant, bool)>::new();
        for participant in self.state.voice_participants(channel) {
            let speaking = self.participant_speaking(&participant, channel);
            if let Some((entry, any_speaking)) = grouped
                .iter_mut()
                .find(|(entry, _)| entry.user_id == participant.user_id)
            {
                entry.self_mute &= participant.self_mute;
                entry.self_deaf &= participant.self_deaf;
                entry.mute |= participant.mute;
                entry.deaf |= participant.deaf;
                entry.suppress |= participant.suppress;
                entry.self_video |= participant.self_video;
                entry.self_stream |= participant.self_stream;
                *any_speaking |= speaking;
            } else {
                grouped.push((participant, speaking));
            }
        }
        grouped
    }

    pub(super) fn voice_participant_pane(
        &self,
        channel: Snowflake,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let participants = self.state.voice_participants(channel);
        let count = participants.len();
        let connected = self.state.voice.channel_id == Some(channel);
        let guild = self
            .state
            .channels
            .get(&channel)
            .and_then(|channel| channel.guild_id);
        div()
            .flex_shrink_0()
            .bg(rgb(0x211e2b))
            .border_b_1()
            .border_color(rgb(0x363140))
            .flex()
            .flex_col()
            .child(
                div()
                    .px_5()
                    .py_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Voice participants"),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .rounded_md()
                                    .bg(rgb(0x393248))
                                    .text_size(px(12.))
                                    .child(count.to_string()),
                            ),
                    )
                    .child(
                        button(
                            ("viewed-voice-call", channel.0),
                            if connected {
                                "Leave voice"
                            } else {
                                "Join voice"
                            },
                        )
                        .bg(rgb(if connected { 0x613342 } else { 0x6f56d7 }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if connected {
                                this.command(Command::LeaveVoice, cx);
                            } else {
                                this.command(
                                    Command::JoinVoice {
                                        guild_id: guild,
                                        channel_id: channel,
                                    },
                                    cx,
                                );
                            }
                        })),
                    ),
            )
            .child(
                div()
                    .id(("voice-participants", channel.0))
                    .max_h(px(250.))
                    .overflow_y_scroll()
                    .px_5()
                    .pb_4()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .when(participants.is_empty(), |view| {
                        view.child(
                            div()
                                .py_5()
                                .text_color(rgb(MUTED))
                                .child("No one is in this voice channel."),
                        )
                    })
                    .children(
                        participants
                            .into_iter()
                            .enumerate()
                            .map(|(index, participant)| {
                                let speaking = self.participant_speaking(&participant, channel);
                                // Availability is filtered by the worker's current call
                                // and local connection, including own-account other devices.
                                let stream_identity =
                                    participant.livekit_identity.clone().filter(|identity| {
                                        participant.self_stream
                                            && self.state.voice.channel_id == Some(channel)
                                            && self.available_streams.contains(identity)
                                    });
                                div()
                                    .id(("voice-participant-card", index))
                                    .w(px(172.))
                                    .min_h(px(134.))
                                    .p_3()
                                    .rounded_lg()
                                    .border_2()
                                    .border_color(rgb(if speaking { 0x4fc58a } else { 0x373140 }))
                                    .bg(rgb(0x2b2638))
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .child(voice_avatar(
                                        &participant.display_name,
                                        participant.user_id,
                                        speaking,
                                        48.,
                                    ))
                                    .child(
                                        div()
                                            .w_full()
                                            .min_w_0()
                                            .text_center()
                                            .text_size(px(13.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_ellipsis()
                                            .child(format!(
                                                "{}{}",
                                                participant.display_name,
                                                if participant.is_current_user {
                                                    " (you)"
                                                } else {
                                                    ""
                                                }
                                            )),
                                    )
                                    .child(voice_badges(&participant, false))
                                    .when_some(stream_identity, |view, identity| {
                                        let watching = self.viewer.desired.contains(&identity);
                                        view.child(
                                            button(
                                                ("watch-participant-stream", index),
                                                if watching {
                                                    "Stop watching"
                                                } else {
                                                    "Watch stream"
                                                },
                                            )
                                            .when(watching, |view| view.bg(rgb(0x6650bd)))
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.watch_stream(
                                                        identity.clone(),
                                                        !watching,
                                                        cx,
                                                    );
                                                }),
                                            ),
                                        )
                                    })
                                    .when(speaking, |view| {
                                        view.child(
                                            div()
                                                .text_size(px(10.))
                                                .text_color(rgb(0x66d4a0))
                                                .child("Speaking"),
                                        )
                                    })
                            }),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn settings(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("settings-body")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(heading("Voice & audio"))
            .child(muted(
                "Choose devices and configure microphone processing. Changes apply immediately.",
            ))
            .child(
                button("refresh-devices", "Refresh audio devices")
                    .max_w(px(180.))
                    .on_click(
                        cx.listener(|this, _, _, cx| this.command(Command::RefreshDevices, cx)),
                    ),
            )
            .child(self.input_picker.clone())
            .child(self.output_picker.clone())
            .child(self.audio_controls(cx))
            .child(heading("Microphone processing"))
            .children(
                [
                    ("aec", "Echo cancellation", self.dsp.echo_cancellation),
                    ("ns", "Noise suppression", self.dsp.noise_suppression),
                    ("agc", "Automatic gain", self.dsp.auto_gain_control),
                ]
                .into_iter()
                .map(|(id, label, enabled)| {
                    div()
                        .w_full()
                        .max_w(px(520.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(label)
                        .child(
                            button(id, if enabled { "On" } else { "Off" })
                                .w(px(64.))
                                .text_center()
                                .when(enabled, |view| view.bg(rgb(0x6650bd)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    match id {
                                        "aec" => {
                                            this.dsp.echo_cancellation = !this.dsp.echo_cancellation
                                        }
                                        "ns" => {
                                            this.dsp.noise_suppression = !this.dsp.noise_suppression
                                        }
                                        _ => {
                                            this.dsp.auto_gain_control = !this.dsp.auto_gain_control
                                        }
                                    }
                                    this.command(Command::SetDsp(this.dsp.clone()), cx);
                                })),
                        )
                }),
            )
            .child(
                button("back-chat", "Back to chat")
                    .max_w(px(180.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pane = Pane::Chat;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    pub(super) fn audio_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.mixer.input_mode;
        div()
            .w_full()
            .max_w(px(520.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(heading("Levels & transmission"))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(muted(format!(
                        "Microphone {}%",
                        (self.mixer.input_gain * 100.).round()
                    )))
                    .child(
                        button("input-down", "−").on_click(cx.listener(|this, _, _, cx| {
                            this.mixer.input_gain = (this.mixer.input_gain - 0.1).max(0.);
                            this.command(Command::SetMixer(this.mixer.clone()), cx);
                        })),
                    )
                    .child(
                        button("input-up", "+").on_click(cx.listener(|this, _, _, cx| {
                            this.mixer.input_gain = (this.mixer.input_gain + 0.1).min(2.);
                            this.command(Command::SetMixer(this.mixer.clone()), cx);
                        })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(muted(format!(
                        "Output {}%",
                        (self.mixer.output_gain * 100.).round()
                    )))
                    .child(
                        button("output-down", "−").on_click(cx.listener(|this, _, _, cx| {
                            this.mixer.output_gain = (this.mixer.output_gain - 0.1).max(0.);
                            this.command(Command::SetMixer(this.mixer.clone()), cx);
                        })),
                    )
                    .child(
                        button("output-up", "+").on_click(cx.listener(|this, _, _, cx| {
                            this.mixer.output_gain = (this.mixer.output_gain + 0.1).min(2.);
                            this.command(Command::SetMixer(this.mixer.clone()), cx);
                        })),
                    ),
            )
            .child(self.mode_picker.clone())
            .when(mode == InputMode::VoiceActivity, |v| {
                v.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(muted(format!(
                            "Activation threshold: {:.0} dB",
                            self.mixer.voice_activation_threshold_db
                        )))
                        .child(button("threshold-down", "−5 dB").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.mixer.voice_activation_threshold_db =
                                    (this.mixer.voice_activation_threshold_db - 5.).max(-96.);
                                this.command(Command::SetMixer(this.mixer.clone()), cx);
                            },
                        )))
                        .child(button("threshold-up", "+5 dB").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.mixer.voice_activation_threshold_db =
                                    (this.mixer.voice_activation_threshold_db + 5.).min(0.);
                                this.command(Command::SetMixer(this.mixer.clone()), cx);
                            },
                        ))),
                )
            })
            .when(mode == InputMode::PushToTalk, |v| {
                v.child(muted(
                    "Hold F8 while this window is active, or hold the Talk button.",
                ))
                .child(
                    button("ptt-held", "Hold to talk")
                        .bg(rgb(0x6650bd))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.command(Command::SetPushToTalkPressed(true), cx)
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.command(Command::SetPushToTalkPressed(false), cx)
                            }),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.command(Command::SetPushToTalkPressed(false), cx)
                            }),
                        ),
                )
            })
            .child(
                button(
                    "mic-test",
                    if self.testing_microphone {
                        "Stop microphone test"
                    } else {
                        "Test microphone (meter only)"
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.testing_microphone = !this.testing_microphone;
                    this.command(
                        if this.testing_microphone {
                            Command::StartMicrophoneTest
                        } else {
                            Command::StopMicrophoneTest
                        },
                        cx,
                    );
                })),
            )
            .child(muted(format!(
                "Input level {:.0}% · Output level {:.0}%",
                self.levels.0 * 100.,
                self.levels.1 * 100.
            )))
            .child(
                div()
                    .h(px(6.))
                    .w_full()
                    .rounded_md()
                    .bg(rgb(0x353142))
                    .child(
                        div()
                            .h_full()
                            .w(relative(self.levels.0.clamp(0., 1.)))
                            .rounded_md()
                            .bg(rgb(0x6bc99d)),
                    ),
            )
            .when(!self.participants.is_empty(), |v| {
                v.child(heading("Participant volume"))
            })
            .children(
                self.participants
                    .iter()
                    .map(|(identity, (name, settings))| {
                        let id = identity.clone();
                        let down = id.clone();
                        let up = id.clone();
                        div()
                            .flex()
                            .gap_3()
                            .items_center()
                            .child(format!(
                                "{} · {:.0}%",
                                if name.is_empty() { identity } else { name },
                                settings.gain * 100.
                            ))
                            .child(
                                button(SharedString::from(format!("participant-down-{id}")), "−")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some((_, settings)) =
                                            this.participants.get_mut(&down)
                                        {
                                            settings.gain = (settings.gain - 0.1).max(0.);
                                            let settings = *settings;
                                            this.command(
                                                Command::SetParticipantAudio {
                                                    identity: down.clone(),
                                                    settings,
                                                },
                                                cx,
                                            );
                                        }
                                    })),
                            )
                            .child(
                                button(SharedString::from(format!("participant-up-{id}")), "+")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some((_, settings)) = this.participants.get_mut(&up)
                                        {
                                            settings.gain = (settings.gain + 0.1).min(2.);
                                            let settings = *settings;
                                            this.command(
                                                Command::SetParticipantAudio {
                                                    identity: up.clone(),
                                                    settings,
                                                },
                                                cx,
                                            );
                                        }
                                    })),
                            )
                            .child(
                                button(
                                    SharedString::from(format!("participant-mute-{id}")),
                                    if settings.muted { "Unmute" } else { "Mute" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if let Some((_, settings)) = this.participants.get_mut(&id)
                                        {
                                            settings.muted = !settings.muted;
                                            let settings = *settings;
                                            this.command(
                                                Command::SetParticipantAudio {
                                                    identity: id.clone(),
                                                    settings,
                                                },
                                                cx,
                                            );
                                        }
                                    },
                                )),
                            )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn selected_share_audio(&self, cx: &App) -> Option<SharedAudioSelection> {
        self.share_audio_picker.read(cx).selected().and_then(|id| {
            if id == "system" {
                Some(SharedAudioSelection::System)
            } else {
                id.strip_prefix("app:")
                    .filter(|pid| self.applications.iter().any(|app| app.process_id == *pid))
                    .map(|pid| SharedAudioSelection::Application {
                        process_id: pid.into(),
                    })
            }
        })
    }

    pub(super) fn shared_audio_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(heading("Share audio"))
            .when(cfg!(target_os = "linux"), |view| {
                view.child(
                    button(
                        "include-share-audio",
                        if self.share_audio_enabled {
                            "Include audio: On"
                        } else {
                            "Include audio: Off"
                        },
                    )
                    .w(px(180.))
                    .when(self.share_audio_enabled, |button| button.bg(rgb(0x6650bd)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.share_audio_enabled = !this.share_audio_enabled;
                        cx.notify();
                    })),
                )
            })
            .when(!cfg!(target_os = "linux"), |view| {
                view.child(muted("Audio sharing is unavailable on this platform."))
            })
            .when(self.share_audio_enabled, |view| {
                view.child(muted(
                    "The selected audio source starts with your screen share.",
                ))
                .child(self.share_audio_picker.clone())
                .child(
                    button("refresh-apps", "Refresh applications").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.command(Command::RefreshAudioApplications, cx);
                        },
                    )),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_3()
                        .items_center()
                        .child(muted(format!(
                            "Shared audio {:.0}%",
                            self.shared_audio_settings.gain * 100.
                        )))
                        .child(button("share-gain-down", "−").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.shared_audio_settings.gain =
                                    (this.shared_audio_settings.gain - 0.1).max(0.);
                                this.command(
                                    Command::SetSharedAudio(this.shared_audio_settings),
                                    cx,
                                );
                            },
                        )))
                        .child(button("share-gain-up", "+").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.shared_audio_settings.gain =
                                    (this.shared_audio_settings.gain + 0.1).min(2.);
                                this.command(
                                    Command::SetSharedAudio(this.shared_audio_settings),
                                    cx,
                                );
                            },
                        )))
                        .child(
                            button(
                                "share-audio-mute",
                                if self.shared_audio_settings.muted {
                                    "Unmute shared audio"
                                } else {
                                    "Mute shared audio"
                                },
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.shared_audio_settings.muted =
                                    !this.shared_audio_settings.muted;
                                this.command(
                                    Command::SetSharedAudio(this.shared_audio_settings),
                                    cx,
                                );
                            })),
                        ),
                )
            })
            .into_any_element()
    }

    pub(super) fn screens(&self, cx: &mut Context<Self>) -> AnyElement {
        let system_picker = self
            .sources
            .iter()
            .find(|source| source.kind == ScreenSourceKind::SystemPicker)
            .cloned();
        let selected = self
            .screen_picker
            .read(cx)
            .selected()
            .and_then(|id| {
                self.sources.iter().find(|source| {
                    source.kind != ScreenSourceKind::SystemPicker && screen_source_id(source) == id
                })
            })
            .cloned();
        div()
            .id("screen-sources")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(heading("Share your screen"))
            .child(muted(if system_picker.is_some() {
                "Choose your quality, then select a display or window in the system dialog."
            } else {
                "Choose a display or window and your sharing quality."
            }))
            .when(system_picker.is_none(), |view| {
                view.child(self.screen_picker.clone())
            })
            .child(self.fps_picker.clone())
            .child(self.resolution_picker.clone())
            .child(self.shared_audio_controls(cx))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .when_some(system_picker, |view, source| {
                        view.child(
                            button("choose-screen-source", "Choose display or window…")
                                .bg(rgb(0x6f56d7))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.start_screen_share(source.clone(), cx);
                                })),
                        )
                    })
                    .when_some(selected, |view, source| {
                        view.child(
                            button("start-screen-share", "Share screen")
                                .bg(rgb(0x6f56d7))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.start_screen_share(source.clone(), cx);
                                })),
                        )
                    })
                    .child(
                        button("refresh-sources", "Refresh sources").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.command(Command::RefreshScreenSources, cx);
                            },
                        )),
                    ),
            )
            .child(button("cancel-share", "Back to chat").on_click(cx.listener(
                |this, _, _, cx| {
                    this.pane = Pane::Chat;
                    cx.notify();
                },
            )))
            .into_any_element()
    }
}
