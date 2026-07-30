//! Input mapping: raw crossterm events -> high level Action.
//! Stateless pure functions. Handler receives &AppSnapshot for focus/overlay/layout decisions.
//! No blocking, no fs, no parser work.

pub mod crossterm_adapter;
pub mod handler;
pub mod keyboard;
pub mod mouse;

pub use crossterm_adapter::{is_alt_tool, is_command_k, is_f6, is_shift_f6};
pub use handler::handle_event;
