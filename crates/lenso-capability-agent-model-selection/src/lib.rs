#![allow(clippy::all)]

mod turn_selection;

pub use turn_selection::{
    ModelSelectionEvidence, TURN_MODEL_SELECTION_EXTENSION, TurnModelSelection,
};

include!("generated.rs");
