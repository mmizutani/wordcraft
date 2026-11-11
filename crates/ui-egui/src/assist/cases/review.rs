//! **Review this case** (#107): the consolidation pass as one turn the person can see. Their click
//! on Details › Case asks one ordinary turn in the bound case's conversation
//! ([`crate::assist::review`]) with the fixed [`REVIEW_ASK`] and the case's notes, balance, plan
//! and most recent record lines as one data-grade source, under the same proof as recall that no
//! one else reads the document. The answer's review verbs take the normal marker route
//! ([`super::tick`]): a correction or retirement of the model's own unconfirmed note applies, of a
//! confirmed note is a card the person answers, `supported` and `gap` are rows that change
//! nothing. Outside a review's turn the transcript drops the verbs before they get here.

use egui::{RichText, Ui};
use serde_json::json;
use wordcraft_assist::cases::{CaseRef, NoteId, Quoted, RecallAsk, Shelf};
use wordcraft_assist::{Sent, Written};

use crate::WordApp;
use crate::theme::Tokens;

/// What the review's turn carries ([`RecallAsk::review`]), under the same proof as recall
/// ([`super::proof`]); `None`, with `sent` saying why, withholds the review.
pub(crate) fn for_review(app: &WordApp, sent: &mut Sent) -> Option<RecallAsk> {
    let (shelf, at, today, alone) = super::proof(app, sent)?;
    Some(RecallAsk::review(shelf, at, today, alone))
}

/// A review says the record supports note `note`: a quiet row with its evidence, if the
/// conversation's case has that note; a note it doesn't have is dropped and counted, as an
/// unknown marker is. One read of the case's file, the size the page reads on a frame.
pub(crate) fn supported(c: &mut super::super::Conversation, dir: Option<&std::path::Path>, at: &CaseRef, note: NoteId, evidence: &Quoted) {
    let known = dir.and_then(|dir| Shelf::at(dir).open(at).ok()).is_some_and(|case| case.notes.iter().any(|n| n.id == note));
    if known {
        c.assistant.case_written(Written::Supported, Some(note), evidence.for_person());
    } else {
        c.assistant.drop_case_marker();
    }
}

