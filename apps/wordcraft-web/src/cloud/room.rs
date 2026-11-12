use super::{MAX_BYTES, MAX_REVISION, failure, revision, status_failure, valid_id};
use serde::Deserialize;
use wordcraft_engine::room::remote::Proposal;
use wordcraft_ui_egui::cloud::{
    CloudFailure, CloudFailureKind, RoomAccess, RoomAccessState, RoomChanges, RoomGrant, RoomGrantRole, RoomGrantedRole, RoomMetadata,
    RoomParticipant, RoomPosition, RoomRole,
};

pub(super) const MAX_CHANGE_BYTES: usize = 256 * 1024;
const MAX_CHANGES: usize = 32;
const MAX_REPLAY_CHANGE_BYTES: usize = 512 * 1024;
// The response also contains one ID and revision per change plus the page cursor.
pub(super) const MAX_REPLAY_RESPONSE_BYTES: usize = MAX_REPLAY_CHANGE_BYTES + 4 * 1024;
const MAX_GRANTS: usize = 32;
const MAX_DISPLAY_NAME: usize = 256;
const ROOT: &str = "/api/cloud/rooms";

pub(super) fn room_url(id: &str) -> Option<String> {
    valid_id(id).then(|| format!("{ROOT}/{id}"))
}

pub(super) fn replay_url(id: &str, after: u64, through: Option<u64>) -> Option<String> {
    if after > MAX_REVISION || through.is_some_and(|through| through < after || through > MAX_REVISION) {
        return None;
    }
    let url = format!("{}/changes?after={after}", room_url(id)?);
    Some(match through {
        Some(through) => format!("{url}&through={through}"),
        None => url,
    })
}

pub(super) fn grant_url(id: &str, grant_id: &str) -> Option<String> {
    valid_id(grant_id).then_some(())?;
    Some(format!("{}/grants/{grant_id}", room_url(id)?))
}

pub(super) fn valid_operation_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

pub(super) fn submission_bytes(proposal: &Proposal) -> Option<Vec<u8>> {
    if !valid_operation_id(&proposal.operation_id)
        || proposal.base_revision > MAX_REVISION
        || !(1..=MAX_CHANGE_BYTES).contains(&proposal.change_json.len())
    {
        return None;
    }
    // Preserve the exact JSON of the in-flight proposal across uncertain retries.
    let mut bytes = format!(r#"{{"operation_id":"{}","base_revision":{},"change":"#, proposal.operation_id, proposal.base_revision).into_bytes();
    bytes.extend_from_slice(&proposal.change_json);
    bytes.push(b'}');
    Some(bytes)
}

pub(super) fn valid_metadata(metadata: &RoomMetadata, id: &str, activating: Option<u64>) -> bool {
    metadata.id == id
        && (1..=MAX_REVISION).contains(&metadata.file_revision)
        && activating.is_none_or(|revision| revision == metadata.file_revision && metadata.role == RoomRole::Owner)
        && metadata.checkpoint_revision <= metadata.head
        && metadata.published_room_revision <= metadata.head
        && metadata.head <= MAX_REVISION
}

pub(super) fn replay_page(bytes: &[u8], after: u64, through: Option<u64>) -> Option<RoomChanges> {
    if bytes.len() > MAX_REPLAY_RESPONSE_BYTES {
        return None;
    }
    let page = serde_json::from_slice(bytes).ok()?;
    valid_page(&page, after, through).then_some(page)
}

fn valid_page(page: &RoomChanges, after: u64, through: Option<u64>) -> bool {
    if page.changes.len() > MAX_CHANGES || page.head > MAX_REVISION || page.head < after || through.is_some_and(|head| page.head != head) {
        return false;
    }
    let mut last = after;
    let mut total_bytes = 0;
    for entry in &page.changes {
        let Ok(bytes) = serde_json::to_vec(&entry.change) else { return false };
        if entry.revision != last + 1 || entry.revision > page.head || !valid_operation_id(&entry.operation_id) || bytes.len() > MAX_CHANGE_BYTES {
            return false;
        }
        total_bytes += bytes.len();
        last = entry.revision;
    }
    total_bytes <= MAX_REPLAY_CHANGE_BYTES && if last < page.head { last > after && page.next_after == Some(last) } else { page.next_after.is_none() }
}

#[derive(Deserialize)]
pub(super) struct SubmissionReceipt {
    pub(super) revision: u64,
    pub(super) head: u64,
}

impl SubmissionReceipt {
    pub(super) fn valid_for(&self, base_revision: u64) -> bool {
        base_revision < self.revision && self.revision <= self.head && self.head <= MAX_REVISION
    }
}

pub(super) fn valid_access(access: &RoomAccess) -> bool {
    match access.state {
        RoomAccessState::Owner | RoomAccessState::Unrequested => access.request_code.is_none(),
        _ => access.request_code.as_deref().is_some_and(valid_id),
    }
}

#[derive(Deserialize)]
pub(super) struct Grants {
    pub(super) grants: Vec<RoomGrant>,
}

impl Grants {
    pub(super) fn valid(&self) -> bool {
        self.grants.len() <= MAX_GRANTS
            && self.grants.iter().enumerate().all(|(index, grant)| {
                valid_grant(grant) && self.grants.get(..index).is_some_and(|previous| previous.iter().all(|item| item.id != grant.id))
            })
    }
}

fn valid_grant(grant: &RoomGrant) -> bool {
    valid_id(&grant.id) && grant.display_name.as_deref().is_none_or(valid_display_name)
}

fn valid_display_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_DISPLAY_NAME && name.trim() == name && !name.chars().any(char::is_control)
}

