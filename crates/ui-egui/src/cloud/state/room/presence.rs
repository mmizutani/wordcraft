use super::{CloudReply, CloudRequest, CloudResponse, CloudService, RemoteReplica, Role, RoomRole, Session, poll};
use crate::cloud::{RoomParticipant, RoomPosition};
use std::collections::HashMap;
use wordcraft_engine::room::{COLORS, Other, Participant, ParticipantId, Presence};

const HEARTBEAT_MS: f64 = 5_000.0;
const EXPIRE_MS: f64 = 15_000.0;

enum Reading {
    Heartbeat(CloudReply),
    Participants(CloudReply),
}

pub(super) struct RoomPresence {
    id: Option<String>,
    next: f64,
    updated: f64,
    reading: Option<Reading>,
    participants: Vec<RoomParticipant>,
    ids: HashMap<String, ParticipantId>,
    serial: u32,
    mapped: (u64, u64),
    pub(super) others: Vec<(Other, f64)>,
}

impl RoomPresence {
    pub(super) fn new(now: f64) -> Self {
        Self {
            id: None,
            next: now + HEARTBEAT_MS,
            updated: now,
            reading: None,
            participants: Vec::new(),
            ids: HashMap::new(),
            serial: 0,
            mapped: (0, 0),
            others: Vec::new(),
        }
    }

    pub(super) fn waiting(&self) -> bool {
        self.reading.is_some()
    }

    pub(super) fn tick(&mut self, id: &str, replica: &RemoteReplica, session: &Session, service: &CloudService, now: f64) {
        let revision = replica.revision();
        let mapping = (revision, session.changes().version());
        let mut changed = mapping != self.mapped;
        if now - self.updated >= EXPIRE_MS && !self.participants.is_empty() {
            self.participants.clear();
            changed = true;
        }
        if let Some(reading) = &self.reading {
            let reply = match reading {
                Reading::Heartbeat(reply) | Reading::Participants(reply) => reply,
            };
            if let Some(result) = poll(reply) {
                self.reading = None;
                match result {
                    Ok(CloudResponse::RoomHeartbeat) => {
                        self.reading = Some(Reading::Participants((service.request)(CloudRequest::RoomPresence { id: id.into() })));
                    }
                    Ok(CloudResponse::RoomPresence(participants)) => {
                        self.participants = participants;
                        self.updated = now;
                        changed = true;
                    }
                    _ => {}
                }
            }
        }
        if changed {
            self.remap(replica, now);
            self.mapped = mapping;
        }
        if self.reading.is_some() || now < self.next {
            return;
        }
        self.next = now + HEARTBEAT_MS;
        if self.id.is_none() {
            self.id = (service.new_operation_id)().ok();
        }
        let Some(session_id) = self.id.clone() else { return };
        let position = if replica.pending_len() == 0 {
            match (session.doc.anchor_at(&session.sel.anchor), session.doc.anchor_at(&session.sel.focus)) {
                (Some(anchor), Some(focus)) => RoomPosition { anchor: Some(anchor), focus: Some(focus) },
                _ => RoomPosition { anchor: None, focus: None },
            }
        } else {
            RoomPosition { anchor: None, focus: None }
        };
        self.reading = Some(Reading::Heartbeat((service.request)(CloudRequest::RoomHeartbeat { id: id.into(), session_id, revision, position })));
    }

    fn remap(&mut self, replica: &RemoteReplica, now: f64) {
        self.ids.retain(|id, _| self.participants.iter().any(|p| p.session_id == *id));
        let mut next = Vec::with_capacity(self.participants.len());
        for participant in &self.participants {
            if self.id.as_ref() == Some(&participant.session_id) {
                continue;
            }
            let id = *self.ids.entry(participant.session_id.clone()).or_insert_with(|| {
                self.serial = self.serial.saturating_add(1);
                ParticipantId(self.serial)
            });
            let who = Participant {
                id,
                name: participant.display_name.clone().unwrap_or_else(|| tl!("Another participant").into()),
                role: match participant.role {
                    RoomRole::Owner => Role::Owner,
                    RoomRole::Editor => Role::Editor,
                    RoomRole::Viewer => Role::Viewer,
                },
            };
            let location = match (participant.position.anchor, participant.position.focus) {
                (Some(anchor), Some(focus)) if participant.revision == replica.revision() => replica.map_presence(Presence { anchor, focus }),
                _ => None,
            };
            let (anchors, at) = location.map_or((None, None), |(anchors, at)| (Some(anchors), Some(at)));
            let stamp = self
                .others
                .iter()
                .find(|(other, _)| other.who.id == id && other.anchors == anchors)
                .map_or(if anchors.is_some() { now } else { f64::NEG_INFINITY }, |(_, stamp)| *stamp);
            let color = u8::try_from(id.0.saturating_sub(1) % u32::from(COLORS)).unwrap_or_default();
            next.push((Other { who, color, at, anchors, later: participant.revision > replica.revision() }, stamp));
        }
        self.others = next;
    }
}
