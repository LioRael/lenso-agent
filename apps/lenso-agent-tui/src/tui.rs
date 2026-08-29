//! Interactive terminal surface for the composed Agent App.

mod blocks;
mod markdown;
mod palette;
mod shell;

use markdown::lines as markdown_lines;
use palette::Palette;

pub use shell::{TuiOptions, run};