pub(super) fn valid_grant_receipt(grant: &RoomGrant, id: &str, role: RoomGrantedRole) -> bool {
    valid_grant(grant)
        && grant.id == id
        && matches!(
            (grant.role, role),
            (RoomGrantRole::Editor, RoomGrantedRole::Editor)
                | (RoomGrantRole::Viewer, RoomGrantedRole::Viewer)
                | (RoomGrantRole::Revoked, RoomGrantedRole::Revoked)
        )
}

#[derive(Deserialize)]
pub(super) struct Presence {
    pub(super) participants: Vec<RoomParticipant>,
}

impl Presence {
    pub(super) fn valid(&self) -> bool {
        self.participants.len() <= 64
            && self.participants.iter().enumerate().all(|(index, participant)| {
                valid_heartbeat(&participant.session_id, participant.revision, participant.position)
                    && participant.display_name.as_deref().is_none_or(valid_display_name)
                    && self.participants.get(..index).is_some_and(|previous| previous.iter().all(|item| item.session_id != participant.session_id))
            })
    }
}

pub(super) fn valid_heartbeat(session_id: &str, revision: u64, position: RoomPosition) -> bool {
    valid_operation_id(session_id)
        && revision <= MAX_REVISION
        && position.anchor.is_some() == position.focus.is_some()
        && [position.anchor, position.focus].into_iter().flatten().all(|anchor| anchor.off <= MAX_BYTES)
}

#[derive(Deserialize)]
pub(super) struct RoomSaveReceipt {
    pub(super) file_revision: u64,
    pub(super) room_revision: u64,
}

impl RoomSaveReceipt {
    pub(super) fn valid_for(&self, room_revision: u64) -> bool {
        (1..=MAX_REVISION).contains(&self.file_revision) && room_revision == self.room_revision
    }
}

pub(super) fn room_status_failure(status: u16, code: Option<&str>) -> CloudFailure {
    use CloudFailureKind::*;
    match (status, code) {
        (403, Some("room_role_changed" | "room_read_only" | "room_owner_required")) => {
            failure(AccessDenied, "Your current room access does not allow that action.")
        }
        (404, _) => failure(Missing, "The shared room is unavailable."),
        (409, Some("change_conflict")) => failure(Conflict, "This edit conflicts with a change already in the room."),
        (409, Some("stale_room_revision")) => failure(RoomRevisionExpired, "This room revision is no longer available."),
        (409, Some("receipt_expired")) => failure(Recovery, "The room changed. Reopen it to recover the latest version."),
        (409, Some("room_storage_limit")) => failure(Capacity, "The shared room has reached its storage limit."),
        (409, Some("room_access_limit")) => failure(Capacity, "The shared room has reached its access request limit."),
        (413, _) => failure(Invalid, "The shared room rejected data that exceeded its size limit."),
        (422, Some("invalid_checkpoint" | "unsupported_room_document")) => failure(Invalid, "This document cannot be opened as a shared room."),
        _ => status_failure(status, code),
    }
}

