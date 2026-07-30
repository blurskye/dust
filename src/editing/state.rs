use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use crate::document::{BufferRevision, ByteOffset, TextBuffer};
use slotmap::SlotMap;

use crate::editing::{BufferId, CommandInvocation, PaneId, TabId, UndoTree, ViewId};

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum EditorMode {
    #[default]
    Normal,
    Insert,
    Replace,
    VisualChar,
    VisualLine,
    VisualBlock,
    OperatorPending,
    CommandLine,
    Prompt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionKind {
    Character,
    Line,
    Block,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    pub anchor: ByteOffset,
    pub head: ByteOffset,
    pub kind: SelectionKind,
}

#[derive(Clone, Debug)]
pub struct Buffer {
    pub text: TextBuffer,
    pub path: Option<PathBuf>,
    pub read_only: bool,
    pub dirty: bool,
    pub saved_revision: BufferRevision,
    pub(crate) saved_text: String,
    persisted_bytes: Vec<u8>,
    pub undo: UndoTree,
}

impl Buffer {
    #[must_use]
    pub fn new(text: TextBuffer, path: Option<PathBuf>) -> Self {
        let saved_text = text.to_string();
        let saved_revision = text.revision();
        let persisted_bytes = text.to_bytes();
        Self {
            undo: UndoTree::new(saved_text.clone()),
            text,
            path,
            read_only: false,
            dirty: false,
            saved_revision,
            saved_text,
            persisted_bytes,
        }
    }

    pub fn refresh_dirty(&mut self) {
        self.dirty = self.text.to_string() != self.saved_text;
    }

    pub fn mark_saved(&mut self, revision: BufferRevision) {
        if revision == self.text.revision() {
            self.mark_saved_as(revision, self.text.to_bytes(), self.path.clone());
        }
    }

    pub fn mark_saved_as(
        &mut self,
        revision: BufferRevision,
        bytes: Vec<u8>,
        path: Option<PathBuf>,
    ) {
        self.path = path;
        self.persisted_bytes = bytes.clone();
        self.saved_revision = revision;
        if let Ok(saved) = TextBuffer::from_bytes(&bytes) {
            self.saved_text = saved.to_string();
        }
        self.refresh_dirty();
    }

    #[must_use]
    pub fn persisted_bytes(&self) -> &[u8] {
        &self.persisted_bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplaceRecord {
    pub start: ByteOffset,
    pub original: String,
    pub inserted_len: usize,
}

#[derive(Clone, Debug)]
pub struct View {
    pub buffer: BufferId,
    pub cursor: ByteOffset,
    pub selection: Option<Selection>,
    pub scroll_line: usize,
    pub preferred_column: Option<usize>,
    pub mode: EditorMode,
    pub replace_history: Vec<ReplaceRecord>,
    pub block_insert: Vec<ByteOffset>,
}

impl View {
    #[must_use]
    pub fn new(buffer: BufferId) -> Self {
        Self {
            buffer,
            cursor: ByteOffset(0),
            selection: None,
            scroll_line: 0,
            preferred_column: None,
            mode: EditorMode::Normal,
            replace_history: Vec::new(),
            block_insert: Vec::new(),
        }
    }
}

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
}

impl RegisterStore {
    pub fn set(
        &mut self,
        name: char,
        value: RegisterValue,
        enabled: bool,
        mirror_named_to_unnamed: bool,
    ) {
        if !enabled || name == '_' {
            return;
        }
        self.values.insert(name, value.clone());
        if mirror_named_to_unnamed && name != '"' {
            self.values.insert('"', value);
        }
    }

    #[must_use]
    pub fn get(&self, name: char) -> Option<&RegisterValue> {
        self.values.get(&name)
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayoutNode {
    Pane(PaneId),
    Split {
        axis: Axis,
        ratio: u16,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pane {
    pub view: ViewId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tab {
    pub root: LayoutNode,
    pub focused: PaneId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditingPolicy {
    pub tab_width: usize,
    pub indent_width: usize,
    pub expand_tabs: bool,
    pub undo_break_on_text_commit: bool,
    pub registers_enabled: bool,
    pub mirror_named_to_unnamed: bool,
    pub ambiguous_width_wide: bool,
}

impl Default for EditingPolicy {
    fn default() -> Self {
        Self {
            tab_width: 4,
            indent_width: 4,
            expand_tabs: true,
            undo_break_on_text_commit: false,
            registers_enabled: true,
            mirror_named_to_unnamed: true,
            ambiguous_width_wide: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct EditorState {
    pub workspace_root: PathBuf,
    pub buffers: SlotMap<BufferId, Buffer>,
    pub views: SlotMap<ViewId, View>,
    pub panes: SlotMap<PaneId, Pane>,
    pub tabs: SlotMap<TabId, Tab>,
    pub active_tab: Option<TabId>,
    pub active_view: Option<ViewId>,
    pub registers: RegisterStore,
    pub policy: EditingPolicy,
}

impl EditorState {
    #[must_use]
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            buffers: SlotMap::with_key(),
            views: SlotMap::with_key(),
            panes: SlotMap::with_key(),
            tabs: SlotMap::with_key(),
            active_tab: None,
            active_view: None,
            registers: RegisterStore::default(),
            policy: EditingPolicy::default(),
        }
    }

    pub fn load_hidden_text(&mut self, text: TextBuffer, path: PathBuf) -> BufferId {
        if let Some((buffer, _)) = self
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
        {
            return buffer;
        }
        self.buffers.insert(Buffer::new(text, Some(path)))
    }

    pub fn open_text(&mut self, text: TextBuffer, path: Option<PathBuf>) -> (BufferId, ViewId) {
        if let Some(path) = path.as_ref() {
            if let Some((buffer, _)) = self
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(path))
            {
                if let Some((view, _)) = self.views.iter().find(|(_, view)| view.buffer == buffer) {
                    self.show_view(view);
                    return (buffer, view);
                }
                let view = self.views.insert(View::new(buffer));
                self.show_view(view);
                return (buffer, view);
            }
        }
        let buffer = self.buffers.insert(Buffer::new(text, path));
        let view = self.views.insert(View::new(buffer));
        self.show_view(view);
        (buffer, view)
    }

    pub fn show_buffer(&mut self, buffer: BufferId) -> Result<ViewId, &'static str> {
        if !self.buffers.contains_key(buffer) {
            return Err("buffer is stale");
        }
        let existing = self
            .views
            .iter()
            .find(|(_, view)| view.buffer == buffer)
            .map(|(view, _)| view);
        let view = existing.unwrap_or_else(|| self.views.insert(View::new(buffer)));
        self.show_view(view);
        Ok(view)
    }

    pub fn cycle_buffer(&mut self, offset: isize, wrap: bool) -> Result<ViewId, &'static str> {
        let buffers = self.buffers.keys().collect::<Vec<_>>();
        if buffers.is_empty() {
            return Err("no buffers are open");
        }
        let current = self.active_buffer_id().unwrap_or(buffers[0]);
        let index = buffers
            .iter()
            .position(|buffer| *buffer == current)
            .unwrap_or(0);
        let target = buffers[offset_index(index, buffers.len(), offset, wrap)];
        self.show_buffer(target)
    }

    fn show_view(&mut self, view: ViewId) {
        if let Some((tab_id, pane_id)) = self.tabs.iter().find_map(|(tab_id, tab)| {
            pane_ids(&tab.root).into_iter().find_map(|pane_id| {
                self.panes
                    .get(pane_id)
                    .is_some_and(|pane| pane.view == view)
                    .then_some((tab_id, pane_id))
            })
        }) {
            self.active_tab = Some(tab_id);
            if let Some(tab) = self.tabs.get_mut(tab_id) {
                tab.focused = pane_id;
            }
            self.active_view = Some(view);
            return;
        }
        if let Some(tab_id) = self.active_tab
            && let Some(tab) = self.tabs.get(tab_id)
            && let Some(pane) = self.panes.get_mut(tab.focused)
        {
            let previous = pane.view;
            pane.view = view;
            self.active_view = Some(view);
            if previous != view
                && !self
                    .panes
                    .iter()
                    .any(|(_, candidate)| candidate.view == previous)
            {
                self.views.remove(previous);
            }
            return;
        }
        let pane = self.panes.insert(Pane { view });
        let tab = self.tabs.insert(Tab {
            root: LayoutNode::Pane(pane),
            focused: pane,
        });
        self.active_tab = Some(tab);
        self.active_view = Some(view);
    }

    pub fn split_active(&mut self, axis: Axis, ratio: u16) -> Result<ViewId, &'static str> {
        if !(100..=900).contains(&ratio) {
            return Err("split ratio must be between 100 and 900");
        }
        let tab_id = self.active_tab.ok_or("no active tab")?;
        let focused = self.tabs.get(tab_id).ok_or("active tab is stale")?.focused;
        let source_view = self.panes.get(focused).ok_or("focused pane is stale")?.view;
        let cloned = self
            .views
            .get(source_view)
            .ok_or("active view is stale")?
            .clone();
        let view = self.views.insert(cloned);
        let pane = self.panes.insert(Pane { view });
        let tab = self.tabs.get_mut(tab_id).ok_or("active tab is stale")?;
        if !split_leaf(&mut tab.root, focused, axis, ratio, pane) {
            self.panes.remove(pane);
            self.views.remove(view);
            return Err("focused pane is absent from layout");
        }
        tab.focused = pane;
        self.active_view = Some(view);
        Ok(view)
    }

    pub fn new_tab(&mut self) -> Result<TabId, &'static str> {
        let source = self.active_view.ok_or("no active view")?;
        let view = self.views.insert(
            self.views
                .get(source)
                .ok_or("active view is stale")?
                .clone(),
        );
        let pane = self.panes.insert(Pane { view });
        let tab = self.tabs.insert(Tab {
            root: LayoutNode::Pane(pane),
            focused: pane,
        });
        self.active_tab = Some(tab);
        self.active_view = Some(view);
        Ok(tab)
    }

    #[must_use]
    pub fn active_tab_views(&self) -> Vec<ViewId> {
        self.active_tab
            .and_then(|tab| self.tabs.get(tab))
            .map_or_else(Vec::new, |tab| {
                pane_ids(&tab.root)
                    .into_iter()
                    .filter_map(|pane| self.panes.get(pane).map(|pane| pane.view))
                    .collect()
            })
    }

    pub fn focus_pane_id(&mut self, pane: PaneId) -> Result<(), &'static str> {
        let tab = self.active_tab.ok_or("no active tab")?;
        if !pane_ids(&self.tabs.get(tab).ok_or("active tab is stale")?.root).contains(&pane) {
            return Err("pane is absent from active tab");
        }
        self.set_focused_pane(tab, pane)
    }

    pub fn focus_pane(&mut self, offset: isize, wrap: bool) -> Result<(), &'static str> {
        let tab_id = self.active_tab.ok_or("no active tab")?;
        let tab = self.tabs.get(tab_id).ok_or("active tab is stale")?;
        let panes = pane_ids(&tab.root);
        let current = panes
            .iter()
            .position(|pane| *pane == tab.focused)
            .ok_or("focused pane is absent from layout")?;
        let target = offset_index(current, panes.len(), offset, wrap);
        self.set_focused_pane(tab_id, panes[target])
    }

    pub fn focus_tab(&mut self, offset: isize, wrap: bool) -> Result<(), &'static str> {
        let tabs = self.tabs.keys().collect::<Vec<_>>();
        let active = self.active_tab.ok_or("no active tab")?;
        let current = tabs
            .iter()
            .position(|tab| *tab == active)
            .ok_or("active tab is stale")?;
        let target = tabs[offset_index(current, tabs.len(), offset, wrap)];
        let pane = self.tabs.get(target).ok_or("target tab is stale")?.focused;
        self.active_tab = Some(target);
        self.set_focused_pane(target, pane)
    }

    pub fn resize_active_pane(&mut self, delta: i16) -> Result<(), &'static str> {
        let tab_id = self.active_tab.ok_or("no active tab")?;
        let tab = self.tabs.get_mut(tab_id).ok_or("active tab is stale")?;
        if resize_parent(&mut tab.root, tab.focused, delta) {
            Ok(())
        } else {
            Err("active pane has no resizable split")
        }
    }

    pub fn close_active_view(&mut self) -> Result<ViewId, &'static str> {
        let view = self.active_view.ok_or("no active view")?;
        let tab_id = self.active_tab.ok_or("no active tab")?;
        let pane_count = pane_ids(&self.tabs.get(tab_id).ok_or("active tab is stale")?.root).len();
        if pane_count > 1 {
            self.close_active_pane()?;
        } else if self.tabs.len() > 1 {
            self.close_active_tab()?;
        } else {
            let tab = self.tabs.remove(tab_id).ok_or("active tab is stale")?;
            for pane_id in pane_ids(&tab.root) {
                self.panes.remove(pane_id);
            }
            self.views.remove(view);
            self.active_tab = None;
            self.active_view = None;
        }
        Ok(view)
    }

    pub fn close_active_pane(&mut self) -> Result<(), &'static str> {
        let tab_id = self.active_tab.ok_or("no active tab")?;
        let focused = self.tabs.get(tab_id).ok_or("active tab is stale")?.focused;
        let pane_count = pane_ids(&self.tabs.get(tab_id).ok_or("active tab is stale")?.root).len();
        if pane_count == 1 {
            return self.close_active_tab();
        }
        let tab = self.tabs.get_mut(tab_id).ok_or("active tab is stale")?;
        let new_root = remove_leaf(tab.root.clone(), focused).ok_or("cannot close pane")?;
        tab.root = new_root;
        let next = pane_ids(&tab.root)[0];
        tab.focused = next;
        if let Some(pane) = self.panes.remove(focused) {
            self.views.remove(pane.view);
        }
        self.set_focused_pane(tab_id, next)
    }

    pub fn close_active_tab(&mut self) -> Result<(), &'static str> {
        if self.tabs.len() <= 1 {
            return Err("cannot close the final tab");
        }
        let active = self.active_tab.ok_or("no active tab")?;
        let tab = self.tabs.remove(active).ok_or("active tab is stale")?;
        for pane_id in pane_ids(&tab.root) {
            if let Some(pane) = self.panes.remove(pane_id) {
                self.views.remove(pane.view);
            }
        }
        let next = self.tabs.keys().next().ok_or("no remaining tab")?;
        let pane = self.tabs.get(next).ok_or("remaining tab is stale")?.focused;
        self.active_tab = Some(next);
        self.set_focused_pane(next, pane)
    }

    fn set_focused_pane(&mut self, tab: TabId, pane: PaneId) -> Result<(), &'static str> {
        let view = self.panes.get(pane).ok_or("target pane is stale")?.view;
        self.tabs.get_mut(tab).ok_or("target tab is stale")?.focused = pane;
        self.active_view = Some(view);
        Ok(())
    }

    pub fn replace_buffer_from_disk(
        &mut self,
        buffer: BufferId,
        text: TextBuffer,
        path: PathBuf,
    ) -> bool {
        let Some(target) = self.buffers.get_mut(buffer) else {
            return false;
        };
        let read_only = target.read_only;
        *target = Buffer::new(text, Some(path));
        target.read_only = read_only;
        let maximum = ByteOffset(target.text.len_bytes());
        for (_, view) in self
            .views
            .iter_mut()
            .filter(|(_, view)| view.buffer == buffer)
        {
            view.cursor = view.cursor.min(maximum);
            while !target.text.is_char_boundary(view.cursor) {
                view.cursor = ByteOffset(view.cursor.get().saturating_sub(1));
            }
            view.selection = None;
            view.mode = EditorMode::Normal;
            view.scroll_line = view
                .scroll_line
                .min(target.text.len_lines().saturating_sub(1));
        }
        true
    }

    pub fn validate_layout(&self) -> Result<(), &'static str> {
        if self.tabs.is_empty() {
            return if self.panes.is_empty()
                && self.views.is_empty()
                && self.active_tab.is_none()
                && self.active_view.is_none()
            {
                Ok(())
            } else {
                Err("empty layout retains pane/view state")
            };
        }
        let active_tab = self.active_tab.ok_or("tabs exist without an active tab")?;
        let active = self.tabs.get(active_tab).ok_or("active tab is stale")?;
        let mut referenced_panes = HashSet::new();
        let mut referenced_views = HashSet::new();
        for (_, tab) in &self.tabs {
            let leaves = pane_ids(&tab.root);
            if !leaves.contains(&tab.focused) {
                return Err("focused pane is absent from tab layout");
            }
            for pane_id in leaves {
                if !referenced_panes.insert(pane_id) {
                    return Err("pane appears more than once in layouts");
                }
                let pane = self.panes.get(pane_id).ok_or("layout pane is stale")?;
                if !referenced_views.insert(pane.view) {
                    return Err("view appears in more than one pane");
                }
                if !self.views.contains_key(pane.view) {
                    return Err("pane view is stale");
                }
            }
        }
        if referenced_panes.len() != self.panes.len() {
            return Err("orphan pane exists");
        }
        if referenced_views.len() != self.views.len() {
            return Err("orphan view exists");
        }
        let focused_view = self
            .panes
            .get(active.focused)
            .ok_or("active focused pane is stale")?
            .view;
        if self.active_view != Some(focused_view) {
            return Err("active view disagrees with active tab focus");
        }
        Ok(())
    }

    #[must_use]
    pub fn active_buffer_id(&self) -> Option<BufferId> {
        self.active_view
            .and_then(|view| self.views.get(view))
            .map(|view| view.buffer)
    }
}

