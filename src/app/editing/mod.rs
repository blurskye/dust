pub(in crate::app) mod clipboard;
pub(in crate::app) mod flash;
pub(in crate::app) mod replay;
pub(in crate::app) mod substitute;

pub(in crate::app) mod interaction;

pub(in crate::app) mod commands;

#[cfg(test)]
mod clipboard_tests;

#[cfg(test)]
mod editing_tests;
