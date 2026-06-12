//! MAGI three-brain auto-spiral scheduler.
//!
//! The MAGI system uses three distinct "brains" wired in a spiral loop:
//!
//! 1. **Casper** — scrutinizes the PRD and previous round results, identifying gaps
//!    and generating a focused review for the next execution round.
//! 2. **Balthasar** — wraps the tool-equipped agent loop to execute tasks based on
//!    Casper's scrutiny and the original PRD.
//! 3. **Melchior** — evaluates execution quality on a 0-100 scale and decides
//!    whether the spiral should continue or stop.
//!
//! The [`MagiScheduler`] orchestrates these brains in rounds until Melchior
//! signals completion or the round budget is exhausted.

pub mod scheduler;
pub mod scrutinize;
pub mod execute;
pub mod promote;

pub use scheduler::{MagiError, MagiRound, MagiScheduler, Promotion};
pub use scrutinize::scrutinize;
pub use execute::execute_subtask;
pub use promote::promote;
