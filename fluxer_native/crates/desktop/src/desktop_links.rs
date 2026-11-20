// SPDX-License-Identifier: AGPL-3.0-or-later
//! Channel and message routes from the classic AppProtocol and DeepLinkUtils.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DesktopLink {
    scope: Option<Snowflake>,
    channel: Snowflake,
    message: Option<Snowflake>,
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
        if segments.next()? != "channels" {
            return None;
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
        let channel = id(segments.next()?)?;
        let message = match segments.next() {
            Some(part) => Some(id(part)?),
            None => None,
        };
        segments.next().is_none().then_some(Self {
            scope,
            channel,
            message,
        })
    }
    pub fn open(self, view: &mut Fluxer, cx: &mut Context<Fluxer>) {
        view.follow_message_channel(self.channel, Some(self.scope), self.message, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{DesktopLink, Snowflake};
    #[test]
    fn decodes_reference_route_forms_and_keeps_full_snowflake_precision() {
        let target = DesktopLink {
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
            Some(DesktopLink {
                scope: None,
                channel: Snowflake(42),
                message: None
            })
        );
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
            "fluxer:/invite/123",
        ] {
            assert_eq!(DesktopLink::parse(url), None, "{url}");
        }
    }
}
