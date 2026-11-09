//! Case memory for the assistant (#107): typed records of past contract cases, and the ranker that
//! recalls their notes for a similar one. This is the pure core: no files, no window, no model.
//! Similarity is a function of closed types ([`record`]) over English and Japanese text
//! ([`text`]), and the power balance then against now is a table ([`recall`]).

pub mod recall;
pub mod record;
pub mod text;

pub use record::{
    Applies, Balance, Body, Case, ContractKind, CounterpartyKind, Day, Facts, Kind, Lang, Law, Note, OurSide, Paper, Quoted, Side, SizeBand, Stance,
    Standing, Topic, Topics,
};

#[cfg(test)]
mod tests;
