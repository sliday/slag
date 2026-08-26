//! render — presentation helpers shared by the dashboard and stream mode.
//!
//! Nothing here touches a terminal. Each module turns forge data into a
//! renderable shape (spans, kinds, counts) and lets the caller map that
//! onto ratatui styles or ANSI escapes, so the interesting logic stays
//! testable as plain data.

pub mod diff;
pub mod trace;