fn pane_ids(node: &LayoutNode) -> Vec<PaneId> {
    match node {
        LayoutNode::Pane(pane) => vec![*pane],
        LayoutNode::Split { first, second, .. } => {
            let mut panes = pane_ids(first);
            panes.extend(pane_ids(second));
            panes
        }
    }
}

fn split_leaf(
    node: &mut LayoutNode,
    target: PaneId,
    axis: Axis,
    ratio: u16,
    new_pane: PaneId,
) -> bool {
    match node {
        LayoutNode::Pane(pane) if *pane == target => {
            *node = LayoutNode::Split {
                axis,
                ratio,
                first: Box::new(LayoutNode::Pane(target)),
                second: Box::new(LayoutNode::Pane(new_pane)),
            };
            true
        }
        LayoutNode::Pane(_) => false,
        LayoutNode::Split { first, second, .. } => {
            split_leaf(first, target, axis, ratio, new_pane)
                || split_leaf(second, target, axis, ratio, new_pane)
        }
    }
}

fn contains_pane(node: &LayoutNode, target: PaneId) -> bool {
    match node {
        LayoutNode::Pane(pane) => *pane == target,
        LayoutNode::Split { first, second, .. } => {
            contains_pane(first, target) || contains_pane(second, target)
        }
    }
}

