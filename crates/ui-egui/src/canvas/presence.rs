//! Others in the shared document (#32): each participant's selection, caret and a flag with
//! their name, in their colour ([`Tokens::participant_colors`]). Their places come from the room
//! every frame ([`crate::WordApp::others_here`]) as places in the window's document, so they are
//! drawn where the text they are at is now.

use std::ops::RangeInclusive;

use egui::{Color32, Painter, Pos2, Rect, Stroke, pos2, vec2};
use wordcraft_doc::Pos;
use wordcraft_engine::room::{Other, ParticipantId};
use wordcraft_layout::DocLayout;
use wordcraft_layout::hit::page_lines;

use super::pages::page_screen_scale;
use crate::share::FLAG_MS;
use crate::theme::{Tokens, medium};

/// At most this many others are drawn, and at most this many selection rectangles in all of
/// those on screen: a room with many agents selecting long stretches still draws in time.
const MAX_DRAWN: usize = 64;
const MAX_RECTS: usize = 2000;

/// The size of the name on a flag, in points.
const FLAG_TEXT: f32 = 11.0;

/// Where another participant is on screen.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Mark {
    pub(crate) who: ParticipantId,
    pub(crate) name: String,
    pub(crate) color: u8,
    /// Their selection's rectangles (none while it is a caret).
    pub(crate) rects: Vec<Rect>,
    /// Their caret: the bar from its top to its bottom (across, for turned text).
    pub(crate) caret: Option<[Pos2; 2]>,
    /// Whether their name shows: for a while after they moved, while the pointer is over their
    /// caret, and while the person follows them.
    pub(crate) flag: bool,
}

/// The pages others are drawn on: `layout` of `doc` (with the page the caret is on,
/// `page_hint`), the pages' screen rectangles, the zoom, and the part of the screen the pages
/// are seen in.
pub(crate) struct Pages<'a> {
    pub(crate) layout: &'a DocLayout,
    pub(crate) doc: &'a wordcraft_doc::Document,
    pub(crate) page_hint: usize,
    pub(crate) rects: &'a [Rect],
    /// Per page, the part shown ([`super::Geometry::bands`]): with Hide White Space a page's
    /// rect overlaps its neighbours' in the hidden margins, so nothing outside it is drawn.
    pub(crate) bands: &'a [(f32, f32)],
    pub(crate) scale: f32,
    pub(crate) visible: Rect,
}

