// SPDX-License-Identifier: AGPL-3.0-or-later
//! Routes from the classic AppProtocol and DeepLinkUtils.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DesktopLink {
    Home,
    Channel {
        scope: Option<Snowflake>,
        channel: Snowflake,
        message: Option<Snowflake>,
    },
    Invite(String),
}

impl DesktopLink {
    pub fn parse(raw: &str) -> Option<Self> {
        if raw.len() > 512
            || raw
                .chars()
                .any(|c| c.is_control() || matches!(c, '"' | '\'' | '<' | '>' | '\\' | '|'))
        {
            return None;
        }
        let (scheme, _) = raw.split_once(':')?;
        if !scheme.eq_ignore_ascii_case("fluxer") {
            return None;
        }
        let url = reqwest::Url::parse(raw).ok()?;
        if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
            return None;
        }
        let authority = url
            .host_str()
            .filter(|host| *host != "-" && !host.is_empty());
        let mut segments = authority
            .into_iter()
            .chain(url.path().split('/').filter(|part| !part.is_empty()));
        match segments.next()? {
            "channels" => {}
            "invite" => {
                let code = segments.next()?;
                if !fluxer_protocol::valid_invite_code(code)
                    || !matches!(segments.next(), None | Some("login"))
                    || segments.next().is_some()
                {
                    return None;
                }
                return Some(Self::Invite(code.to_owned()));
            }
            _ => return None,
        }
        let id = |part: &str| {
            if !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            part.parse::<u64>()
                .ok()
                .filter(|id| *id != 0)
                .map(Snowflake)
        };
        let scope = match segments.next()? {
            "@me" => None,
            part => Some(id(part)?),
        };
        let channel = match segments.next() {
            Some(part) => id(part)?,
            None if scope.is_none() => return Some(Self::Home),
            None => return None,
        };
        let message = match segments.next() {
            Some(part) => Some(id(part)?),
            None => None,
        };
        segments.next().is_none().then_some(Self::Channel {
            scope,
            channel,
            message,
        })
    }
    pub fn ready(&self, view: &Fluxer) -> bool {
        !matches!(self, Self::Invite(_)) || view.can_open_linked_invite()
    }

    pub fn open(self, view: &mut Fluxer, cx: &mut Context<Fluxer>) {
        match self {
            Self::Home => {
                view.selected_guild = None;
                view.pane = Pane::Friends;
                cx.notify();
            }
            Self::Channel {
                scope,
                channel,
                message,
            } => view.follow_message_channel(channel, Some(scope), message, cx),
            Self::Invite(code) => view.open_linked_invite(&code, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DesktopLink, Snowflake};
    #[test]
    fn decodes_reference_route_forms_and_keeps_full_snowflake_precision() {
        let target = DesktopLink::Channel {
            scope: Some(Snowflake(u64::MAX)),
            channel: Snowflake(12),
            message: Some(Snowflake(34)),
        };
        for url in [
            "fluxer:/channels/18446744073709551615/12/34",
            "fluxer://channels/18446744073709551615/12/34",
            "FLUXER://-/channels/18446744073709551615/12/34?jump=1#fragment",
        ] {
            assert_eq!(DesktopLink::parse(url), Some(target.clone()), "{url}");
        }
        assert_eq!(
            DesktopLink::parse("fluxer:///channels/@me/42"),
            Some(DesktopLink::Channel {
                scope: None,
                channel: Snowflake(42),
                message: None
            })
        );
    }

    #[test]
    fn decodes_home_and_invite_deep_links() {
        assert_eq!(
            DesktopLink::parse("fluxer:/channels/@me"),
            Some(DesktopLink::Home)
        );
        for url in [
            "fluxer:/invite/Letters-123",
            "fluxer://invite/Letters-123/login",
            "fluxer://-/invite/Letters-123?action=login",
        ] {
            assert_eq!(
                DesktopLink::parse(url),
                Some(DesktopLink::Invite("Letters-123".into()))
            );
        }
    }

    #[test]
    fn rejects_non_routes_and_ambiguous_ids() {
        for url in [
            "https://fluxer.com/channels/1/2",
            "fluxer://attacker@channels/1/2",
            "fluxer://channels:80/1/2",
            "fluxer:/channels/1/0",
            "fluxer:/channels/1/+2",
            "fluxer:/channels/1/18446744073709551616",
            "fluxer:/channels/1/2/%33",
            "fluxer:/channels/1/2/3/4",
            "fluxer:/channels/1/2\n",
            "fluxer:/invite/a%2Fb",
            "fluxer:/invite/abc/extra",
            "fluxer:/invite/abc/login/extra",
        ] {
            assert_eq!(DesktopLink::parse(url), None, "{url}");
        }
    }
}
