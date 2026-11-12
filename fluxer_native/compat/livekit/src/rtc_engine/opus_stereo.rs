// SPDX-License-Identifier: Apache-2.0
//! Fluxer compatibility: mirror the bundled JS client's per-track Opus stereo
//! negotiation, including dual-PC publisher offers/answers and subscriber answers.
use std::collections::HashSet;
fn sections(sdp: &str) -> (Vec<Vec<String>>, &str) {
    let eol = if sdp.contains("\r\n") { "\r\n" } else { "\n" };
    let mut sections = vec![Vec::new()];
    for line in sdp.split(eol) {
        if line.starts_with("m=") {
            sections.push(Vec::new());
        }
        sections.last_mut().unwrap().push(line.to_owned());
    }
    (sections, eol)
}
fn opus_payloads(section: &[String]) -> Vec<String> {
    section
        .iter()
        .filter_map(|line| {
            let mut parts = line.strip_prefix("a=rtpmap:")?.split_whitespace();
            let payload = parts.next()?;
            let codec = parts.next()?.to_ascii_lowercase();
            codec.starts_with("opus/48000/").then(|| payload.to_owned())
        })
        .collect()
}
fn mid(section: &[String]) -> Option<&str> {
    section.iter().find_map(|line| line.strip_prefix("a=mid:"))
}
fn parameter_is(config: &str, key: &str, value: &str) -> bool {
    config.split(';').any(|p| {
        p.trim()
            .split_once('=')
            .is_some_and(|(k, v)| k.trim().eq_ignore_ascii_case(key) && v.trim() == value)
    })
}
pub(super) fn remote_stereo_mids(sdp: &str) -> HashSet<String> {
    let (sections, _) = sections(sdp);
    sections
        .into_iter()
        .filter(|section| {
            section
                .first()
                .is_some_and(|line| line.starts_with("m=audio "))
        })
        .filter_map(|section| {
            let stereo = opus_payloads(&section).into_iter().any(|payload| {
                section.iter().any(|line| {
                    line.strip_prefix(&format!("a=fmtp:{payload} "))
                        .is_some_and(|config| parameter_is(config, "sprop-stereo", "1"))
                })
            });
            if stereo {
                mid(&section).map(str::to_owned)
            } else {
                None
            }
        })
        .collect()
}
fn stereo_parameters(config: &str) -> String {
    let mut parameters = config
        .split(';')
        .map(str::trim)
        .filter(|parameter| {
            !parameter.is_empty()
                && !parameter.split_once('=').is_some_and(|(key, _)| {
                    key.trim().eq_ignore_ascii_case("stereo")
                        || key.trim().eq_ignore_ascii_case("sprop-stereo")
                })
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    parameters.extend(["stereo=1".into(), "sprop-stereo=1".into()]);
    parameters.join(";")
}
pub(super) fn munge_selected(
    sdp: &str,
    mids: &HashSet<String>,
    track_ids: &HashSet<String>,
) -> String {
    let (mut sections, eol) = sections(sdp);
    for section in &mut sections {
        if !section
            .first()
            .is_some_and(|line| line.starts_with("m=audio "))
        {
            continue;
        }
        let selected = mid(section).is_some_and(|id| mids.contains(id))
            || section.iter().any(|line| {
                line.strip_prefix("a=msid:")
                    .and_then(|value| value.split_whitespace().nth(1))
                    .is_some_and(|id| track_ids.contains(id))
            });
        if !selected {
            continue;
        }
        for payload in opus_payloads(section) {
            let prefix = format!("a=fmtp:{payload} ");
            if let Some(line) = section.iter_mut().find(|line| line.starts_with(&prefix)) {
                *line = format!("{prefix}{}", stereo_parameters(&line[prefix.len()..]));
            } else {
                let insert =
                    section.len() - usize::from(section.last().is_some_and(String::is_empty));
                section.insert(insert, format!("{prefix}stereo=1;sprop-stereo=1"));
            }
        }
    }
    sections.into_iter().flatten().collect::<Vec<_>>().join(eol)
}
#[cfg(test)]
mod fluxer_stereo_tests {
    use super::*;
    const SDP:&str="v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:mic\r\na=msid:- mono-cid\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;useinbandfec=1\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:share\r\na=msid:- share-cid\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;stereo=0;sprop-stereo=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:video\r\na=rtpmap:111 VP8/90000\r\n";
    #[test]
    fn publisher_stereo_matches_cid_then_mid_without_touching_mono_or_video() {
        let tracks = HashSet::from(["share-cid".into()]);
        let offer = munge_selected(SDP, &HashSet::new(), &tracks);
        let parts = offer.split("m=audio").collect::<Vec<_>>();
        assert!(!parts[1].contains("stereo="));
        assert!(parts[2].contains("stereo=1;sprop-stereo=1"));
        assert!(offer.ends_with("a=rtpmap:111 VP8/90000\r\n"));
        let answer = SDP.replace("a=msid:- share-cid\r\n", "");
        let answer = munge_selected(&answer, &HashSet::from(["share".into()]), &HashSet::new());
        assert!(answer.contains("stereo=1;sprop-stereo=1"));
        assert_eq!(munge_selected(&offer, &HashSet::new(), &tracks), offer);
    }
    #[test]
    fn subscriber_responds_only_to_exact_remote_sprop_stereo_offer() {
        let offer = munge_selected(SDP, &HashSet::from(["share".into()]), &HashSet::new());
        let mids = remote_stereo_mids(&offer);
        assert_eq!(mids, HashSet::from(["share".into()]));
        assert!(remote_stereo_mids(&offer.replace("sprop-stereo=1", "sprop-stereo=10")).is_empty());
        let answer = munge_selected(SDP, &mids, &HashSet::new());
        assert!(!answer.split("m=audio").nth(1).unwrap().contains("stereo="));
    }
    #[test]
    fn missing_fmtp_is_created_and_lf_line_endings_are_preserved() {
        let sdp = "v=0\nm=audio 9 UDP/TLS/RTP/SAVPF 109\na=mid:screen\na=rtpmap:109 opus/48000/2\n";
        let out = munge_selected(sdp, &HashSet::from(["screen".into()]), &HashSet::new());
        assert!(out.contains("a=fmtp:109 stereo=1;sprop-stereo=1\n"));
        assert!(!out.contains('\r'));
    }
}
