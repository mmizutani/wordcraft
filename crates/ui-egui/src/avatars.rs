//! The row of avatars beside Share (#32): who else is in the shared document the window shows,
//! each a circle in their colour with their initials, drawn in code. Clicking one follows them
//! (`ui.follow`); clicking it again stops. Past [`SHOWN`], the rest are in a "+n" menu.

use egui::{Color32, Rect, Sense, Stroke, Ui, vec2};
use serde_json::json;

use crate::WordApp;
use crate::share::Avatar;
use crate::theme::{Tokens, medium};

/// Avatars drawn in the row; the rest are in its "+n" menu.
pub(crate) const SHOWN: usize = 4;
/// An avatar's size, and the gap after it.
const SIZE: f32 = 22.0;
const GAP: f32 = 2.0;

/// How wide the row is for `n` people (none: no row).
pub(crate) fn width(n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let circles = n.min(SHOWN) + usize::from(n > SHOWN);
    circles as f32 * (SIZE + GAP) + 6.0
}

/// The row, in the far-side cluster of the tab strip (it fills from the far side, so the first
/// to join is drawn nearest Share).
pub(crate) fn row(app: &mut WordApp, ui: &mut Ui, t: &Tokens, avatars: &[Avatar]) {
    if avatars.is_empty() {
        return;
    }
    ui.add_space(6.0);
    for a in avatars.iter().take(SHOWN) {
        let following = app.canvas.follow == Some(a.id);
        let (r, resp) = ui.allocate_exact_size(vec2(SIZE, SIZE), Sense::click());
        paint(ui, t, r, a, following, resp.hovered());
        let label = label_of(a);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, following, &label));
        if resp.on_hover_text(hint(a, following)).clicked() {
            follow(app, a, following);
        }
        ui.add_space(GAP);
    }
    let rest: Vec<&Avatar> = avatars.iter().skip(SHOWN).collect();
    if rest.is_empty() {
        return;
    }
    let (r, resp) = ui.allocate_exact_size(vec2(SIZE, SIZE), Sense::click());
    ui.painter().circle_filled(r.center(), SIZE / 2.0, if resp.hovered() { t.pressed } else { t.hover });
    let more = format!("+{}", rest.len());
    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, &more, medium(10.0), t.text);
    let label = tl!("More people in this document").to_string();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    crate::rtl::menu(&resp).show(|ui| {
        for a in rest {
            let following = app.canvas.follow == Some(a.id);
            if ui.selectable_label(following, label_of(a)).on_hover_text(hint(a, following)).clicked() {
                follow(app, a, following);
                ui.close();
            }
        }
    });
}

fn paint(ui: &Ui, t: &Tokens, r: Rect, a: &Avatar, following: bool, hovered: bool) {
    let c = t.participant(a.color);
    let p = ui.painter();
    p.circle_filled(r.center(), SIZE / 2.0, c);
    if following || hovered {
        // A ring in the strip's text colour says whom the view follows.
        p.circle_stroke(r.center(), SIZE / 2.0 + 1.5, Stroke::new(if following { 2.0 } else { 1.0 }, t.text));
    }
    p.text(r.center(), egui::Align2::CENTER_CENTER, a.initials(), medium(10.0), t.on_participant);
    if !a.placed {
        // Not anywhere in the document yet: a dimmed avatar.
        p.circle_filled(r.center(), SIZE / 2.0, Color32::from_black_alpha(70));
    }
}

/// The name with the role, and the number of copies when there is more than one.
fn label_of(a: &Avatar) -> String {
    let role = tl!(role_name(a.role));
    if a.copies > 1 { format!("{} · {} · {}", a.name, role, a.copies) } else { format!("{} · {}", a.name, role) }
}

fn role_name(r: wordcraft_engine::room::Role) -> &'static str {
    use wordcraft_engine::room::Role;
    match r {
        Role::Viewer => "Viewer",
        Role::Commenter => "Commenter",
        Role::Suggester => "Suggester",
        Role::Editor => "Editor",
        Role::Owner => "Owner",
    }
}

fn hint(a: &Avatar, following: bool) -> String {
    let what = if following {
        tl!("Click to stop following")
    } else if a.placed {
        tl!("Click to follow their cursor")
    } else {
        tl!("Not in the document yet")
    };
    format!("{}\n{}", label_of(a), what)
}

fn follow(app: &mut WordApp, a: &Avatar, following: bool) {
    let participant = if following { serde_json::Value::Null } else { json!(a.id) };
    if let Err(e) = app.run("ui.follow", json!({"participant": participant})) {
        app.status(e);
    }
}
