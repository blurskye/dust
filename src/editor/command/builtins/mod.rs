mod registration;
pub use registration::register_builtin_commands;
mod comment;
mod execute;
mod motion;
mod multi;
mod pairs;
mod text_object;
mod transaction;

#[cfg(test)]
mod tests;
