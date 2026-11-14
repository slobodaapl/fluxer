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
                                bio:(mode=="long").then(||"A long profile biography 🙂 with enough text to check wrapping and scrolling. ".repeat(25)),
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
