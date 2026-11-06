//! Built-in editing commands.

mod registration;
pub use registration::register_builtin_commands;
mod comment;
mod execute;
mod motion;
pub(crate) use motion::visual_ranges;
mod multi;
mod pairs;
mod text_object;
mod transaction;

#[cfg(test)]
mod tests;
