//! Case memory for the assistant (#107): typed records of past contract cases, the ranker that
//! recalls their notes for a similar one, and the shelf that keeps them on disk. Similarity is a
//! function of closed types ([`record`]) over English and Japanese text ([`text`]), the power
//! balance then against now is a table ([`recall`]), and every change to a case on disk is a typed
//! operation under a lock ([`store`]). A turn bound to a case carries what ranks highest from it
//! and the same client's other cases, as data ([`recall::RecallAsk`]), and a model writes working
//! notes back into that case through a marker in its answer ([`marker`]), unconfirmed. A lesson
//! the person kept on a card crosses clients, as text and coarse facts only ([`lessons`]).

pub mod lessons;
pub mod marker;
pub mod recall;
pub mod record;
pub mod store;
pub mod text;

pub use lessons::{KeptLesson, LessonDraft, LessonId, LessonOp, LessonProposal, Lessons, names_in};
pub use marker::{CASE_ASK, Card, CaseMarker, MODEL_WRITES, ModelWrites, Route};
pub use recall::{Alone, BalanceGap, ItemId, MAX_RUNGS, Others, Place, RecallAsk, RecallSent, RecalledItem, Rung};
pub use record::{
    Applied, Applies, Balance, BalanceDraft, Body, Case, CaseError, Click, ContractKind, CounterpartyKind, Day, Factor, FactorKind, Facts, Kind,
    Lang, Law, ModelWrite, Note, NoteDraft, NoteId, OurSide, Paper, PersonOp, Plan, Quoted, Side, SizeBand, Stage, Stance, Standing, Topic, Topics,
};
pub use store::{CaseHeader, CaseId, CaseRef, Listing, Shelf, Wall, WallId};

#[cfg(test)]
mod tests;
