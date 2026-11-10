//! Which document is a later draft of a case's contract: a paragraph-text fingerprint of each
//! draft the person bound ([`DraftPrint`]), and the share of a new document's paragraphs a past
//! draft holds ([`DraftPrint::overlap`]). No paragraph text is kept, only hashes, and the result
//! goes to the person's screen as a suggestion that still takes their click; it never reaches a
//! model, a log or a file. Paragraph text is hashed rather than `w14:paraId` read, because the
//! text survives another editor and needs no fact about Word nobody has verified.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wordcraft_doc::{Document, StoryRef};

use super::record::{Day, MAX_TEXT, Quoted};
use super::text::folded;

/// Most hashes one print keeps: the smallest ones, so two prints of any size compare in bounded
/// work. About 3 KiB of `case.json` each.
pub const MAX_HASHES: usize = 256;

/// Most prints a case keeps; the oldest goes first.
pub const MAX_DRAFTS: usize = 8;

/// Fewest letters and digits a paragraph has to count: a heading number, "(a)", an empty line or a
/// signature label is shared by every contract and says nothing about lineage.
pub const MIN_CHARS: usize = 24;

/// Most bytes read of one paragraph's text before it is hashed: a paragraph is a clause, and one
/// of megabytes is hostile.
const MAX_PARAGRAPH_BYTES: usize = 4096;

/// A paragraph-text fingerprint of one draft: the [`MAX_HASHES`] smallest SHA-256-truncated hashes
/// of its non-trivial paragraphs, each folded as the case text is ([`folded`]). Sorted, distinct.
/// Bounded, and no text is kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct DraftPrint {
    pub on: Day,
    /// The document's title as the case shows it.
    pub name: Quoted,
    hashes: Vec<u32>,
}

impl DraftPrint {
    /// The print of `doc`'s body, called `name`, bound on `on`.
    pub fn of(doc: &Document, name: &str, on: Day) -> DraftPrint {
        let mut smallest: BTreeSet<u32> = BTreeSet::new();
        let mut text = String::new();
        for path in doc.para_paths(StoryRef::Body) {
            let Some(para) = doc.para(StoryRef::Body, &path) else { continue };
            text.clear();
            para.plain_text_up_to(&mut text, MAX_PARAGRAPH_BYTES);
            let Some(hash) = hash_of(&text) else { continue };
            if smallest.len() < MAX_HASHES {
                smallest.insert(hash);
            } else if smallest.last().is_some_and(|last| hash < *last) {
                smallest.insert(hash);
                smallest.pop_last();
            }
        }
        let mut name = Quoted::parse(name);
        name.cut(MAX_TEXT);
        DraftPrint { on, name, hashes: smallest.into_iter().collect() }
    }

    /// How many paragraphs the print stands for, at most [`MAX_HASHES`].
    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }

    /// The share of `self`'s paragraphs that `old` has: containment of the new document in the
    /// old draft, not Jaccard, since a later draft adds and drops clauses and what matters is how
    /// much of what is on screen was there before. Exact while neither print was cut at
    /// [`MAX_HASHES`]; past that, the bottom-k estimate over the k smallest hashes of the union
    /// (k the smaller print's size): the share of those from `self` that `old` has too. Zero when
    /// either is empty.
    pub fn overlap(&self, old: &DraftPrint) -> f32 {
        let (a, b) = (&self.hashes, &old.hashes);
        if a.is_empty() || b.is_empty() {
            return 0.0;
        }
        // Whole prints: the k smallest of the union is the union, and the estimate is exact.
        let k = if a.len() < MAX_HASHES && b.len() < MAX_HASHES { a.len().saturating_add(b.len()) } else { a.len().min(b.len()) };
        let (mut i, mut j, mut taken, mut from_new, mut in_both) = (0usize, 0usize, 0usize, 0u32, 0u32);
        while taken < k {
            match (a.get(i), b.get(j)) {
                (Some(x), Some(y)) if x == y => {
                    from_new += 1;
                    in_both += 1;
                    i += 1;
                    j += 1;
                }
                (Some(x), Some(y)) if x < y => {
                    from_new += 1;
                    i += 1;
                }
                (Some(_), None) => {
                    from_new += 1;
                    i += 1;
                }
                (None, Some(_)) | (Some(_), Some(_)) => j += 1,
                (None, None) => break,
            }
            taken += 1;
        }
        if from_new == 0 { 0.0 } else { in_both as f32 / from_new as f32 }
    }

    /// Hold the hashes to the limit, sorted and distinct, as a file may not: whether anything went.
    pub(crate) fn clamp(&mut self) -> bool {
        let before = self.hashes.clone();
        let mut set: BTreeSet<u32> = self.hashes.iter().copied().collect();
        while set.len() > MAX_HASHES {
            set.pop_last();
        }
        self.hashes = set.into_iter().collect();
        let mut cut = self.hashes != before;
        cut |= self.name.cut(MAX_TEXT);
        cut
    }

    #[doc(hidden)]
    pub fn hashes(&self) -> &[u32] {
        &self.hashes
    }
}