fn resize_parent(node: &mut LayoutNode, target: PaneId, delta: i16) -> bool {
    let LayoutNode::Split {
        ratio,
        first,
        second,
        ..
    } = node
    else {
        return false;
    };
    if resize_parent(first, target, delta) || resize_parent(second, target, delta) {
        return true;
    }
    let signed = if contains_pane(first, target) {
        delta
    } else if contains_pane(second, target) {
        -delta
    } else {
        return false;
    };
    let adjusted = i32::from(*ratio) + i32::from(signed);
    *ratio = u16::try_from(adjusted.clamp(100, 900)).unwrap_or(*ratio);
    true
}

fn remove_leaf(node: LayoutNode, target: PaneId) -> Option<LayoutNode> {
    match node {
        LayoutNode::Pane(pane) => (pane != target).then_some(LayoutNode::Pane(pane)),
        LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match (remove_leaf(*first, target), remove_leaf(*second, target)) {
            (Some(first), Some(second)) => Some(LayoutNode::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (Some(node), None) | (None, Some(node)) => Some(node),
            (None, None) => None,
        },
    }
}

fn offset_index(current: usize, len: usize, offset: isize, wrap: bool) -> usize {
    let len = isize::try_from(len).unwrap_or(isize::MAX).max(1);
    let candidate = isize::try_from(current).unwrap_or(0).saturating_add(offset);
    let target = if wrap {
        candidate.rem_euclid(len)
    } else {
        candidate.clamp(0, len.saturating_sub(1))
    };
    usize::try_from(target).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::document::{ByteOffset, ByteRange, TextBuffer, TextEdit, TextEditSet};

    use super::{EditorState, RegisterStore};
    use crate::editing::{CommandInvocation, InvocationSource};

    #[test]
    fn cursor_belongs_to_view_not_shared_buffer() {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        let (buffer, first) = state.open_text(TextBuffer::from_text("shared"), None);
        let second = state.views.insert(super::View::new(buffer));
        state.views[first].cursor = ByteOffset(1);
        state.views[second].cursor = ByteOffset(5);
        assert_eq!(state.buffers[buffer].text.to_string(), "shared");
        assert_ne!(state.views[first].cursor, state.views[second].cursor);
    }

    #[test]
    fn register_storage_disable_black_hole_and_mirroring_are_explicit() {
        let mut registers = RegisterStore::default();
        let old = super::RegisterValue {
            text: "old".into(),
            shape: super::RegisterShape::Character,
        };
        registers.set('"', old.clone(), true, true);
        registers.set(
            'a',
            super::RegisterValue {
                text: "named".into(),
                shape: super::RegisterShape::Character,
            },
            true,
            false,
        );
        assert_eq!(registers.get('"'), Some(&old));
        assert_eq!(
            registers.get('a').map(|value| value.text.as_str()),
            Some("named")
        );
        registers.set('_', old.clone(), true, true);
        registers.set('b', old, false, true);
        assert!(registers.get('_').is_none());
        assert!(registers.get('b').is_none());
    }

    #[test]
    fn macro_registers_are_typed_and_uppercase_appends() {
        let mut registers = RegisterStore::default();
        let first =
            CommandInvocation::new("edit.delete_char", InvocationSource::Test).expect("invocation");
        let second =
            CommandInvocation::new("edit.toggle_case", InvocationSource::Test).expect("invocation");
        registers.set_macro('a', vec![first]);
        registers.set_macro('A', vec![second]);
        assert_eq!(registers.get_macro('a').expect("macro").len(), 2);
        assert_eq!(registers.get_macro('A').expect("macro").len(), 2);
    }

    #[test]
    fn canonical_path_identity_reuses_buffer_and_view() {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        let path = PathBuf::from("/workspace/file.rs");
        let first = state.open_text(TextBuffer::from_text("first"), Some(path.clone()));
        let second = state.open_text(TextBuffer::from_text("ignored"), Some(path));
        assert_eq!(first, second);
        assert_eq!(state.buffers.len(), 1);
        assert_eq!(state.views.len(), 1);
        assert_eq!(state.buffers[first.0].text.to_string(), "first");
    }

    #[test]
    fn replacing_a_pane_keeps_hidden_buffers_without_orphan_views() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (first_buffer, first_view) = state.open_text(TextBuffer::from_text("one"), None);
        state.buffers[first_buffer].dirty = true;
        let (_, second_view) = state.open_text(TextBuffer::from_text("two"), None);
        assert!(!state.views.contains_key(first_view));
        assert!(state.buffers.contains_key(first_buffer));
        assert_eq!(state.views.len(), 1);
        assert_eq!(state.active_view, Some(second_view));
        state
            .cycle_buffer(-1, true)
            .expect("reopen hidden dirty buffer");
        assert_eq!(state.active_buffer_id(), Some(first_buffer));
        assert!(state.buffers[first_buffer].dirty);
        state.validate_layout().expect("valid reopened layout");
    }

    #[test]
    fn split_and_tab_layouts_keep_independent_views_and_focus() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (buffer, first) = state.open_text(TextBuffer::from_text("abc"), None);
        let second = state
            .split_active(super::Axis::Horizontal, 500)
            .expect("split");
        assert_eq!(state.views[first].buffer, buffer);
        assert_eq!(state.views[second].buffer, buffer);
        state.views[second].cursor = crate::document::ByteOffset(2);
        state.resize_active_pane(50).expect("grow second pane");
        assert!(matches!(
            state.tabs[state.active_tab.expect("tab")].root,
            super::LayoutNode::Split { ratio: 450, .. }
        ));
        state.focus_pane(-1, true).expect("first pane");
        assert_eq!(state.active_view, Some(first));
        assert_eq!(state.views[first].cursor, crate::document::ByteOffset(0));
        state.focus_pane(-1, false).expect("clamped focus");
        assert_eq!(state.active_view, Some(first));
        let first_tab = state.active_tab.expect("tab");
        let second_tab = state.new_tab().expect("new tab");
        assert_ne!(first_tab, second_tab);
        state.focus_tab(-1, true).expect("previous tab");
        assert_eq!(state.active_tab, Some(first_tab));
        state.close_active_pane().expect("close split");
        assert_eq!(
            state.tabs[first_tab].root,
            super::LayoutNode::Pane(state.tabs[first_tab].focused)
        );
    }

    #[test]
    fn layout_model_sequences_preserve_all_id_and_focus_invariants() {
        let mut state = EditorState::new(PathBuf::from("/model"));
        state.open_text(TextBuffer::from_text("seed"), None);
        for step in 0..120 {
            match step % 8 {
                0 if state.panes.len() < 8 => {
                    let _ = state.split_active(super::Axis::Horizontal, 500);
                }
                1 if state.tabs.len() < 4 => {
                    let _ = state.new_tab();
                }
                2 => {
                    let _ = state.focus_pane(1, true);
                }
                3 => {
                    let _ = state.focus_tab(-1, true);
                }
                4 => {
                    let _ = state.resize_active_pane(50);
                }
                5 => {
                    state.open_text(TextBuffer::from_text(&format!("buffer-{step}")), None);
                }
                6 if state.panes.len() > state.tabs.len() => {
                    let _ = state.close_active_pane();
                }
                7 if state.tabs.len() > 1 => {
                    let _ = state.close_active_tab();
                }
                _ => {}
            }
            state.validate_layout().expect("layout invariant");
        }
        let expected = state
            .buffers
            .keys()
            .collect::<std::collections::BTreeSet<_>>();
        let mut reachable = std::collections::BTreeSet::new();
        for _ in 0..expected.len() {
            state.cycle_buffer(1, true).expect("cycle hidden buffer");
            reachable.insert(state.active_buffer_id().expect("active buffer"));
            state.validate_layout().expect("cycled layout invariant");
        }
        assert_eq!(reachable, expected, "every hidden buffer remains reachable");
    }

    #[test]
    fn save_completion_for_old_revision_does_not_clear_newer_dirty_edit() {
        let mut buffer = super::Buffer::new(TextBuffer::from_text("a"), None);
        let first = TextEditSet::new(
            buffer.text.revision(),
            vec![TextEdit::new(ByteRange::empty(ByteOffset(1)), "b")],
        );
        buffer.text.apply_edits(&first).expect("first edit");
        buffer.refresh_dirty();
        let persisted = buffer.text.revision();
        let second = TextEditSet::new(
            buffer.text.revision(),
            vec![TextEdit::new(ByteRange::empty(ByteOffset(2)), "c")],
        );
        buffer.text.apply_edits(&second).expect("second edit");
        buffer.refresh_dirty();
        buffer.mark_saved(persisted);
        assert!(buffer.dirty);
        assert_eq!(buffer.text.to_string(), "abc");
    }
}
