//! Text registers and recorded macros.

use crate::editor::CommandInvocation;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterShape {
    Character,
    Line,
    Block,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterValue {
    pub text: String,
    pub shape: RegisterShape,
}

#[derive(Clone, Debug, Default)]
pub struct RegisterStore {
    values: HashMap<char, RegisterValue>,
    macros: HashMap<char, Vec<CommandInvocation>>,
    /// Bumped on every stored value, so the clipboard sync notices a yank cheaply.
    revision: u64,
}

impl RegisterStore {
    pub fn set(
        &mut self,
        name: char,
        mut value: RegisterValue,
        enabled: bool,
        mirror_named_to_unnamed: bool,
    ) {
        if !enabled || name == '_' {
            return;
        }
        let lower = name.to_ascii_lowercase();
        if name.is_ascii_uppercase()
            && let Some(previous) = self.values.get(&lower)
        {
            let mut text =
                if previous.shape == RegisterShape::Block && value.shape == RegisterShape::Block {
                    let mut left = previous.text.split('\n');
                    let mut right = value.text.split('\n');
                    let mut rows = Vec::new();
                    loop {
                        match (left.next(), right.next()) {
                            (None, None) => break,
                            (left, right) => rows.push(format!(
                                "{}{}",
                                left.unwrap_or_default(),
                                right.unwrap_or_default()
                            )),
                        }
                    }
                    rows.join("\n")
                } else {
                    let mut text = previous.text.clone();
                    if previous.shape == RegisterShape::Line && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&value.text);
                    text
                };
            if previous.shape == RegisterShape::Line || value.shape == RegisterShape::Line {
                value.shape = RegisterShape::Line;
                if !text.ends_with('\n') {
                    text.push('\n');
                }
            } else if value.shape != previous.shape {
                value.shape = RegisterShape::Character;
            }
            value.text = text;
        }
        self.revision = self.revision.wrapping_add(1);
        self.values.insert(lower, value.clone());
        if mirror_named_to_unnamed && name != '"' {
            self.values.insert('"', value);
        }
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn get(&self, name: char) -> Option<&RegisterValue> {
        self.values.get(&name.to_ascii_lowercase())
    }

    pub fn set_macro(&mut self, name: char, invocations: Vec<CommandInvocation>) {
        if name.is_ascii_uppercase() {
            self.macros
                .entry(name.to_ascii_lowercase())
                .or_default()
                .extend(invocations);
        } else {
            self.macros.insert(name, invocations);
        }
    }

    #[must_use]
    pub fn get_macro(&self, name: char) -> Option<&[CommandInvocation]> {
        self.macros
            .get(&name.to_ascii_lowercase())
            .map(Vec::as_slice)
    }
}