/// The button on Details › Case, with what it costs in front of it.
pub(crate) fn button(app: &mut WordApp, ui: &mut Ui, t: &Tokens) {
    if ui.small_button(tl!("Review this case")).clicked() {
        super::run(app, "assist.memory.case.review", json!({}));
    }
    let says = tl!(
        "One turn in this document's conversation, which spends one request on your plan: the assistant checks this case's unconfirmed notes against its record, corrects or retires them, says which the record supports, and asks what the record can't answer. A note you confirmed changes only when you keep the card."
    );
    ui.add(egui::Label::new(RichText::new(says).small().color(t.text_dim)).wrap());
    ui.add_space(4.0);
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wordcraft_assist::cases::{Applied, Card, ModelWrite, PersonOp, REVIEW_ASK, Shelf, Standing};
    use wordcraft_assist::{Entry, Fake, Step};

    use super::super::tests::{HERE, ask, cases, click, draft, entries, frames_until, harness, notes_of, recalled_json, today};
    use crate::WordApp;
    use crate::assist::memory::tests::{Folder, settle, window};

    const MODEL_NOTE: &str = "Kestrel Freight KK signs by March.";
    const CORRECTED: &str = "Kestrel Freight KK signs by May.";
    const WHY: &str = "Turn 3 of the record reports the May date.";

    fn correct(note: &str, text: &str) -> String {
        format!(r#"[[case]]{{"op":"correct","note":"{note}","text":"{text}","why":"{WHY}"}}[[/case]]"#)
    }

    fn retire(note: &str) -> String {
        format!(r#"[[case]]{{"op":"retire","note":"{note}","why":"{WHY}"}}[[/case]]"#)
    }

    /// The bound case with the person's confirmed n1 ([`HERE`]) and the model's unconfirmed n2.
    fn with_model_note(folder: &Folder) -> (super::super::tests::Cases, WordApp) {
        let c = cases(folder);
        let shelf = Shelf::at(&folder.0);
        assert!(matches!(shelf.model(&c.here, ModelWrite::Note(draft(MODEL_NOTE)), today()), Ok(Applied::Added(_))));
        let mut a = window(folder);
        a.bind_case(Some(c.here.clone()));
        (c, a)
    }

    /// Click **Review this case** as the person: the review turn runs against `fake`.
    fn review(a: &mut WordApp, fake: Fake) -> std::sync::Arc<std::sync::Mutex<wordcraft_assist::Seen>> {
        let seen = fake.seen();
        a.assist_with(Box::new(fake));
        a.run("assist.memory.case.review", json!({})).unwrap();
        settle(a);
        seen
    }

    fn dropped(a: &mut WordApp) -> u32 {
        crate::assist::details_for(a).turns.last().map_or(0, |t| t.markers_dropped)
    }

    /// A correction of the model's own unconfirmed note applies, with the why as how; of the
    /// person's confirmed note it is a card that changes nothing until Keep, and Keep applies it
    /// as the person's edit.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_correction_of_an_unconfirmed_note_applies_and_of_a_confirmed_one_is_a_card_until_keep() {
        use egui_kittest::kittest::Queryable;
        let folder = Folder::new();
        let (c, mut a) = with_model_note(&folder);
        a.session.view.assistant_pane = true;
        let answer = format!("Reviewed. {} {}", correct("n2", CORRECTED), correct("n1", "Cap liability at eighteen months."));
        let seen = review(&mut a, Fake::new().then(vec![Step::Say(answer)]));
        assert_eq!(seen.lock().unwrap().turns[0].prompt, REVIEW_ASK);
        frames_until(&mut a, "the correction never applied", |_| notes_of(&folder, &c.here)[1].0 == CORRECTED);
        let case = Shelf::at(&folder.0).open(&c.here).unwrap();
        assert_eq!(case.notes[1].how.as_ref().unwrap().for_person(), WHY);
        assert_eq!(case.notes[1].standing, Standing::Unconfirmed);
        assert_eq!(notes_of(&folder, &c.here)[0], (HERE.to_string(), true), "the confirmed note stands");
        let shown = entries(&a);
        assert!(shown.iter().any(|e| matches!(e, Entry::CaseWritten { what: wordcraft_assist::Written::Corrected, note: Some(_), .. })), "{shown:?}");
        assert!(shown.iter().any(|e| matches!(e, Entry::CaseCard { card: Card::Write(ModelWrite::Correct { .. }), answer: None, .. })), "{shown:?}");
        let mut h = harness(a);
        assert!(h.query_by_label_contains("Now: ").is_some(), "the card shows the note as it stands");
        crate::assist::memory::tests::control_click(&mut h, "Keep");
        assert_eq!(notes_of(&folder, &c.here)[0].0, HERE, "the control channel's Keep");
        h.get_by_label("Keep").click();
        h.run_steps(3);
        assert_eq!(notes_of(&folder, &c.here)[0], ("Cap liability at eighteen months.".to_string(), true), "the person's Keep is their edit");
        let case = Shelf::at(&folder.0).open(&c.here).unwrap();
        assert_eq!(case.notes[0].how.as_ref().unwrap().for_person(), WHY);
        assert!(entries(h.state()).iter().any(|e| matches!(e, Entry::CaseCard { answer: Some(_), .. })));
    }

    /// A retirement of the model's own unconfirmed note removes it and its text blocks the model's
    /// noting it again; of the person's confirmed note it is a card, and nothing goes.
    #[test]
    fn a_retired_unconfirmed_note_goes_and_blocks_a_re_note_and_a_retired_confirmed_note_is_a_card() {
        let folder = Folder::new();
        let (c, mut a) = with_model_note(&folder);
        let answer = format!("Reviewed. {} {}", retire("n2"), retire("n1"));
        review(&mut a, Fake::new().then(vec![Step::Say(answer)]));
        frames_until(&mut a, "the retirement never applied", |_| notes_of(&folder, &c.here).len() == 1);
        assert_eq!(notes_of(&folder, &c.here), [(HERE.to_string(), true)]);
        let shelf = Shelf::at(&folder.0);
        assert_eq!(shelf.model(&c.here, ModelWrite::Note(draft(MODEL_NOTE)), today()), Ok(Applied::Known), "retired text blocks the model");
        let shown = entries(&a);
        assert!(shown.iter().any(|e| matches!(e, Entry::CaseWritten { what: wordcraft_assist::Written::Retired, .. })), "{shown:?}");
        assert!(shown.iter().any(|e| matches!(e, Entry::CaseCard { card: Card::Write(ModelWrite::Retire { .. }), answer: None, .. })), "{shown:?}");
        assert_eq!(dropped(&mut a), 0);
    }

    /// A review verb in an ordinary turn changes nothing and is counted among the dropped markers.
    #[test]
    fn a_review_verb_in_an_ordinary_turn_changes_nothing_and_is_counted() {
        let folder = Folder::new();
        let (c, mut a) = with_model_note(&folder);
        let answer = format!("Summarized. {} {}", correct("n2", CORRECTED), retire("n1"));
        ask(&mut a, Fake::new().then(vec![Step::Say(answer)]), "Summarize this");
        for _ in 0..5 {
            crate::assist::tick(&mut a);
        }
        assert_eq!(notes_of(&folder, &c.here), [(HERE.to_string(), true), (MODEL_NOTE.to_string(), false)]);
        assert_eq!(dropped(&mut a), 2);
        assert!(!entries(&a).iter().any(|e| matches!(e, Entry::CaseCard { .. } | Entry::CaseWritten { .. })));
    }

    /// A note id the conversation's case doesn't have, be it the same client's other case's or
    /// another client's, is dropped and counted, and that case's note is untouched.
    #[test]
    fn a_note_id_from_another_case_is_dropped_and_touches_nothing() {
        let folder = Folder::new();
        let c = cases(&folder);
        let mut a = window(&folder);
        a.bind_case(Some(c.here.clone()));
        // The neighbour has n2 (a model's unconfirmed note) and the other client n1; here has n1 only.
        let neighbor_before = notes_of(&folder, &c.neighbor);
        let other_before = notes_of(&folder, &c.other);
        let answer = format!(
            "Reviewed. {} {} {}",
            retire("n2"),
            correct("n7", "planted"),
            r#"[[case]]{"op":"supported","note":"n2","evidence":"planted"}[[/case]]"#
        );
        review(&mut a, Fake::new().then(vec![Step::Say(answer)]));
        frames_until(&mut a, "the markers were never counted", |a| dropped(a) == 3);
        assert_eq!(notes_of(&folder, &c.neighbor), neighbor_before);
        assert_eq!(notes_of(&folder, &c.other), other_before);
        assert_eq!(notes_of(&folder, &c.here), [(HERE.to_string(), true)]);
        assert!(!entries(&a).iter().any(|e| matches!(e, Entry::CaseCard { .. } | Entry::CaseWritten { .. })));
    }

    /// The review turn carries the case's notes and its record as one data-grade source, nothing
    /// of them in the instructions, and the fixed prompt; `supported` and `gap` show as rows. With
    /// a second participant the review is withheld and says who.
    #[test]
    fn the_review_turn_carries_notes_and_record_as_data_and_is_withheld_with_a_second_participant() {
        use wordcraft_engine::room::Role;
        let folder = Folder::new();
        let (c, mut a) = with_model_note(&folder);
        let shelf = Shelf::at(&folder.0);
        shelf.person(click(), &c.here, PersonOp::Record(true), today()).unwrap();
        let asked = "Summarize the Kestrel draft's audit clause";
        let fake = Fake::new().then(vec![Step::Say("Summarized.".into())]).then(vec![Step::Say(
            r#"Reviewed. [[case]]{"op":"supported","note":"n1","evidence":"Turn 1 asked about the cap."}[[/case]] [[case]]{"op":"gap","question":"Did the audit clause land?"}[[/case]]"#.into(),
        )]);
        let seen = ask(&mut a, fake, asked);
        frames_until(&mut a, "the turn never reached the record", |_| {
            wordcraft_assist::cases::search_log(&shelf, &c.here, "audit clause", 4).is_ok_and(|s| !s.hits.is_empty())
        });
        a.run("assist.memory.case.review", json!({})).unwrap();
        settle(&mut a);
        let turns = seen.lock().unwrap().turns.clone();
        assert_eq!(turns.len(), 2);
        let turn = &turns[1];
        assert_eq!(turn.prompt, REVIEW_ASK);
        let items = recalled_json(turn);
        assert_eq!(items.len(), 1, "one item: the whole case");
        let notes = items[0]["notes"].as_array().unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0]["id"], "n1");
        assert_eq!(notes[0]["body"]["rule"], HERE);
        assert_eq!(notes[1]["standing"], "unconfirmed");
        let record = items[0]["record"].as_array().unwrap();
        assert!(record.iter().any(|r| r["t"] == "turn" && r["asked"] == asked), "{record:#?}");
        let memory = turn.memory.as_ref().map(|m| m.as_str().to_string()).unwrap_or_default();
        assert!(!memory.contains(HERE) && !memory.contains(asked) && !memory.contains(MODEL_NOTE), "{memory}");
        for _ in 0..5 {
            crate::assist::tick(&mut a);
        }
        let shown = entries(&a);
        assert!(
            shown.iter().any(
                |e| matches!(e, Entry::CaseWritten { what: wordcraft_assist::Written::Supported, note: Some(_), text } if text.starts_with("Turn 1"))
            ),
            "{shown:?}"
        );
        assert!(
            shown
                .iter()
                .any(|e| matches!(e, Entry::CaseWritten { what: wordcraft_assist::Written::Gap, note: None, text } if text.starts_with("Did"))),
            "{shown:?}"
        );
        assert_eq!(notes_of(&folder, &c.here).len(), 2, "rows change nothing");
        // Someone else can read the document now: no review.
        let _key = a.shared_room().unwrap().invite("Bob", Role::Viewer).unwrap();
        let r = a.run("assist.memory.case.review", json!({}));
        assert!(r.as_ref().is_err_and(|e| e.contains("Bob")), "{r:?}");
        assert_eq!(seen.lock().unwrap().turns.len(), 2, "no turn was sent");
    }

    /// A click the control channel made on **Review this case** starts nothing: the review is the
    /// person's alone, as every case command is. Their own click asks the turn.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_control_channel_click_on_review_this_case_starts_nothing() {
        use egui_kittest::kittest::Queryable;
        let folder = Folder::new();
        let (_, mut a) = with_model_note(&folder);
        let fake = Fake::new();
        let seen = fake.seen();
        a.assist_with(Box::new(fake));
        a.session.view.assistant_pane = true;
        a.assist.view = crate::assist::PaneView::Details;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 3000.0)).build_ui_state(
            |ui, app: &mut WordApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            a,
        );
        h.run_steps(4);
        assert_eq!(h.state_mut().execute("assist.memory.case.review", json!({})), Err(crate::assist::memory::YOURS.to_string()), "a script's");
        crate::assist::memory::tests::control_click(&mut h, "Review this case");
        assert!(h.state().assist.conversation.is_none(), "the control channel's click started a conversation");
        assert!(seen.lock().unwrap().turns.is_empty());
        h.get_by_label("Review this case").click();
        h.run_steps(3);
        settle(h.state_mut());
        let turns = seen.lock().unwrap().turns.clone();
        assert_eq!(turns.len(), 1, "the person's click asked the review");
        assert_eq!(turns[0].prompt, REVIEW_ASK);
    }
}