pub(super) fn checkpoint_revision(tag: &str) -> Option<u64> {
    if tag == "\"0\"" { Some(0) } else { revision(tag) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ROOM: &str = "123456789012345678901234567890ab";
    const OP: &str = "c2032927-0b48-40aa-8d5d-d312e5f4de20";

    #[test]
    fn accepts_canonical_room_revision_zero() {
        assert_eq!(checkpoint_revision("\"0\""), Some(0));
        assert_eq!(checkpoint_revision("\"1\""), Some(1));
        assert_eq!(checkpoint_revision("\"00\""), None);
        assert_eq!(checkpoint_revision("W/\"0\""), None);
    }

    #[test]
    fn validates_room_urls_and_metadata() {
        assert_eq!(room_url(ROOM), Some(format!("/api/cloud/rooms/{ROOM}")));
        assert_eq!(room_url("//external"), None);
        assert_eq!(grant_url(ROOM, "../save"), None);
        assert_eq!(grant_url(ROOM, ROOM), Some(format!("/api/cloud/rooms/{ROOM}/grants/{ROOM}")));
        assert_eq!(replay_url(ROOM, 0, Some(2)), Some(format!("/api/cloud/rooms/{ROOM}/changes?after=0&through=2")));
        assert_eq!(replay_url(ROOM, 1, Some(0)), None);
        assert_eq!(replay_url(ROOM, MAX_REVISION + 1, None), None);

        let metadata =
            RoomMetadata { id: ROOM.into(), file_revision: 5, checkpoint_revision: 1, published_room_revision: 0, head: 2, role: RoomRole::Editor };
        assert!(valid_metadata(&metadata, ROOM, None));
        assert!(!valid_metadata(&metadata, ROOM, Some(5)));
        let metadata = RoomMetadata { role: RoomRole::Owner, ..metadata };
        assert!(valid_metadata(&metadata, ROOM, Some(5)));
        assert!(!valid_metadata(&metadata, ROOM, Some(4)));
        assert!(!valid_metadata(&RoomMetadata { checkpoint_revision: 3, ..metadata }, ROOM, None));
    }

    #[test]
    fn accepts_only_contiguous_replay_through_the_captured_head() {
        let mut page = json!({ "role": "viewer", "head": 2, "changes": [
            { "revision": 1, "operation_id": OP, "change": {} },
        ], "next_after": 1 });
        let parse = |page: &serde_json::Value, after, through| replay_page(page.to_string().as_bytes(), after, through);
        assert_eq!(parse(&page, 0, Some(2)).unwrap().role, RoomRole::Viewer);
        assert!(parse(&page, 0, Some(3)).is_none());
        assert!(parse(&page, 1, Some(2)).is_none());
        page["next_after"] = json!(2);
        assert!(parse(&page, 0, None).is_none());
        page["next_after"] = json!(null);
        assert!(parse(&page, 0, None).is_none());
        page["changes"] = json!([]);
        assert!(parse(&page, 0, None).is_none());
        assert!(parse(&page, 2, None).is_some());
        page["changes"] = (1..=33).map(|revision| json!({ "revision": revision, "operation_id": OP, "change": {} })).collect();
        page["head"] = json!(33);
        assert!(parse(&page, 0, None).is_none());
        assert!(replay_page(&vec![b' '; MAX_REPLAY_RESPONSE_BYTES + 1], 0, None).is_none());
    }

    #[test]
    fn sends_the_same_change_json_and_requires_a_matching_receipt() {
        let mut proposal = Proposal { operation_id: OP.into(), base_revision: 4, change_json: br"{ }".to_vec() };
        let expected = format!(r#"{{"operation_id":"{OP}","base_revision":4,"change":{{ }}}}"#);
        assert_eq!(submission_bytes(&proposal).as_deref(), Some(expected.as_bytes()));
        proposal.change_json.resize(MAX_CHANGE_BYTES + 1, b' ');
        assert!(submission_bytes(&proposal).is_none());
        assert!(SubmissionReceipt { revision: 5, head: 6 }.valid_for(4));
        assert!(!SubmissionReceipt { revision: 4, head: 6 }.valid_for(4));
        assert!(!SubmissionReceipt { revision: 7, head: 6 }.valid_for(4));
        assert!(RoomSaveReceipt { file_revision: 2, room_revision: 0 }.valid_for(0));
        assert!(!RoomSaveReceipt { file_revision: 0, room_revision: 0 }.valid_for(0));
        assert!(!RoomSaveReceipt { file_revision: 2, room_revision: 2 }.valid_for(3));
    }

    #[test]
    fn validates_access_grant_and_presence_responses() {
        let access = RoomAccess { state: RoomAccessState::Pending, request_code: Some(ROOM.into()) };
        assert!(valid_access(&access));
        assert!(!valid_access(&RoomAccess { request_code: None, ..access }));
        let grant = RoomGrant { id: ROOM.into(), display_name: Some("Élodie".into()), role: RoomGrantRole::Viewer };
        assert!(valid_grant_receipt(&grant, ROOM, RoomGrantedRole::Viewer));
        assert!(!valid_grant_receipt(&grant, ROOM, RoomGrantedRole::Editor));
        assert!(Grants { grants: vec![grant.clone()] }.valid());
        assert!(!Grants { grants: vec![grant.clone(), grant] }.valid());

        let position = RoomPosition::default();
        assert!(valid_heartbeat(OP, 0, position));
        assert!(!valid_heartbeat(OP, MAX_REVISION + 1, position));
        assert!(!valid_heartbeat(ROOM, 0, position));
        let participant = RoomParticipant { session_id: OP.into(), role: RoomRole::Owner, display_name: Some("A\nB".into()), revision: 0, position };
        assert!(!Presence { participants: vec![participant] }.valid());
        assert!(Presence { participants: vec![] }.valid());
    }

    #[test]
    fn maps_known_room_failures_without_displaying_server_text() {
        use CloudFailureKind::*;
        assert_eq!(room_status_failure(403, Some("room_read_only")).kind, AccessDenied);
        assert_eq!(room_status_failure(409, Some("change_conflict")).kind, Conflict);
        assert_eq!(room_status_failure(409, Some("stale_room_revision")).kind, RoomRevisionExpired);
        assert_eq!(room_status_failure(409, Some("receipt_expired")).kind, Recovery);
        assert_eq!(room_status_failure(409, Some("room_access_limit")).kind, Capacity);
        assert_eq!(status_failure(409, Some("room_required")).kind, RoomRequired);
        assert_eq!(room_status_failure(409, Some("some internal error")).kind, Retryable);
        assert!(!room_status_failure(409, Some("some internal error")).message.contains("internal error"));
    }
}
