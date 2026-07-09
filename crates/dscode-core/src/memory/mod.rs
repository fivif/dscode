//! # Memory — Three-Tier Memory System (Scribe)
//!
//! ```text
//! Raw Messages  ──►  Facts (triples)  ──►  Patterns
//! (exact)            (structured)          (generalized)
//! ```

pub mod fact;
pub mod fts;
pub mod pattern;
pub mod raw;
pub mod scribe;
pub mod store;

pub use fact::Fact;
pub use pattern::Pattern;
pub use raw::RawMessage;
pub use scribe::Scribe;
pub use store::MemoryStore;