/// The hash of one paragraph's folded text, or `None` for a trivial one.
fn hash_of(text: &str) -> Option<u32> {
    let folded = folded(text);
    if folded.chars().filter(|c| c.is_alphanumeric()).count() < MIN_CHARS {
        return None;
    }
    let digest = Sha256::digest(folded.as_bytes());
    let bytes: [u8; 4] = digest.get(..4).and_then(|b| b.try_into().ok())?;
    Some(u32::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cases::record::{Case, PersonOp};

    fn day() -> Day {
        Day::parse("2026-11-10").unwrap()
    }

    /// Thirty clauses of a synthetic master services agreement.
    fn clauses() -> Vec<String> {
        (0..30)
            .map(|i| format!("Clause {i}. Kestrel Freight KK shall perform the services described in schedule {i} with reasonable skill and care."))
            .collect()
    }

    fn doc(paragraphs: &[String]) -> Document {
        Document::from_text(&paragraphs.join("\n"))
    }

    fn print(paragraphs: &[String]) -> DraftPrint {
        DraftPrint::of(&doc(paragraphs), "Kestrel MSA 2026", day())
    }

    #[test]
    fn a_later_draft_overlaps_high_and_an_unrelated_contract_low() {
        let first = clauses();
        let mut later = first.clone();
        for i in [3usize, 7, 11, 19, 25] {
            later[i] = format!("Clause {i}. Kestrel Freight KK shall perform the services in schedule {i} with the care of a prudent operator.");
        }
        later.truncate(27);
        for i in 30..34 {
            later.push(format!("Clause {i}. Bluefin Systems Ltd may audit the records once a year on thirty days' notice."));
        }
        let unrelated: Vec<String> =
            (0..30).map(|i| format!("Section {i}. The tenant shall keep the premises at unit {i} in good repair and insured.")).collect();
        let old = print(&first);
        let later = print(&later);
        let unrelated = print(&unrelated);
        // 22 of the later draft's 31 paragraphs (27 kept, 5 of them edited, 4 added) are in the old draft.
        let kept = later.overlap(&old);
        assert!((kept - 22.0 / 31.0).abs() < 0.01, "{kept}");
        assert!(unrelated.overlap(&old) == 0.0, "{}", unrelated.overlap(&old));
        assert!(old.overlap(&old) == 1.0);
    }

    #[test]
    fn two_contracts_from_one_template_share_their_template_paragraphs_only() {
        let template: Vec<String> = clauses().into_iter().take(12).collect();
        let mut a = template.clone();
        a.extend((0..8).map(|i| format!("Schedule A item {i}. Kestrel Freight KK pays 1,200,000 yen a month for the Osaka route.")));
        let mut b = template.clone();
        b.extend((0..8).map(|i| format!("Schedule B item {i}. Bluefin Systems Ltd pays 900 pounds a month for the Leeds route.")));
        let (a, b) = (print(&a), print(&b));
        assert!((a.overlap(&b) - 0.6).abs() < 0.01, "{}", a.overlap(&b));
    }

    #[test]
    fn folding_and_whitespace_do_not_change_a_paragraphs_hash_but_its_words_do() {
        let a = print(&["The Supplier shall deliver the goods  by\tthe end of March.".to_string()]);
        let b = print(&["the supplier shall deliver the goods by the end of march.".to_string()]);
        let c = print(&["The Supplier shall deliver the goods by the end of April.".to_string()]);
        assert_eq!(a.hashes(), b.hashes());
        assert_ne!(a.hashes(), c.hashes());
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn trivial_paragraphs_are_not_printed() {
        let trivial = ["", "1.", "Article 3", "(a)", "   ", "第３条", "Signed: ______"].map(str::to_string);
        assert!(print(&trivial).is_empty());
        let real = "Article 3. The parties shall keep each other's information confidential.".to_string();
        assert_eq!(print(&[trivial.as_slice(), &[real]].concat()).len(), 1);
    }

    #[test]
    fn a_print_holds_no_paragraph_text_and_at_most_256_hashes_for_thousands_of_paragraphs() {
        let canary = "Canary paragraph: Kestrel Freight KK owes Bluefin Systems Ltd nothing before April.";
        let mut paragraphs = vec![canary.to_string()];
        paragraphs.extend((0..3000).map(|i| format!("Clause {i}. Kestrel Freight KK shall deliver lot {i} to the Yokohama warehouse by noon.")));
        let p = DraftPrint::of(&doc(&paragraphs), "Kestrel MSA 2026", day());
        assert_eq!(p.len(), MAX_HASHES);
        let json = serde_json::to_string(&p).unwrap();
        for word in ["Canary", "Kestrel", "Yokohama", "owes"] {
            assert!(!json.contains(word) || word == "Kestrel", "{word} in {json}");
        }
        assert!(!json.contains("Canary paragraph") && !json.contains("Yokohama"), "{json}");
        let back: DraftPrint = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        let sorted = p.hashes().windows(2).all(|w| w[0] < w[1]);
        assert!(sorted, "sorted and distinct");
    }

    #[test]
    fn a_cut_print_still_estimates_its_overlap_with_a_whole_one() {
        let big: Vec<String> =
            (0..2000).map(|i| format!("Clause {i}. Kestrel Freight KK shall deliver lot {i} to the Yokohama warehouse by noon.")).collect();
        let half: Vec<String> = big.iter().step_by(2).cloned().collect();
        let (big, half) = (print(&big), print(&half));
        assert_eq!(big.len(), MAX_HASHES);
        assert_eq!(half.len(), MAX_HASHES);
        // Every paragraph of the half is in the whole; about half the whole's are in the half.
        assert!((half.overlap(&big) - 1.0).abs() < 0.01, "{}", half.overlap(&big));
        let share = big.overlap(&half);
        assert!((0.4..=0.6).contains(&share), "{share}");
    }

    #[test]
    fn a_case_keeps_at_most_eight_prints_oldest_out_and_reads_a_file_without_them() {
        let mut case = Case::new("Kestrel MSA 2026");
        for i in 0..10 {
            let p = DraftPrint::of(&doc(&clauses()), &format!("draft {i}"), day());
            case.apply_person(PersonOp::AddDraft(p), day()).unwrap();
        }
        assert_eq!(case.drafts.len(), MAX_DRAFTS);
        let names: Vec<&str> = case.drafts.iter().map(|d| d.name.for_person()).collect();
        assert_eq!(names, ["draft 2", "draft 3", "draft 4", "draft 5", "draft 6", "draft 7", "draft 8", "draft 9"]);
        let bytes = case.to_bytes().unwrap();
        let back = Case::read(&bytes);
        assert_eq!(back, case);
        assert!(!back.damaged);
        let without = Case::read(br#"{"version":1,"title":"Kestrel MSA 2026"}"#);
        assert!(without.drafts.is_empty() && !without.damaged);
        // A file holding more than the cap, or hashes out of order, is clamped and reads as damaged.
        let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let drafts = json["drafts"].as_array_mut().unwrap();
        let mut extra = drafts[0].clone();
        extra["hashes"] = serde_json::json!([9, 3, 3, 1]);
        drafts.push(extra);
        let read = Case::read(json.to_string().as_bytes());
        assert!(read.damaged);
        assert_eq!(read.drafts.len(), MAX_DRAFTS);
        assert_eq!(read.drafts[0].name.for_person(), "draft 3", "the oldest went");
        assert_eq!(read.drafts[7].hashes(), [1, 3, 9]);
    }

    /// Nothing of a print reaches a turn: the review's source and recall's items carry neither
    /// the paragraph text (which the print never held) nor its hash numbers nor the key.
    #[test]
    fn no_turn_carries_a_print() {
        use crate::cases::record::Click;
        use crate::cases::store::Shelf;
        let dir = std::env::temp_dir().join(format!("wordcraft-lineage-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shelf = Shelf::at(&dir);
        let click = Click::by_person(true).unwrap();
        let wall = shelf.new_wall(click, "Kestrel Freight KK", "stamp", 1).unwrap();
        let at = shelf.new_case(click, &wall, "Kestrel MSA 2026", "stamp", 2).unwrap();
        let canary = "Canary clause: the liability cap is twelve months of fees.";
        let p = DraftPrint::of(&doc(&[canary.to_string()]), "Kestrel draft", day());
        assert_eq!(p.len(), 1);
        let hash = p.hashes()[0].to_string();
        shelf.person(click, &at, PersonOp::AddDraft(p), day()).unwrap();
        let (sent, sources) = crate::cases::recall::compose_for_eval_without_room_check(&shelf, &at, "liability cap", day()).unwrap();
        let review = crate::cases::recall::compose_review(&shelf, &at).unwrap();
        let review_sent = review.sent();
        let review_sources = review.into_sources();
        assert_eq!(review_sources.len(), 1);
        let texts: Vec<String> = sources.iter().chain(&review_sources).map(|s| s.as_text()).collect();
        let everything = format!("{sent:?} {review_sent:?} {texts:?}");
        for needle in [canary, &hash, "drafts", "hashes"] {
            assert!(!everything.contains(needle), "{needle} reached a turn: {everything}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
