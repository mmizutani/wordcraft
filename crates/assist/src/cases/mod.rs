//! Case memory for the assistant (#107): typed records of past contract cases, the ranker that
//! recalls their notes for a similar one, and the shelf that keeps them on disk. Similarity is a
//! function of closed types ([`record`]) over English and Japanese text ([`text`]), the power
//! balance then against now is a table ([`recall`]), and every change to a case on disk is a typed
//! operation under a lock ([`store`]). No window or model uses it yet.

pub mod recall;
pub mod record;
pub mod store;
pub mod text;

pub use record::{
    Applied, Applies, Balance, BalanceDraft, Body, Case, CaseError, Click, ContractKind, CounterpartyKind, Day, Factor, FactorKind, Facts, Kind,
    Lang, Law, ModelWrite, Note, NoteDraft, NoteId, OurSide, Paper, PersonOp, Plan, Quoted, Side, SizeBand, Stage, Stance, Standing, Topic, Topics,
};
pub use store::{CaseHeader, CaseId, CaseRef, Listing, Shelf, Wall, WallId};

#[cfg(test)]
mod tests;