/// Where everyone else in `others` (with when their place last changed) is on `pages`; `now` and
/// the `pointer` say whose names show, and `follow` whom the person follows.
pub(crate) fn marks(pages: &Pages<'_>, others: &[(&Other, f64)], now: f64, pointer: Option<Pos2>, follow: Option<ParticipantId>) -> Vec<Mark> {
    let Pages { layout, doc, page_hint, rects, bands, scale, visible } = *pages;
    // The screen band page `pi` shows: all of it across, its band down.
    let band = |pi: usize, pr: &Rect| -> Rect {
        let (top, bot) = bands.get(pi).copied().unwrap_or((0.0, pr.height()));
        Rect::from_x_y_ranges(egui::Rangef::EVERYTHING, pr.min.y + top..=pr.min.y + bot)
    };
    let to_screen = |pi: usize, r: &wordcraft_geom::Rect| -> Option<Rect> {
        let pr = rects.get(pi)?;
        let s = layout.pages.get(pi).map_or(scale, |page| page_screen_scale(*pr, page, scale));
        let sr = Rect::from_min_size(pos2(pr.min.x + r.x * s, pr.min.y + r.y * s), vec2(r.w * s, r.h * s)).intersect(band(pi, pr));
        (sr.height() > 0.0).then_some(sr)
    };
    // The pages on screen, with one either side, so a selection cut to them ends off screen.
    let mut seen = (0..rects.len()).filter(|&pi| rects.get(pi).is_some_and(|pr| band(pi, pr).intersects(visible)));
    let shown_pages = seen.next().map(|first| {
        let last = seen.next_back().unwrap_or(first);
        first.saturating_sub(1)..=last.saturating_add(1).min(rects.len().saturating_sub(1))
    });
    let mut budget = MAX_RECTS;
    let mut out = Vec::new();
    for (o, moved) in others.iter().filter(|(o, _)| o.at.is_some()).take(MAX_DRAWN) {
        let Some(sel) = o.at.as_ref() else { continue };
        let mut sel_rects = Vec::new();
        let on_screen = shown_pages.clone().and_then(|pages| cut_to_pages(layout, &sel.anchor, &sel.focus, pages));
        if let Some((from, to)) = on_screen.filter(|_| budget > 0) {
            // Only what is on screen counts: a long selection, or one far off, costs the pages
            // the person sees, and leaves the budget to the selections there.
            let shown = layout.selection_rects(doc, &from, &to, page_hint);
            sel_rects.extend(shown.iter().filter_map(|(pi, r)| to_screen(*pi, r)).filter(|sr| sr.intersects(visible)).take(budget));
            budget = budget.saturating_sub(sel_rects.len());
        }
        let caret = layout.caret_on(&sel.focus, page_hint).and_then(|c| {
            let pr = rects.get(c.page)?;
            let s = layout.pages.get(c.page).map_or(scale, |page| page_screen_scale(*pr, page, scale));
            let (x, y0) = (pr.min.x + c.x * s, pr.min.y + c.top * s);
            let shown = band(c.page, pr).y_range();
            if c.width > 0.0 {
                shown.contains(y0).then(|| [pos2(x, y0), pos2(x + c.width * s, y0)])
            } else {
                let (top, bottom) = (y0.max(shown.min), (y0 + c.height * s).min(shown.max));
                (top < bottom).then(|| [pos2(x, top), pos2(x, bottom)])
            }
        });
        let hovered = caret.zip(pointer).is_some_and(|(bar, p)| Rect::from_two_pos(bar[0], bar[1]).expand(6.0).contains(p));
        let flag = caret.is_some() && (now - moved < FLAG_MS || hovered || follow == Some(o.who.id));
        out.push(Mark { who: o.who.id, name: o.who.name.clone(), color: o.color, rects: sel_rects, caret, flag });
    }
    out
}

/// Of the selection `a..b`, the part on `pages`: from the first line there it holds to the last.
/// None when it holds none of their lines (or is a caret). Others' selections are laid out only
/// there, so a participant selecting a whole long document costs the window the pages on screen
/// each frame, not every line.
fn cut_to_pages(layout: &DocLayout, a: &Pos, b: &Pos, pages: RangeInclusive<usize>) -> Option<(Pos, Pos)> {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    let (mut lo, mut hi): (Option<Pos>, Option<Pos>) = (None, None);
    for page in layout.pages.get(pages)? {
        for l in page_lines(page, a.story) {
            let Some(line) = l.para.lines.get(l.li) else { continue };
            let at = |off| Pos { story: a.story, path: l.path.clone(), off };
            let (start, stop) = (at(line.start), at(line.stop));
            if lo.as_ref().is_none_or(|lo| start < *lo) {
                lo = Some(start);
            }
            if hi.as_ref().is_none_or(|hi| stop > *hi) {
                hi = Some(stop);
            }
        }
    }
    let from = lo?.max(a.clone());
    let to = hi?.min(b.clone());
    (from < to).then_some((from, to))
}

/// Others' selections, under the person's own.
pub(crate) fn paint_selections(painter: &Painter, t: &Tokens, marks: &[Mark]) {
    for m in marks {
        let c = t.participant(m.color);
        for r in &m.rects {
            painter.rect_filled(*r, 0.0, Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 0x38));
        }
    }
}

