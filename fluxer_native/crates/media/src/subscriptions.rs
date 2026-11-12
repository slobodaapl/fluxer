// SPDX-License-Identifier: AGPL-3.0-or-later
//! Explicit remote subscriptions matching VoiceRoomEventBinder.ts and the
//! screen-share watch managers. Camera demand remains a separate future API.
use livekit::prelude::*;
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Decision {
    pub subscribe: bool,
    pub enabled: bool,
}
#[derive(Default)]
pub(crate) struct Policy {
    pub watched: BTreeSet<String>,
    pub deafened: bool,
}
impl Policy {
    pub fn decision(
        &self,
        identity: &str,
        source: TrackSource,
        legacy_screen_audio: bool,
    ) -> Decision {
        // Audio preferences disable watched stream audio while retaining watch
        // intent. Microphone deafen explicitly stops the SFU subscription.
        let (subscribe, enabled) = if legacy_screen_audio || source == TrackSource::ScreenshareAudio
        {
            let watch = self.watched.contains(identity);
            (watch, watch && !self.deafened)
        } else {
            match source {
                TrackSource::Microphone => (!self.deafened, !self.deafened),
                TrackSource::Screenshare => {
                    let watch = self.watched.contains(identity);
                    (watch, watch)
                }
                _ => (false, false),
            }
        };
        Decision { subscribe, enabled }
    }
}

