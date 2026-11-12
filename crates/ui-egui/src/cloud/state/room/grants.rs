//! The owner's list of pending requests and current document grants.

use crate::cloud::{CloudReply, CloudRequest, CloudResponse, CloudService, RoomGrant, RoomGrantRole, RoomGrantedRole};
use serde_json::{Value, json};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum GrantAction {
    Refresh,
    Set { grant_id: String, role: RoomGrantedRole },
}

impl GrantAction {
    pub(super) fn command(&self) -> (&'static str, Value) {
        match self {
            Self::Refresh => ("ui.cloudRoomGrants", json!({})),
            Self::Set { grant_id, role } => ("ui.cloudSetRoomRole", json!({"grant_id": grant_id, "role": role})),
        }
    }
}

#[derive(Default)]
pub(super) struct OwnerGrants {
    rows: Vec<RoomGrant>,
    started: bool,
    pending: Option<(GrantAction, CloudReply)>,
    message: Option<String>,
}

impl OwnerGrants {
    pub(super) fn start(&mut self, service: &CloudService, room: &str, action: GrantAction) -> Result<(), String> {
        if self.pending.is_some() {
            return Err(tl!("A sharing request is already in progress.").into());
        }
        let request = match &action {
            GrantAction::Refresh => CloudRequest::RoomGrants { id: room.to_owned() },
            GrantAction::Set { grant_id, role } => CloudRequest::RoomSetRole { id: room.to_owned(), grant_id: grant_id.clone(), role: *role },
        };
        self.started = true;
        self.message = None;
        self.pending = Some((action, (service.request)(request)));
        Ok(())
    }

    pub(super) fn poll(&mut self) {
        let Some(result) = self.pending.as_ref().and_then(|(_, reply)| super::super::poll(reply)) else { return };
        let Some((action, _)) = self.pending.take() else { return };
        match (action, result) {
            (GrantAction::Refresh, Ok(CloudResponse::RoomGrants(rows))) => self.rows = rows,
            (GrantAction::Set { grant_id, .. }, Ok(CloudResponse::RoomGrant(grant))) if grant.id == grant_id => {
                if let Some(row) = self.rows.iter_mut().find(|row| row.id == grant_id) {
                    *row = grant;
                } else {
                    self.rows.push(grant);
                }
            }
            (_, Err(error)) => self.message = Some(error.message),
            _ => self.message = Some(tl!("WordCraft could not confirm the sharing change.").into()),
        }
    }

    pub(super) fn show(&self, ui: &mut egui::Ui) -> Option<GrantAction> {
        if !self.started {
            return Some(GrantAction::Refresh);
        }
        ui.separator();
        ui.heading(tl!("Document access"));
        let ready = self.pending.is_none();
        if ui.add_enabled(ready, egui::Button::new(tl!("Refresh access list"))).clicked() {
            return Some(GrantAction::Refresh);
        }
        if let Some(message) = &self.message {
            ui.label(message);
        }
        if self.rows.is_empty() {
            ui.label(if ready { tl!("No access requests yet.") } else { tl!("Loading access requests…") });
        }
        for row in &self.rows {
            if let Some(action) = show_row(ui, row, ready) {
                return Some(action);
            }
        }
        None
    }
}

fn show_row(ui: &mut egui::Ui, grant: &RoomGrant, ready: bool) -> Option<GrantAction> {
    let role = match grant.role {
        RoomGrantRole::Pending => tl!("Pending"),
        RoomGrantRole::Editor => tl!("Editor"),
        RoomGrantRole::Viewer => tl!("Viewer"),
        RoomGrantRole::Revoked => tl!("Revoked"),
    };
    if let Some(name) = &grant.display_name {
        ui.label(name);
    } else {
        ui.label(tl!("Request code"));
        ui.monospace(&grant.id);
    }
    ui.label(role);
    ui.horizontal(|ui| {
        let options = [
            (tl!("Editor"), RoomGrantRole::Editor, RoomGrantedRole::Editor),
            (tl!("Viewer"), RoomGrantRole::Viewer, RoomGrantedRole::Viewer),
            (tl!("Revoke"), RoomGrantRole::Revoked, RoomGrantedRole::Revoked),
        ];
        for (label, current, role) in options {
            if ui.add_enabled(ready && grant.role != current, egui::Button::new(label)).clicked() {
                return Some(GrantAction::Set { grant_id: grant.id.clone(), role });
            }
        }
        None
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn applies_the_confirmed_role_to_the_pending_request() {
        let (sender, requests) = mpsc::channel();
        let service = CloudService {
            new_operation_id: Box::new(|| Ok("operation".into())),
            request: Box::new(move |request| {
                let (response, receiver) = mpsc::channel();
                sender.send((request, response)).unwrap();
                receiver
            }),
        };
        let mut panel = OwnerGrants::default();
        panel.start(&service, "room", GrantAction::Refresh).unwrap();
        let (request, respond) = requests.recv().unwrap();
        assert_eq!(request, CloudRequest::RoomGrants { id: "room".into() });
        let pending = RoomGrant { id: "request".into(), display_name: Some("Synthetic tester".into()), role: RoomGrantRole::Pending };
        respond.send(Ok(CloudResponse::RoomGrants(vec![pending.clone()]))).unwrap();
        panel.poll();
        assert_eq!(panel.rows, std::slice::from_ref(&pending));

        let action = GrantAction::Set { grant_id: "request".into(), role: RoomGrantedRole::Editor };
        assert_eq!(action.command(), ("ui.cloudSetRoomRole", json!({"grant_id": "request", "role": "editor"})));
        panel.start(&service, "room", action).unwrap();
        let (request, respond) = requests.recv().unwrap();
        assert_eq!(request, CloudRequest::RoomSetRole { id: "room".into(), grant_id: "request".into(), role: RoomGrantedRole::Editor });
        assert_eq!(panel.rows, std::slice::from_ref(&pending));
        let confirmed = RoomGrant { role: RoomGrantRole::Editor, ..pending };
        respond.send(Ok(CloudResponse::RoomGrant(confirmed.clone()))).unwrap();
        panel.poll();
        assert_eq!(panel.rows, [confirmed]);
    }
}