/// Others' carets, and the flags with their names above them: a flag that would cover one
/// drawn before it goes above that one.
pub(crate) fn paint_carets(painter: &Painter, t: &Tokens, marks: &[Mark]) {
    let mut flags: Vec<Rect> = Vec::new();
    for m in marks {
        let Some(bar) = m.caret else { continue };
        let c = t.participant(m.color);
        let x = bar[0].x.round() + 0.5;
        let across = (bar[0].y - bar[1].y).abs() < 0.5;
        let line = if across {
            [pos2(bar[0].x, bar[0].y.round() + 0.5), pos2(bar[1].x, bar[0].y.round() + 0.5)]
        } else {
            [pos2(x, bar[0].y), pos2(x, bar[1].y)]
        };
        painter.line_segment(line, Stroke::new(2.0, c));
        if !m.flag {
            // A small square at the top says whose caret it is in their colour even without the name.
            painter.rect_filled(Rect::from_center_size(pos2(x, bar[0].y), vec2(5.0, 5.0)), 1.0, c);
            continue;
        }
        let galley = painter.layout_no_wrap(m.name.clone(), medium(FLAG_TEXT), t.on_participant);
        let size = galley.size() + vec2(8.0, 2.0);
        let mut r = Rect::from_min_size(pos2(x - 1.0, bar[0].y - size.y), size);
        for _ in 0..MAX_DRAWN {
            match flags.iter().find(|f| f.intersects(r)) {
                Some(f) => r = r.translate(vec2(0.0, f.min.y - r.max.y - 1.0)),
                None => break,
            }
        }
        painter.rect_filled(r, egui::CornerRadius { nw: 3, ne: 3, se: 3, sw: 0 }, c);
        painter.galley(r.left_center() + vec2(4.0, -galley.size().y / 2.0), galley, t.on_participant);
        flags.push(r);
    }
}

#[cfg(test)]
mod tests {
    use egui::Rect;
    use serde_json::json;
    use wordcraft_doc::Document;
    use wordcraft_engine::Session;
    use wordcraft_engine::room::{Lobby, Replica, hosted};

    use super::{FLAG_MS, Mark, marks};
    use crate::control::{self, ControlRequest, Outcome};
    use crate::{Services, WordApp};