pub(crate) struct Action {
    pub publication: RemoteTrackPublication,
    pub decision: Decision,
    pub force: bool,
}
pub(crate) struct Plan {
    pub actions: Vec<Action>,
    pub availability: Vec<(String, bool)>,
}
struct Publication {
    identity: String,
    publication: RemoteTrackPublication,
}
#[derive(Default)]
pub(crate) struct Registry {
    pub policy: Policy,
    participants: HashMap<String, RemoteParticipant>,
    publications: HashMap<String, Publication>,
    available: BTreeSet<String>,
}
impl Registry {
    pub fn observe_participant(&mut self, participant: RemoteParticipant) -> Plan {
        let identity = participant.identity().to_string();
        for publication in participant.track_publications().into_values() {
            self.record(&identity, publication);
        }
        self.participants.insert(identity, participant);
        self.plan()
    }
    pub fn observe_publication(
        &mut self,
        participant: RemoteParticipant,
        publication: RemoteTrackPublication,
    ) -> Plan {
        let identity = participant.identity().to_string();
        self.record(&identity, publication);
        self.participants.insert(identity, participant);
        self.plan()
    }
    fn record(&mut self, identity: &str, publication: RemoteTrackPublication) {
        self.publications.insert(
            publication.sid().to_string(),
            Publication {
                identity: identity.into(),
                publication,
            },
        );
    }
    pub fn remove_publication(&mut self, sid: &str) -> Plan {
        self.publications.remove(sid);
        self.plan()
    }
    pub fn remove_participant(&mut self, identity: &str) -> Plan {
        self.participants.remove(identity);
        self.publications
            .retain(|_, publication| publication.identity != identity);
        self.plan()
    }
    pub fn reconcile(&mut self) -> Plan {
        // Participant handles are shared with Room and reflect refreshed SIDs
        // after reconnect. Rebuild metadata, preserving explicit watch intent.
        self.publications.clear();
        let participants = self.participants.values().cloned().collect::<Vec<_>>();
        for participant in participants {
            let identity = participant.identity().to_string();
            for publication in participant.track_publications().into_values() {
                self.record(&identity, publication);
            }
        }
        let mut plan = self.plan();
        for action in &mut plan.actions {
            action.force = true;
        }
        plan
    }
    pub fn clear_publications(&mut self) -> Plan {
        self.participants.clear();
        self.publications.clear();
        self.plan()
    }
    pub fn plan(&mut self) -> Plan {
        let available = self
            .publications
            .values()
            .filter(|entry| {
                entry.publication.source() == TrackSource::Screenshare
                    && entry.publication.kind() == TrackKind::Video
            })
            .map(|entry| entry.identity.clone())
            .collect::<BTreeSet<_>>();
        let availability = availability_changes(&self.available, &available);
        self.available = available;
        let actions = self
            .publications
            .values()
            .map(|entry| Action {
                publication: entry.publication.clone(),
                decision: self.decision(&entry.identity, &entry.publication),
                force: false,
            })
            .collect();
        Plan {
            actions,
            availability,
        }
    }
    pub fn decision(&self, identity: &str, publication: &RemoteTrackPublication) -> Decision {
        self.policy.decision(
            identity,
            publication.source(),
            publication.kind() == TrackKind::Audio && is_screen_audio_label(&publication.name()),
        )
    }
    pub fn desired_track(&self, sid: &str) -> bool {
        self.publications
            .get(sid)
            .is_some_and(|entry| self.decision(&entry.identity, &entry.publication).subscribe)
    }
}
fn availability_changes(
    previous: &BTreeSet<String>,
    next: &BTreeSet<String>,
) -> Vec<(String, bool)> {
    previous
        .difference(next)
        .map(|id| (id.clone(), false))
        .chain(next.difference(previous).map(|id| (id.clone(), true)))
        .collect()
}
fn is_screen_audio_label(label: &str) -> bool {
    let label = label
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        label.as_str(),
        "screenaudio" | "screenshareaudio" | "systemaudio"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn microphone_deafen_never_auto_subscribes_video_or_stream_audio() {
        let mut policy = Policy::default();
        assert_eq!(
            policy.decision("alice", TrackSource::Microphone, false),
            Decision {
                subscribe: true,
                enabled: true
            }
        );
        for source in [
            TrackSource::Camera,
            TrackSource::Screenshare,
            TrackSource::ScreenshareAudio,
            TrackSource::Unknown,
        ] {
            assert!(!policy.decision("alice", source, false).subscribe);
        }
        policy.deafened = true;
        assert_eq!(
            policy.decision("alice", TrackSource::Microphone, false),
            Decision {
                subscribe: false,
                enabled: false
            }
        );
    }
    #[test]
    fn explicit_watch_is_per_identity_and_deafen_preserves_video_intent() {
        let mut policy = Policy {
            watched: BTreeSet::from(["alice".into()]),
            deafened: false,
        };
        for source in [TrackSource::Screenshare, TrackSource::ScreenshareAudio] {
            assert!(policy.decision("alice", source, false).subscribe);
            assert!(!policy.decision("bob", source, false).subscribe);
        }
        assert!(
            !policy
                .decision("alice", TrackSource::Camera, false)
                .subscribe
        );
        policy.deafened = true;
        assert_eq!(
            policy.decision("alice", TrackSource::Screenshare, false),
            Decision {
                subscribe: true,
                enabled: true
            }
        );
        assert_eq!(
            policy.decision("alice", TrackSource::ScreenshareAudio, false),
            Decision {
                subscribe: true,
                enabled: false
            }
        );
        policy.watched.clear();
        assert!(
            !policy
                .decision("alice", TrackSource::ScreenshareAudio, false)
                .subscribe
        );
    }
    #[test]
    fn legacy_screen_audio_names_follow_existing_voice_track_source_policy() {
        for name in ["Screen audio", "Screen-share_audio", "SystemAudio"] {
            assert!(is_screen_audio_label(name));
        }
        assert!(!is_screen_audio_label("Microphone"));
        let policy = Policy {
            watched: BTreeSet::from(["alice".into()]),
            ..Default::default()
        };
        assert!(
            policy
                .decision("alice", TrackSource::Unknown, true)
                .subscribe
        );
        assert!(!policy.decision("bob", TrackSource::Unknown, true).subscribe);
    }
    #[test]
    fn availability_reports_only_transitions_and_handles_republish() {
        let old = BTreeSet::from(["alice".into(), "bob".into()]);
        let next = BTreeSet::from(["alice".into(), "carol".into()]);
        assert_eq!(
            availability_changes(&old, &next),
            vec![("bob".into(), false), ("carol".into(), true)]
        );
        assert!(availability_changes(&next, &next).is_empty());
        assert_eq!(availability_changes(&next, &BTreeSet::new()).len(), 2);
    }
    #[test]
    fn preconnect_intent_survives_room_metadata_cleanup() {
        let mut registry = Registry::default();
        registry.policy.watched.insert("alice".into());
        registry.clear_publications();
        assert!(
            registry
                .policy
                .decision("alice", TrackSource::Screenshare, false)
                .subscribe
        );
        assert!(registry.plan().availability.is_empty());
    }
}
