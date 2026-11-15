// SPDX-License-Identifier: AGPL-3.0-or-later
//! Explicit offline-only profile fixture; never mixed into a live session.
use super::*;
impl Fluxer {
    pub(super) fn schedule_profile_demo(&self, window: &Window, cx: &mut Context<Self>) {
        if !self.demo || !std::env::args().any(|arg| arg == "--demo") {
            return;
        }
        let Ok(mode) = std::env::var("FLUXER_PROFILE_DEMO") else {
            return;
        };
        if matches!(mode.as_str(), "loading" | "loading-error") {
            self.schedule_profile_background_demo(mode == "loading-error", window, cx);
            return;
        }
        if !matches!(
            mode.as_str(),
            "basic" | "edge" | "long" | "error" | "limited" | "webhook" | "keyboard" | "hitboxes"
        ) {
            return;
        }
        let handle = window.window_handle();
        cx.spawn(async move|entity,cx| {
            Timer::after(Duration::from_millis(800)).await;
            let _=handle.update(cx,|_,window,cx| {
                let _=entity.update(cx,|view,cx| {
                    view.focus.focus(window);
                    if mode=="keyboard" || mode=="hitboxes" {return;}
                    let size=window.viewport_size();
                    let anchor=if mode=="edge" {point(size.width-px(4.),size.height-px(4.))} else {point(px(330.),px(100.))};
                    let user=view.profile_user(Snowflake(101),"Alex");
                    view.open_user_profile(user,Some(Snowflake(1)),mode=="webhook",anchor,window,cx);
                });
            });
            if !matches!(mode.as_str(),"long"|"error"|"limited") {return;}
            Timer::after(Duration::from_millis(400)).await;
            let _=handle.update(cx,|_,_,cx| {
                let _=entity.update(cx,|view,cx| {
                    let user=view.profile_user(Snowflake(101),"Alex");
                    let (status,profile)=if mode=="error" {
                        (fluxer_client::ProfileStatus::Failed("Offline fixture error".into()),None)
                    } else {
                        let member=view.state.members.get(&(Snowflake(1),Snowflake(101))).cloned();
                        (fluxer_client::ProfileStatus::Ready,Some(fluxer_core::UserProfile {
                            user,user_profile:fluxer_core::ProfileDetails {
                                bio:(mode=="long").then(||format!("Profile website: https://profile-review.example/about\n\n{}", "A long profile biography 🙂 with enough text to check wrapping and scrolling. ".repeat(25))),
                                pronouns:(mode=="long").then(||"they/them".into()),
                                banner_color:Some(0x7860bc),..Default::default()
                            },guild_member:member,profile_limited:mode=="limited",..Default::default()
                        }))
                    };
                    view.profile_ui.popup.update(cx,|popup,cx|popup.apply_snapshot(fluxer_client::ProfileSnapshot {
                        user_id:Snowflake(101),guild_id:Some(Snowflake(1)),status,profile:profile.map(Box::new),
                    },cx));
                });
            });
        }).detach();
    }
}

impl Fluxer {
    fn schedule_profile_background_demo(
        &self,
        fail: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(endpoint) = delayed_profile_images(fail) else {
            return;
        };
        let handle = window.window_handle();
        cx.spawn(async move |entity, cx| {
            Timer::after(Duration::from_millis(800)).await;
            let mut fixture = None;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = entity.update(cx, |view, cx| {
                    let mut user = view.profile_user(Snowflake(101), "Alex");
                    user.avatar = Some("fixture_avatar".into());
                    let open = profile_view::ProfileOpen {
                        user: user.clone(),
                        guild_id: None,
                        member: None,
                        roles: vec![],
                        media_endpoint: Some(endpoint),
                        community_name: None,
                        anchor: point(px(330.), px(100.)),
                        webhook: false,
                        can_message: false,
                        blocked: false,
                        friend_action: None,
                        can_relationship: false,
                    };
                    let snapshot = fluxer_client::ProfileSnapshot {
                        user_id: user.id,
                        guild_id: None,
                        status: fluxer_client::ProfileStatus::Ready,
                        profile: Some(Box::new(fluxer_core::UserProfile {
                            user,
                            user_profile: fluxer_core::ProfileDetails {
                                banner: Some("fixture_banner".into()),
                                accent_color: Some(0x5865f2),
                                bio: Some(
                                    "Delayed image fixture. Reopens with the same cached assets."
                                        .into(),
                                ),
                                ..Default::default()
                            },
                            ..Default::default()
                        })),
                    };
                    view.profile_ui.popup.update(cx, |popup, cx| {
                        popup.open(open.clone(), window, cx);
                        popup.apply_snapshot(snapshot.clone(), cx);
                    });
                    fixture = Some((open, snapshot));
                });
            });
            Timer::after(Duration::from_secs(3)).await;
            if let Some((open, snapshot)) = fixture {
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = entity.update(cx, |view, cx| {
                        view.profile_ui.popup.update(cx, |popup, cx| {
                            popup.dismiss(window, cx);
                            popup.open(open, window, cx);
                            popup.apply_snapshot(snapshot, cx);
                            eprintln!("PROFILE_ASSET_REOPEN");
                        })
                    });
                });
            }
        })
        .detach();
    }
}
fn delayed_profile_images(fail: bool) -> Option<String> {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::atomic::{AtomicUsize, Ordering},
    };
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let endpoint = format!("http://{}", listener.local_addr().ok()?);
    std::thread::spawn(move || {
        let banners = Arc::new(AtomicUsize::new(0));
        let avatars = Arc::new(AtomicUsize::new(0));
        for mut stream in listener.incoming().flatten() {
            let banners = banners.clone();
            let avatars = avatars.clone();
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                let mut request = [0u8; 4096];
                let Ok(count) = stream.read(&mut request) else {
                    return;
                };
                let banner =
                    String::from_utf8_lossy(&request[..count]).starts_with("GET /banners/");
                let request_count = if banner {
                    banners.fetch_add(1, Ordering::Relaxed)
                } else {
                    avatars.fetch_add(1, Ordering::Relaxed)
                } + 1;
                eprintln!(
                    "PROFILE_ASSET_REQUEST kind={} count={request_count}",
                    if banner { "banner" } else { "avatar" }
                );
                std::thread::sleep(Duration::from_millis(if banner { 1200 } else { 300 }));
                let bytes = include_bytes!("../../../../fluxer_app/src/media/images/avatars/1.png");
                let status = if fail && banner {
                    "404 Not Found"
                } else {
                    "200 OK"
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                let _ = stream.write_all(bytes);
            });
        }
    });
    Some(endpoint)
}