    fn harness(text: &str) -> (egui_kittest::Harness<'static, WordApp>, Lobby) {
        let lobby = Lobby::default();
        let mut a = WordApp::new(Session::new(Document::from_text(text)), Services::default()).with_lobby(lobby.clone());
        a.session.author = "Minoru".into();
        let h = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            a,
        );
        (h, lobby)
    }

    fn owner(a: &mut WordApp, method: &str, params: serde_json::Value) -> serde_json::Value {
        let (req, _reply) = ControlRequest::new(method, params);
        match control::handle(a, &egui::Context::default(), &req) {
            Outcome::Done(v) if v["ok"] == true => v["result"].clone(),
            Outcome::Done(v) => panic!("{method}: {v}"),
            Outcome::Screenshot { .. } => panic!("{method} took a screenshot"),
        }
    }

    /// An agent called `name` in the window's shared document.
    pub(crate) fn agent(a: &mut WordApp, lobby: &Lobby, name: &str) -> Replica {
        let key = owner(a, "room.invite", json!({"name": name, "role": "editor"}))["key"].as_str().unwrap().to_string();
        lobby.join_with(&key).unwrap()
    }

    pub(crate) fn runs(copy: &mut Replica, command: &str, params: serde_json::Value) {
        hosted::call(copy, "engine.execute", &json!({"command": command, "params": params})).unwrap();
    }

    /// Where the window draws everyone else, as of `now`, with the pointer at `pointer`.
    fn drawn(a: &mut WordApp, now: f64, pointer: Option<egui::Pos2>) -> Vec<Mark> {
        drawn_in(a, Rect::EVERYTHING, now, pointer)
    }

    /// [`drawn`], with the pages seen in `visible` only.
    fn drawn_in(a: &mut WordApp, visible: Rect, now: f64, pointer: Option<egui::Pos2>) -> Vec<Mark> {
        let layout = a.session.layout();
        let others = a.others_here();
        let pages = super::Pages {
            layout: &layout,
            doc: &a.session.doc,
            page_hint: a.session.page_hint,
            rects: &a.canvas.page_rects,
            bands: &a.canvas.page_bands,
            scale: a.canvas.scale,
            visible,
        };
        marks(&pages, &others, now, pointer, a.canvas.follow)
    }

    #[test]
    fn others_are_drawn_where_they_are_with_their_names_for_a_while() {
        let (mut h, lobby) = harness("one two three\nfour five six");
        owner(h.state_mut(), "room.share", json!({}));
        let mut ada = agent(h.state_mut(), &lobby, "Ada");
        let mut bob = agent(h.state_mut(), &lobby, "Bob");
        runs(&mut ada, "select.text", json!({"text": "two"}));
        runs(&mut bob, "select.text", json!({"text": "five"}));
        runs(&mut bob, "caret.right", json!({}));
        h.run_steps(3);
        let now = crate::now_ms();
        let a = h.state_mut();
        let seen = drawn(a, now, None);
        assert_eq!(seen.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["Ada", "Bob"]);
        let (ada_m, bob_m) = (&seen[0], &seen[1]);
        assert_eq!(ada_m.rects.len(), 1, "Ada's selection");
        assert!(bob_m.rects.is_empty(), "Bob has a caret");
        let (ada_bar, bob_bar) = (ada_m.caret.unwrap(), bob_m.caret.unwrap());
        assert!(bob_bar[0].y > ada_bar[0].y, "Bob is on the next line");
        // Ada's caret is at the end of her selection, after "two".
        assert!((ada_bar[0].x - ada_m.rects[0].max.x).abs() < 1.0);
        assert_ne!(ada_m.color, bob_m.color);
        assert!(seen.iter().all(|m| m.flag), "names show right after they moved");
        // Later they don't, but while the pointer is over a caret, or for someone followed, they do.
        let later = now + FLAG_MS + 1.0;
        assert!(drawn(a, later, None).iter().all(|m| !m.flag));
        let over_bob = drawn(a, later, Some(bob_bar[0]));
        assert_eq!(over_bob.iter().map(|m| m.flag).collect::<Vec<_>>(), [false, true]);
        a.run("ui.follow", json!({"participant": ada.id()})).unwrap();
        assert_eq!(drawn(a, later, None).iter().map(|m| m.flag).collect::<Vec<_>>(), [true, false]);
    }

    #[test]
    fn others_move_with_the_person_s_typing_and_their_names_go_once_they_stay_put() {
        let (mut h, lobby) = harness("one two three");
        owner(h.state_mut(), "room.share", json!({}));
        let mut ada = agent(h.state_mut(), &lobby, "Ada");
        runs(&mut ada, "caret.set", json!({"pos": wordcraft_doc::Pos::body(0, 7)}));
        h.run_steps(2);
        let at = |a: &WordApp| a.others_here().first().and_then(|(o, t)| Some((o.at.as_ref()?.focus.off, *t)));
        let (off, moved) = at(h.state()).unwrap();
        assert_eq!(off, 7);
        // Frames pass with Ada where she is: her name's time doesn't start again.
        h.run_steps(5);
        assert_eq!(at(h.state()), Some((7, moved)));
        // The person types before her: she is drawn after what they typed, in the same frame.
        let a = h.state_mut();
        a.run("caret.set", json!({"pos": wordcraft_doc::Pos::body(0, 0)})).unwrap();
        a.run("text.insert", json!({"text": "Hi "})).unwrap();
        h.run_steps(1);
        assert_eq!(at(h.state()).map(|(off, _)| off), Some(10));
    }

    #[test]
    fn nothing_is_drawn_for_a_document_shared_with_no_one_or_not_shown() {
        let (mut h, lobby) = harness("one two three");
        owner(h.state_mut(), "room.share", json!({}));
        h.run_steps(2);
        assert!(drawn(h.state_mut(), crate::now_ms(), None).is_empty());
        let mut ada = agent(h.state_mut(), &lobby, "Ada");
        runs(&mut ada, "select.text", json!({"text": "two"}));
        h.run_steps(2);
        assert_eq!(drawn(h.state_mut(), crate::now_ms(), None).len(), 1);
        h.state_mut().execute("file.new", json!({})).unwrap();
        h.run_steps(2);
        assert!(h.state_mut().others_here().is_empty(), "another document is shown");
        assert!(h.state_mut().avatars().is_empty());
    }

    #[test]
    fn following_someone_keeps_their_caret_in_view_until_the_person_types() {
        let text: Vec<String> = (0..300).map(|i| format!("Line {i} of a long document")).collect();
        let (mut h, lobby) = harness(&text.join("\n"));
        owner(h.state_mut(), "room.share", json!({}));
        let mut ada = agent(h.state_mut(), &lobby, "Ada");
        runs(&mut ada, "select.text", json!({"text": "Line 290 "}));
        h.run_steps(3);
        let top = h.state().canvas.scroll_offset.y;
        let a = h.state_mut();
        assert_eq!(a.run("ui.follow", json!({})).unwrap(), json!({"following": null}), "no value reads it");
        assert_eq!(a.run("ui.follow", json!({"participant": ada.id()})).unwrap(), json!({"following": ada.id()}));
        h.run_steps(20);
        let followed = h.state().canvas.scroll_offset.y;
        assert!(followed > top + 1000.0, "the view went to Ada's caret: {top} → {followed}");
        // She moves on, and the view goes with her.
        runs(&mut ada, "select.text", json!({"text": "Line 120 "}));
        h.run_steps(20);
        let moved = h.state().canvas.scroll_offset.y;
        assert!(moved < followed - 1000.0, "the view went back up with her: {followed} → {moved}");
        // The person types: following stops, and the view stays with their own work.
        h.input_mut().events.push(egui::Event::Text("x".into()));
        h.run_steps(2);
        assert_eq!(h.state().canvas.follow, None);
        assert_eq!(h.state_mut().run("ui.follow", json!({})).unwrap(), json!({"following": null}));
    }

    #[test]
    fn typing_through_an_input_method_stops_following() {
        // An input method's text comes as its own events, not as keys or text: composing,
        // committing and deleting around the caret each take the view back.
        let text: Vec<String> = (0..300).map(|i| format!("Line {i} of a long document")).collect();
        let (mut h, lobby) = harness(&text.join("\n"));
        owner(h.state_mut(), "room.share", json!({}));
        let mut ada = agent(h.state_mut(), &lobby, "Ada");
        runs(&mut ada, "select.text", json!({"text": "Line 290 "}));
        h.run_steps(3);
        let events = [
            egui::Event::Ime(egui::ImeEvent::Preedit { text: "か".into(), active_range_chars: None }),
            egui::Event::Ime(egui::ImeEvent::Commit("漢字".into())),
            egui::Event::Ime(egui::ImeEvent::DeleteSurrounding { before_chars: 1, after_chars: 0 }),
        ];
        for event in events {
            h.state_mut().run("ui.follow", json!({"participant": ada.id()})).unwrap();
            h.run_steps(3);
            assert_eq!(h.state().canvas.follow, Some(ada.id()));
            h.input_mut().events.push(event.clone());
            h.run_steps(2);
            assert_eq!(h.state().canvas.follow, None, "{event:?} stops following");
        }
        // A composition ending with nothing types nothing.
        h.state_mut().run("ui.follow", json!({"participant": ada.id()})).unwrap();
        h.input_mut().events.push(egui::Event::Ime(egui::ImeEvent::Preedit { text: String::new(), active_range_chars: None }));
        h.run_steps(2);
        assert_eq!(h.state().canvas.follow, Some(ada.id()));
    }

    #[test]
    fn with_white_space_hidden_others_in_a_hidden_header_are_not_drawn_over_the_text() {
        // Hide White Space overlaps each page's rect with its neighbours' in the hidden margins:
        // a header there would land on the page before's text.
        let text: Vec<String> = (0..300).map(|i| format!("Line {i} of a long document")).collect();
        let (mut h, lobby) = harness(&text.join("\n"));
        owner(h.state_mut(), "room.share", json!({}));
        let mut ada = agent(h.state_mut(), &lobby, "Ada");
        runs(&mut ada, "insert.header", json!({"text": "Running head"}));
        runs(&mut ada, "select.all", json!({}));
        owner(h.state_mut(), "select.text", json!({"text": "Line 150 "}));
        h.run_steps(3);
        let page = h.state().session.page_hint;
        assert!(page > 0, "the person is past the first page, whose top always shows");
        let now = crate::now_ms();
        let shown = drawn(h.state_mut(), now, None);
        let [m] = shown.as_slice() else { panic!("{shown:?}") };
        let pr = h.state().canvas.page_rects[page];
        assert!(m.caret.is_some() && !m.rects.is_empty(), "Ada's header selection is drawn: {m:?}");
        assert!(m.rects.iter().all(|r| r.min.y >= pr.min.y && r.max.y < pr.min.y + pr.height() / 4.0), "in the header: {m:?}");
        owner(h.state_mut(), "view.hideWhiteSpace", json!({"value": true}));
        h.run_steps(3);
        assert!(h.state().session.view.hide_white_space);
        let shown = drawn(h.state_mut(), now, None);
        let [m] = shown.as_slice() else { panic!("{shown:?}") };
        assert_eq!((m.caret, m.rects.len()), (None, 0), "nothing of Ada's hidden header is drawn: {m:?}");
        // Body text is still drawn where it shows.
        runs(&mut ada, "insert.closeHeader", json!({}));
        runs(&mut ada, "select.text", json!({"text": "Line 151 "}));
        h.run_steps(3);
        let shown = drawn(h.state_mut(), now, None);
        let [m] = shown.as_slice() else { panic!("{shown:?}") };
        assert!(m.caret.is_some() && m.rects.len() == 1, "{m:?}");
    }

    #[test]
    fn scrolling_read_mode_s_side_margins_stops_following() {
        // Read Mode's wheel turns pages over the whole canvas, its side margins too: that takes
        // the view back from the participant followed, as over the column.
        let text: Vec<String> = (0..300).map(|i| format!("Line {i} of a long document")).collect();
        let (mut h, lobby) = harness(&text.join("\n"));
        owner(h.state_mut(), "room.share", json!({}));
        let mut ada = agent(h.state_mut(), &lobby, "Ada");
        runs(&mut ada, "select.text", json!({"text": "Line 290 "}));
        owner(h.state_mut(), "view.readMode", json!({}));
        owner(h.state_mut(), "view.readColumnWidth", json!({"value": "narrow"}));
        h.run_steps(3);
        assert!(h.state().session.view.read_mode);
        h.state_mut().run("ui.follow", json!({"participant": ada.id()})).unwrap();
        h.run_steps(3);
        let column = h.state().canvas.canvas_rect.unwrap();
        assert!(column.min.x > 60.0, "the column is centred, with margins beside it: {column:?}");
        let margin = egui::pos2(column.min.x - 30.0, column.center().y);
        h.input_mut().events.push(egui::Event::PointerMoved(margin));
        h.input_mut().events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -120.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
            source: egui::MouseWheelSource::Unknown,
        });
        h.run_steps(2);
        assert_eq!(h.state().canvas.follow, None, "a wheel over the margin stops following");
    }

    #[test]
    fn follow_takes_only_someone_else_in_the_shared_document_shown() {
        let (mut h, lobby) = harness("one two three");
        let a = h.state_mut();
        assert!(a.run("ui.follow", json!({"participant": 1})).is_err(), "nothing is shared");
        owner(a, "room.share", json!({}));
        let mut ada = agent(a, &lobby, "Ada");
        runs(&mut ada, "caret.set", json!({"pos": wordcraft_doc::Pos::body(0, 3)}));
        h.run_steps(2);
        let a = h.state_mut();
        for junk in [
            json!({"participant": "Ada"}),
            json!({"participant": -1}),
            json!({"participant": 1}),
            json!({"participant": 99}),
            json!({"participant": 1.5}),
            json!({"participant": [ada.id()]}),
        ] {
            assert!(a.run("ui.follow", junk.clone()).is_err(), "{junk}");
            assert_eq!(a.canvas.follow, None, "{junk}");
        }
        a.run("ui.follow", json!({"participant": ada.id()})).unwrap();
        assert_eq!(control_inspect(a)["following"], json!(ada.id()));
        assert_eq!(a.run("ui.follow", json!({"participant": null})).unwrap(), json!({"following": null}), "null stops");
        a.run("ui.follow", json!({"participant": ada.id()})).unwrap();
        // Someone who leaves is no longer followed.
        drop(ada);
        h.run_steps(2);
        assert_eq!(h.state().canvas.follow, None);
        // Nor once the document isn't shared any more.
        let a = h.state_mut();
        let mut ben = agent(a, &lobby, "Ben");
        runs(&mut ben, "caret.set", json!({"pos": wordcraft_doc::Pos::body(0, 1)}));
        h.run_steps(2);
        let a = h.state_mut();
        a.run("ui.follow", json!({"participant": ben.id()})).unwrap();
        owner(a, "room.stop", json!({}));
        assert_eq!(a.canvas.follow, None, "stopping sharing stops following");
        assert_eq!(control_inspect(a)["following"], json!(null));
        assert_eq!(a.run("ui.follow", json!({})).unwrap(), json!({"following": null}));
    }

    #[test]
    fn a_selection_longer_than_what_is_drawn_is_shaded_where_the_person_looks() {
        // More lines selected than selection rectangles are drawn: the end of the selection,
        // on screen, is still shaded, and so is someone else's selection there.
        let text = (0..2600).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let (mut h, lobby) = harness(&text);
        h.run_steps(2);
        let a = h.state_mut();
        owner(a, "room.share", json!({}));
        let mut ada = agent(a, &lobby, "Ada");
        let mut ben = agent(a, &lobby, "Ben");
        runs(&mut ada, "select.all", json!({}));
        // The last line, on the last page whatever fonts lay the document out.
        runs(&mut ben, "select.text", json!({"text": "line 2599"}));
        owner(h.state_mut(), "caret.docEnd", json!({}));
        h.run_steps(4);
        let a = h.state_mut();
        let last = a.canvas.page_rects.last().copied().unwrap();
        let marks = drawn_in(a, last, 0.0, None);
        let rects = |id| marks.iter().find(|m| m.who == id).map_or(0, |m| m.rects.iter().filter(|r| r.intersects(last)).count());
        assert!(rects(ada.id()) > 0, "Ada's selection is shaded on the last page");
        assert!(marks.iter().flat_map(|m| &m.rects).all(|r| r.intersects(last)), "only what is on it");
        assert_eq!(rects(ben.id()), 1, "and Ben's after hers");
    }

    #[test]
    fn a_long_selection_is_laid_out_only_on_the_pages_on_screen() {
        // Codex's security review: a participant selecting the whole of a long document must
        // not cost the window every line of it each frame.
        let text = (0..2600).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let mut s = Session::new(Document::from_text(&text));
        let layout = s.layout();
        let n = layout.pages.len();
        assert!(n > 20, "{n} pages");
        let all = |s: &mut Session| {
            s.run("select.all", &json!({})).unwrap();
            s.sel.ordered()
        };
        let (start, end) = all(&mut s);
        let (from, to) = super::cut_to_pages(&layout, &start, &end, n - 2..=n - 1).unwrap();
        assert!(layout.page_of(&from).is_some_and(|p| p >= n - 2), "from {from:?}, on {:?}", layout.page_of(&from));
        assert_eq!(to, end, "to the end of the selection, on the last page");
        let rects = layout.selection_rects(&s.doc, &from, &to, 0);
        assert!(!rects.is_empty() && rects.iter().all(|(pi, _)| *pi >= n - 2), "only those pages are laid out");
        // Backwards is the same selection; one off those pages, or pages past the end, is none.
        assert_eq!(super::cut_to_pages(&layout, &end, &start, n - 2..=n - 1), Some((from, to)));
        s.run("select.text", &json!({"text": "line 3"})).unwrap();
        let (a, b) = s.sel.ordered();
        assert_eq!(super::cut_to_pages(&layout, &a, &b, n - 2..=n - 1), None);
        assert_eq!(super::cut_to_pages(&layout, &start, &end, n..=n + 1), None);
        assert!(super::cut_to_pages(&layout, &a, &b, 0..=0).is_some(), "it is on the first page");
    }

    fn control_inspect(a: &mut WordApp) -> serde_json::Value {
        owner(a, "ui.inspect", json!({}))
    }
}
