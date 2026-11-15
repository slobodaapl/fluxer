// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::controls::{Icon, icon, icon_button, quiet_button, slider, toggle};

impl Fluxer {
    pub(super) fn voice_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let joined = self.state.voice.channel_id.is_some();
        let connected = joined && !self.voice_connecting;
        let active = joined || self.voice_connecting;
        let can_share = self
            .state
            .voice
            .channel_id
            .zip(self.state.current_user.as_ref())
            .is_some_and(|(channel, user)| {
                self.state
                    .permissions_for(channel, user.id)
                    .contains(fluxer_core::Permissions::STREAM)
            });
        let name = self
            .state
            .current_user
            .as_ref()
            .map(|user| user.display_name().to_owned())
            .unwrap_or_else(|| "Your account".into());
        let account_avatar = self
            .state
            .current_user
            .as_ref()
            .map(|user| {
                crate::avatar_images::user_avatar(user, self.media_endpoint.as_deref(), 24.)
            })
            .unwrap_or_else(|| voice_avatar(&name, Snowflake::default(), false, 24.));
        let tooltip: SharedString = name.clone().into();
        div()
            .id("account-voice-dock")
            .when_some(self.state.voice.channel_id, |view, channel| {
                crate::context_actions::trigger(
                    view,
                    crate::context_actions::Target::SelfVoice(channel),
                    cx,
                )
            })
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .px_2()
            .py_2()
            .when(active, |view| {
                view.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .size(px(6.))
                                .mr_1()
                                .rounded_full()
                                .bg(rgb(if connected { 0x6bc99d } else { 0xd4b76b })),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(if connected { 0x6bc99d } else { TEXT }))
                                        .text_ellipsis()
                                        .child(if self.voice_connecting {
                                            self.voice_status.clone()
                                        } else if !self.audio_device_status.is_empty() {
                                            "Audio device recovering".into()
                                        } else {
                                            "Voice connected".into()
                                        }),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(rgb(MUTED))
                                        .text_ellipsis()
                                        .child(
                                            self.state
                                                .voice
                                                .channel_id
                                                .map(|id| self.channel_name(id))
                                                .unwrap_or_else(|| "Voice connection".into()),
                                        ),
                                ),
                        )
                        .when((connected && can_share) || self.sharing, |view| {
                            view.child(
                                icon_button(
                                    "share",
                                    if self.sharing {
                                        Icon::Stop
                                    } else {
                                        Icon::ScreenShare
                                    },
                                    if self.sharing {
                                        "Stop sharing your screen"
                                    } else {
                                        "Share your screen"
                                    },
                                )
                                .when(self.sharing, |view| view.text_color(rgb(0x6bc99d)))
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        if this.sharing {
                                            this.command(Command::StopShare, cx);
                                        } else {
                                            this.pane = Pane::Screens;
                                            this.command(Command::RefreshScreenSources, cx);
                                            this.command(Command::RefreshAudioApplications, cx);
                                        }
                                    },
                                )),
                            )
                        })
                        .child(
                            icon_button(
                                "leave",
                                Icon::PhoneOff,
                                if joined {
                                    "Disconnect from voice"
                                } else {
                                    "Cancel voice connection"
                                },
                            )
                            .text_color(rgb(0xe79a9e))
                            .on_click(
                                cx.listener(|this, _, _, cx| this.command(Command::LeaveVoice, cx)),
                            ),
                        ),
                )
            })
            .when(self.sharing, |view| {
                view.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.))
                                .text_color(rgb(MUTED))
                                .child(if self.shared_audio {
                                    if self.shared_audio_settings.muted {
                                        "Screen sharing · audio muted"
                                    } else {
                                        "Screen and audio sharing"
                                    }
                                } else {
                                    "Screen sharing"
                                }),
                        )
                        .when(self.shared_audio, |view| {
                            view.child(
                                icon_button(
                                    "active-share-audio-mute",
                                    if self.shared_audio_settings.muted {
                                        Icon::VolumeOff
                                    } else {
                                        Icon::Volume
                                    },
                                    if self.shared_audio_settings.muted {
                                        "Unmute shared audio"
                                    } else {
                                        "Mute shared audio"
                                    },
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.shared_audio_settings.muted =
                                            !this.shared_audio_settings.muted;
                                        this.command(
                                            Command::SetSharedAudio(this.shared_audio_settings),
                                            cx,
                                        );
                                    },
                                )),
                            )
                        }),
                )
            })
            .child(
                div()
                    .h(px(36.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .id("voice-account-identity")
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .tooltip(move |_, cx| {
                                cx.new(|_| AccountTooltip(tooltip.clone())).into()
                            })
                            .child(
                                div()
                                    .id("account-profile-avatar")
                                    .size(px(24.))
                                    .child(account_avatar)
                                    .when_some(self.state.current_user.clone(), |view, user| {
                                        self.profile_trigger(view, user, None, false, cx)
                                    }),
                            )
                            .child(
                                div().flex_1().min_w_0().flex().items_center().child(
                                    div()
                                        .id("account-profile-name")
                                        .min_w_0()
                                        .max_w_full()
                                        .text_ellipsis()
                                        .text_size(px(12.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(name)
                                        .when_some(
                                            self.state.current_user.clone(),
                                            |view, user| {
                                                self.profile_trigger(view, user, None, false, cx)
                                            },
                                        ),
                                ),
                            ),
                    )
                    .child(
                        icon_button(
                            "mute",
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
                            "deaf",
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
                        icon_button("voice-settings", Icon::Settings, "Voice and audio settings")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings(settings_view::Section::Voice, window, cx);
                            })),
                    ),
            )
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
        let connected = self.state.voice.channel_id == Some(channel);
        let guild = self
            .state
            .channels
            .get(&channel)
            .and_then(|channel| channel.guild_id);
        div()
            .flex_shrink_0()
            .border_b_1()
            .border_color(rgb(PANEL))
            .flex()
            .flex_col()
            .child(
                div()
                    .px_5()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon(Icon::Headphones, 14.).text_color(rgb(MUTED)))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("In voice · {}", participants.len())),
                    )
                    .child(div().flex_1())
                    .child(
                        quiet_button(
                            ("viewed-voice-call", channel.0),
                            if connected {
                                "Disconnect"
                            } else {
                                "Join voice"
                            },
                        )
                        .text_color(rgb(if connected { MUTED } else { ACCENT }))
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
                    .max_h(px(180.))
                    .overflow_y_scroll()
                    .px_5()
                    .pb_3()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .when(participants.is_empty(), |view| {
                        view.child(
                            div()
                                .py_3()
                                .text_size(px(12.))
                                .text_color(rgb(MUTED))
                                .child("No one is here yet."),
                        )
                    })
                    .children(
                        participants
                            .into_iter()
                            .enumerate()
                            .map(|(index, participant)| {
                                let menu_target = self.voice_context_target(
                                    Some(channel),
                                    Some(participant.user_id),
                                    participant.livekit_identity.clone(),
                                );
                                let speaking = self.participant_speaking(&participant, channel);
                                let stream =
                                    participant.livekit_identity.clone().filter(|identity| {
                                        participant.self_stream
                                            && connected
                                            && self.available_streams.contains(identity)
                                    });
                                div()
                                    .id(("voice-participant-card", index))
                                    .when_some(menu_target, |view, target| {
                                        crate::context_actions::trigger(view, target, cx)
                                    })
                                    .w(px(200.))
                                    .min_h(px(58.))
                                    .px_2()
                                    .py_2()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(rgb(if speaking { 0x559b7c } else { PANEL }))
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        self.profile_trigger(
                                            div()
                                                .id(("voice-profile-avatar", index))
                                                .size(px(32.))
                                                .child(crate::avatar_images::voice_avatar(
                                                    &participant,
                                                    guild,
                                                    self.media_endpoint.as_deref(),
                                                    speaking,
                                                    32.,
                                                )),
                                            self.profile_user(
                                                participant.user_id,
                                                &participant.display_name,
                                            ),
                                            guild,
                                            false,
                                            cx,
                                        ),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .items_start()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .id(("voice-profile-name", index))
                                                    .max_w_full()
                                                    .text_size(px(12.))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_ellipsis()
                                                    .child(format!(
                                                        "{}{}",
                                                        participant.display_name,
                                                        if participant.is_current_user {
                                                            " (you)"
                                                        } else {
                                                            ""
                                                        }
                                                    ))
                                                    .map(|view| {
                                                        self.profile_trigger(
                                                            view,
                                                            self.profile_user(
                                                                participant.user_id,
                                                                &participant.display_name,
                                                            ),
                                                            guild,
                                                            false,
                                                            cx,
                                                        )
                                                    }),
                                            )
                                            .child(voice_badges(&participant, false)),
                                    )
                                    .when_some(stream, |view, identity| {
                                        let watching = self.viewer.desired.contains(&identity);
                                        view.child(
                                            icon_button(
                                                ("watch-participant-stream", index),
                                                if watching {
                                                    Icon::Stop
                                                } else {
                                                    Icon::ScreenShare
                                                },
                                                if watching {
                                                    "Stop watching this stream"
                                                } else {
                                                    "Watch live stream"
                                                },
                                            )
                                            .text_color(rgb(if watching {
                                                0xe79a9e
                                            } else {
                                                ACCENT
                                            }))
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.watch_stream(
                                                        identity.clone(),
                                                        !watching,
                                                        cx,
                                                    )
                                                }),
                                            ),
                                        )
                                    })
                            }),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn voice_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        div().id("settings-body").flex_1().min_w_0().h_full().overflow_y_scroll().p_6()
            .child(div().w_full().max_w(px(720.)).flex().flex_col().gap_5()
                .child(heading("Voice & audio"))
                .child(audio_description("Changes apply immediately. Microphone testing starts only when you choose to test it."))
                .child(audio_section("Devices").child(div().flex().items_center().justify_between().child(audio_description("Choose where you speak and listen."))
                    .child(icon_button("refresh-devices",Icon::Refresh,"Refresh audio devices").on_click(cx.listener(|this,_,_,cx|this.command(Command::RefreshDevices,cx)))))
                    .child(self.input_picker.clone()).child(self.output_picker.clone())
                    .when(!self.audio_device_status.is_empty(), |view| view.child(audio_description(self.audio_device_status.clone()))))
                .child(self.audio_controls(cx))
                .child(audio_section("Microphone processing").children([
                    ("aec","Echo cancellation","Reduce echo from sound playing through your speakers.",self.dsp.echo_cancellation),
                    ("ns","Noise suppression","Reduce steady background noise from your microphone.",self.dsp.noise_suppression),
                    ("agc","Automatic gain control","Keep your microphone at a consistent listening level.",self.dsp.auto_gain_control),
                ].into_iter().map(|(id,label,description,enabled)|{
                    audio_setting_row(label,description).child(toggle(id,enabled,label).on_click(cx.listener(move|this,_,_,cx|{
                        match id{"aec"=>this.dsp.echo_cancellation= !this.dsp.echo_cancellation,"ns"=>this.dsp.noise_suppression= !this.dsp.noise_suppression,_=>this.dsp.auto_gain_control= !this.dsp.auto_gain_control}
                        this.command(Command::SetDsp(this.dsp.clone()),cx);
                    })))
                })))
                .child(audio_description("Sliders: arrow keys fine-tune; Home and End set the limits."))
)
            .into_any_element()
    }

    pub(super) fn audio_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.mixer.input_mode;
        div().flex().flex_col().gap_5()
            .child(audio_section("Levels")
                .child(audio_value("Microphone volume",format!("{:.0}%",self.mixer.input_gain*100.)))
                .child(slider("input-volume",self.mixer.input_gain,0.,2.,0.01,"Microphone volume",cx.listener(|this,value:&f32,_,cx|{this.mixer.input_gain= *value;this.command(Command::SetMixer(this.mixer.clone()),cx);})))
                .child(audio_value("Output volume",format!("{:.0}%",self.mixer.output_gain*100.)))
                .child(slider("output-volume",self.mixer.output_gain,0.,2.,0.01,"Output volume",cx.listener(|this,value:&f32,_,cx|{this.mixer.output_gain= *value;this.command(Command::SetMixer(this.mixer.clone()),cx);}))))
            .child(audio_section("Input mode").child(self.mode_picker.clone())
                .when(mode==InputMode::VoiceActivity,|view|view
                    .child(audio_value("Activation threshold",format!("{:.0} dB",self.mixer.voice_activation_threshold_db)))
                    .child(slider("activation-threshold",self.mixer.voice_activation_threshold_db,-96.,0.,1.,"Voice activation threshold",cx.listener(|this,value:&f32,_,cx|{this.mixer.voice_activation_threshold_db= *value;this.command(Command::SetMixer(this.mixer.clone()),cx);})))
                    .child(audio_description("Lower thresholds pick up quieter speech.")))
                .when(mode==InputMode::PushToTalk,|view|view
                    .child(audio_description("Hold F8 while this window is active, or press and hold below."))
                    .child(quiet_button("ptt-held","Hold to talk").on_mouse_down(MouseButton::Left,cx.listener(|this,_,_,cx|this.command(Command::SetPushToTalkPressed(true),cx)))
                        .on_mouse_up(MouseButton::Left,cx.listener(|this,_,_,cx|this.command(Command::SetPushToTalkPressed(false),cx)))
                        .on_mouse_up_out(MouseButton::Left,cx.listener(|this,_,_,cx|this.command(Command::SetPushToTalkPressed(false),cx))))))
            .child(audio_section("Microphone test")
                .child(audio_setting_row("Check your input","Uses your selected microphone for a level meter. Audio is not played back.")
                    .child(quiet_button("mic-test",if self.testing_microphone{"Stop test"}else{"Test microphone"})
                        .when(self.testing_microphone,|view|view.text_color(rgb(0xe79a9e)))
                        .on_click(cx.listener(|this,_,_,cx|{this.testing_microphone= !this.testing_microphone;this.command(if this.testing_microphone{Command::StartMicrophoneTest}else{Command::StopMicrophoneTest},cx);}))))
                .child(audio_meter(self.levels.0))
                .child(audio_description(format!("Input {:.0}% · Output {:.0}%",self.levels.0*100.,self.levels.1*100.))))
            .when(!self.participants.is_empty(),|view|view.child(audio_section("People in your call").child(audio_description("These controls adjust each device. Right-click a person in the call to change their user volume.")).children(self.participants.iter().map(|(identity,(name,_settings))|{
                let settings = self.context.connection_audio.get(identity).copied().unwrap_or_default();
                let gain_id=identity.clone();let mute_id=identity.clone();
                div().flex().flex_col().gap_2().py_2()
                    .child(div().flex().items_center().gap_2()
                        .child(div().flex_1().min_w_0().text_ellipsis().text_size(px(13.)).child(if name.is_empty(){identity.clone()}else{name.clone()}))
                        .child(div().text_size(px(12.)).text_color(rgb(MUTED)).child(format!("{:.0}%",settings.gain*100.)))
                        .child(icon_button(SharedString::from(format!("participant-mute-{identity}")),if settings.muted{Icon::VolumeOff}else{Icon::Volume},if settings.muted{"Unmute participant for you"}else{"Mute participant for you"})
                            .when(settings.muted,|view|view.text_color(rgb(0xe79a9e)))
                            .on_click(cx.listener(move|this,_,_,cx|{let mut settings=this.context.connection_audio.get(&mute_id).copied().unwrap_or_default();settings.muted = !settings.muted;this.set_connection_audio(&mute_id,settings,cx);}))))
                    .child(slider(SharedString::from(format!("participant-volume-{identity}")),settings.gain,0.,2.,0.01,"Connection volume",cx.listener(move|this,value:&f32,_,cx|{let mut settings=this.context.connection_audio.get(&gain_id).copied().unwrap_or_default();settings.gain = *value;this.set_connection_audio(&gain_id,settings,cx);})))
            }))))
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
        audio_section("Audio")
            .when(cfg!(target_os = "linux"), |view| {
                view.child(
                    audio_setting_row(
                        "Include audio",
                        "Start the selected audio source with your screen share.",
                    )
                    .child(
                        toggle(
                            "include-share-audio",
                            self.share_audio_enabled,
                            "Include screen share audio",
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.share_audio_enabled = !this.share_audio_enabled;
                            cx.notify();
                        })),
                    ),
                )
            })
            .when(!cfg!(target_os = "linux"), |view| {
                view.child(audio_description(
                    "Audio sharing is unavailable on this platform.",
                ))
            })
            .when(self.share_audio_enabled, |view| {
                view.child(self.share_audio_picker.clone())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(audio_description("Choose system audio or an application."))
                            .child(div().flex_1())
                            .child(
                                icon_button(
                                    "refresh-apps",
                                    Icon::Refresh,
                                    "Refresh audio applications",
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.command(Command::RefreshAudioApplications, cx)
                                    },
                                )),
                            ),
                    )
                    .child(audio_value(
                        "Shared audio volume",
                        format!("{:.0}%", self.shared_audio_settings.gain * 100.),
                    ))
                    .child(slider(
                        "shared-audio-volume",
                        self.shared_audio_settings.gain,
                        0.,
                        2.,
                        0.01,
                        "Shared audio volume",
                        cx.listener(|this, value: &f32, _, cx| {
                            this.shared_audio_settings.gain = *value;
                            this.command(Command::SetSharedAudio(this.shared_audio_settings), cx);
                        }),
                    ))
                    .child(
                        audio_setting_row(
                            "Mute shared audio",
                            "Independent of your microphone and deafen controls.",
                        )
                        .child(
                            toggle(
                                "share-audio-mute",
                                self.shared_audio_settings.muted,
                                "Mute shared audio",
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
        div().id("screen-sources").flex_1().min_w_0().h_full().overflow_y_scroll().p_6()
            .child(div().w_full().max_w(px(720.)).flex().flex_col().gap_5()
                .child(div().flex().items_center().gap_3().child(heading("Share your screen")).child(div().flex_1())
                    .child(icon_button("cancel-share",Icon::Back,"Back to chat").on_click(cx.listener(|this,_,_,cx|{this.pane=Pane::Chat;cx.notify();}))))
                .child(audio_description(if system_picker.is_some(){"Choose your quality and audio, then select a display or window in the system dialog."}else{"Choose a display or window, then set the quality and audio you want to share."}))
                .when(system_picker.is_none(),|view|view.child(self.screen_picker.clone()))
                .child(audio_section("Video quality").child(self.fps_picker.clone()).child(self.resolution_picker.clone()))
                .child(self.shared_audio_controls(cx))
                .child(div().flex().items_center().gap_2()
                    .when_some(system_picker,|view,source|view.child(quiet_button("choose-screen-source","Choose display or window…").bg(rgb(ACCENT)).text_color(rgb(TEXT)).on_click(cx.listener(move|this,_,_,cx|this.start_screen_share(source.clone(),cx)))))
                    .when_some(selected,|view,source|view.child(quiet_button("start-screen-share","Share screen").bg(rgb(ACCENT)).text_color(rgb(TEXT)).on_click(cx.listener(move|this,_,_,cx|this.start_screen_share(source.clone(),cx)))))
                    .child(icon_button("refresh-sources",Icon::Refresh,"Refresh available sources").on_click(cx.listener(|this,_,_,cx|this.command(Command::RefreshScreenSources,cx))))))
            .into_any_element()
    }
}

fn audio_description(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .text_color(rgb(MUTED))
        .child(text.into())
}
fn audio_section(title: &'static str) -> Div {
    div().flex().flex_col().gap_3().child(
        div()
            .text_size(px(12.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(title),
    )
}
fn audio_setting_row(title: &'static str, description: &'static str) -> Div {
    div().flex().items_center().gap_4().py_2().child(
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_size(px(13.)).child(title))
            .child(audio_description(description)),
    )
}
fn audio_value(title: &'static str, value: String) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(div().text_size(px(13.)).child(title))
        .child(div().text_size(px(12.)).text_color(rgb(MUTED)).child(value))
}
fn audio_meter(level: f32) -> Div {
    div()
        .h(px(5.))
        .w_full()
        .rounded_full()
        .bg(rgb(PANEL))
        .child(
            div()
                .h_full()
                .rounded_full()
                .w(relative(level.clamp(0., 1.)))
                .bg(rgb(0x6bc99d)),
        )
}

struct AccountTooltip(SharedString);
impl Render for AccountTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(RAIL))
            .text_color(rgb(TEXT))
            .text_size(px(12.))
            .child(self.0.clone())
    }
}
