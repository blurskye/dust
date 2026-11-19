//! Named actions the forge screens share and the keys that run them. Actions and the pull
//! request and issue readers read one table, so rebinding an entry rebinds every screen.
//!
//! A chord is a key, or the leader and then a key. The leader is the editor's
//! (`input.keymaps.leader`, Space by default). The key after it resolves the chord at once: an
//! unknown key or Esc cancels it, with no other effect. The `lazygit` preset makes Space the
//! toggle key and `,` the leader.
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::screen) enum Named {
    /// Hide or show the left column (Actions' workflows and runs, the readers' lists).
    ToggleSidebar,
    /// Open or close the thing under the cursor (the Git tab stages with it). Enter does it
    /// in the default keys; the `lazygit` preset puts it on Space.
    Toggle,
}

impl Named {
    /// The footer's word for it.
    pub(in crate::screen) fn label(self) -> &'static str {
        match self {
            Self::ToggleSidebar => "sidebar",
            Self::Toggle => "toggle",
        }
    }
}

/// Which keys the screens use: Dusty's own, or lazygit's habits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::screen) enum Preset {
    #[default]
    Dusty,
    /// `keys = "lazygit"`: lazygit's keys on the Git tab, `,` is the leader.
    Lazygit,
}

impl Preset {
    pub(in crate::screen) fn from_name(name: &str) -> Self {
        if name.eq_ignore_ascii_case("lazygit") {
            Self::Lazygit
        } else {
            Self::Dusty
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::screen) struct Chord {
    /// The leader comes first.
    pub leader: bool,
    /// A plain character key (KeyCode is not `Copy`).
    pub key: char,
}

const fn after_leader(c: char) -> Option<Chord> {
    Some(Chord {
        leader: true,
        key: c,
    })
}

const fn single(key: char) -> Option<Chord> {
    Some(Chord { leader: false, key })
}

/// Every shared binding: (action, Dusty's chord, lazygit's chord). `None` leaves the action to
/// the screen's own keys (Enter opens things in Dusty's keys).
pub(in crate::screen) const TABLE: &[(Named, Option<Chord>, Option<Chord>)] = &[
    (Named::ToggleSidebar, after_leader('e'), after_leader('e')),
    (Named::Toggle, None, single(' ')),
];

/// The keys in force: the preset and the leader it reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::screen) struct Keys {
    pub preset: Preset,
    /// The editor's leader (`input.keymaps.leader`); the `lazygit` preset uses `,` instead.
    pub leader: char,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            preset: Preset::Dusty,
            leader: ' ',
        }
    }
}

impl Keys {
    /// `leader` as the editor's config spells it: `space`, `<space>`, `,` or one character.
    pub(in crate::screen) fn new(leader: &str, preset: Preset) -> Self {
        let word = leader.trim().trim_start_matches('<').trim_end_matches('>');
        let mut chars = word.chars();
        let leader = match (chars.next(), chars.next()) {
            _ if word.eq_ignore_ascii_case("space") => ' ',
            (Some(c), None) if !c.is_control() => c,
            _ => ' ',
        };
        Self { preset, leader }
    }

    pub(in crate::screen) fn leader(self) -> char {
        match self.preset {
            Preset::Lazygit => ',',
            Preset::Dusty => self.leader,
        }
    }

    fn entries(self) -> impl Iterator<Item = (Named, Chord)> {
        TABLE.iter().filter_map(move |(named, dusty, lazygit)| {
            match self.preset {
                Preset::Dusty => *dusty,
                Preset::Lazygit => *lazygit,
            }
            .map(|chord| (*named, chord))
        })
    }

    pub(in crate::screen) fn chord(self, named: Named) -> Option<Chord> {
        self.entries()
            .find(|(entry, _)| *entry == named)
            .map(|(_, chord)| chord)
    }

    /// As the footer shows it: `Space e`, `, e`.
    pub(in crate::screen) fn keys(self, named: Named) -> Option<String> {
        let chord = self.chord(named)?;
        let key = name(chord.key);
        Some(if chord.leader {
            format!("{} {key}", name(self.leader()))
        } else {
            key
        })
    }

    /// The chords the pending leader can finish, for the footer: `(e, sidebar)`.
    pub(in crate::screen) fn after_leader(self) -> Vec<(String, &'static str)> {
        self.entries()
            .filter(|(_, chord)| chord.leader)
            .map(|(named, chord)| (name(chord.key), named.label()))
            .collect()
    }

    /// The leader's own word: `Space`.
    pub(in crate::screen) fn leader_name(self) -> String {
        name(self.leader())
    }
}

fn name(key: char) -> String {
    match key {
        ' ' => "Space".to_owned(),
        c => c.to_string(),
    }
}

/// The plain character a key event carries, if any.
fn plain_char(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(c) if !key.modifiers.control && !key.modifiers.alt => Some(c),
        _ => None,
    }
}

/// What a key does to the shared table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::screen) enum Step {
    /// Not the table's: the screen handles the key as before.
    Pass,
    /// The leader: the next key finishes the chord.
    Wait,
    /// A chord completed.
    Run(Named),
    /// The key after the leader finished no chord (or was Esc): nothing happens.
    Cancel,
}

impl Workspace {
    /// Feeds one key through [`TABLE`]. Not for keys typed into an input.
    pub(in crate::screen) fn chord_key(&mut self, key: &KeyEvent) -> Step {
        let keys = self.keys;
        let typed = plain_char(key);
        if std::mem::take(&mut self.leader_pending) {
            return keys
                .entries()
                .find(|(_, chord)| chord.leader && Some(chord.key) == typed)
                .map_or(Step::Cancel, |(named, _)| Step::Run(named));
        }
        if typed == Some(keys.leader()) && keys.entries().any(|(_, chord)| chord.leader) {
            self.leader_pending = true;
            return Step::Wait;
        }
        keys.entries()
            .find(|(_, chord)| !chord.leader && Some(chord.key) == typed)
            .map_or(Step::Pass, |(named, _)| Step::Run(named))
    }

    /// The leader is waiting for the rest of a chord (the footer says which keys finish it).
    pub(in crate::screen) fn leader_pending(&self) -> bool {
        self.leader_pending
    }

    pub(in crate::screen) fn key_table(&self) -> Keys {
        self.keys
    }

    pub(in crate::screen) fn set_keys(&mut self, keys: Keys) {
        self.keys = keys;
        self.leader_pending = false;
    }

    /// `toggle_sidebar`: the left column hides or comes back, on every forge screen, and the
    /// next session opens the same way.
    pub(in crate::screen) fn toggle_sidebar(&mut self) {
        self.hide_list = !self.hide_list;
        views::set_list_hidden_preference(&self.root, self.hide_list);
    }

    pub(in crate::screen) fn sidebar_hidden(&self) -> bool {
        self.hide_list
    }
}

/// A key the Git tab's lazygit table reads or sends: a plain character, Enter, or Ctrl
/// with a letter (lazygit's `<c-j>`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(in crate::screen) enum Key {
    Char(char),
    Enter,
    Ctrl(char),
}

impl Key {
    /// The key a plain (no Alt) event carries; Ctrl only with a character.
    pub(in crate::screen) fn of(event: &KeyEvent) -> Option<Self> {
        if event.modifiers.alt {
            return None;
        }
        if event.modifiers.control {
            return match event.code {
                KeyCode::Char(c) => Some(Self::Ctrl(c.to_ascii_lowercase())),
                _ => None,
            };
        }
        match event.code {
            KeyCode::Char(c) => Some(Self::Char(c)),
            KeyCode::Enter => Some(Self::Enter),
            _ => None,
        }
    }

    pub(in crate::screen) fn event(self) -> KeyEvent {
        let mut event = KeyEvent::plain(match self {
            Self::Char(c) | Self::Ctrl(c) => KeyCode::Char(c),
            Self::Enter => KeyCode::Enter,
        });
        event.modifiers.control = matches!(self, Self::Ctrl(_));
        event
    }

    /// As the footer and `?` spell it: `Space`, `Enter`, `R`.
    pub(in crate::screen) fn label(self) -> String {
        match self {
            Self::Char(c) => name(c),
            Self::Enter => "Enter".into(),
            Self::Ctrl(c) => format!("C-{c}"),
        }
    }

    /// A footer or sheet token: `Enter`, `Space`, `C-j` or one character.
    fn parse(token: &str) -> Option<Self> {
        if let Some(rest) = token.strip_prefix("C-") {
            let mut letters = rest.chars();
            return match (letters.next(), letters.next()) {
                (Some(c), None) => Some(Self::Ctrl(c)),
                _ => None,
            };
        }
        let mut chars = token.chars();
        match (chars.next(), chars.next()) {
            _ if token == "Enter" => Some(Self::Enter),
            _ if token == "Space" => Some(Self::Char(' ')),
            (Some(c), None) => Some(Self::Char(c)),
            _ => None,
        }
    }
}

/// The Git tab's lists and its reader, as the lazygit table tells them apart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::screen) enum Panel {
    Files,
    Worktrees,
    Submodules,
    Branches,
    Remotes,
    Tags,
    Commits,
    Reflog,
    Stash,
    /// The reader on a working-tree or staged diff, where lines stage.
    Staging,
    /// A history commit's files, or its diff: only the global keys change there.
    CommitFiles,
}

#[cfg(test)]
impl Panel {
    pub(in crate::screen) const ALL: [Self; 11] = [
        Self::Files,
        Self::Worktrees,
        Self::Submodules,
        Self::Branches,
        Self::Remotes,
        Self::Tags,
        Self::Commits,
        Self::Reflog,
        Self::Stash,
        Self::Staging,
        Self::CommitFiles,
    ];
}

/// What a lazygit key does: Dusty Git's key for the same action in that panel, or an action
/// Dusty Git keeps in another panel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::screen) enum To {
    Key(Key),
    /// Save a stash (Dusty Git's `n` in Stashes); lazygit's `s` in Files.
    Stash,
    /// Pull (Dusty Git's `p` everywhere but Stashes, where it pops); lazygit's `p` there.
    Pull,
}

/// One row of the lazygit preset: the key lazygit users press, what it runs, lazygit's word
/// for it.
pub(in crate::screen) type Binding = (Key, To, &'static str);

const fn c(key: char) -> Key {
    Key::Char(key)
}

const fn to(key: char) -> To {
    To::Key(Key::Char(key))
}

const ENTER: To = To::Key(Key::Enter);

/// lazygit's global keys that differ from Dusty Git's. A panel's own row for the same key
/// wins, as in lazygit (`R` renames a branch and rewords a commit).
pub(in crate::screen) const LAZYGIT_GLOBAL: &[Binding] = &[
    (c('R'), to('r'), "refresh"),
    (Key::Ctrl('z'), to('Z'), "redo"),
];

/// lazygit's keys per panel (lazygit's `docs/keybindings/Keybindings_en.md`), for the
/// actions Dusty Git has. A key not listed keeps Dusty Git's meaning.
pub(in crate::screen) fn lazygit_panel(panel: Panel) -> &'static [Binding] {
    match panel {
        Panel::Files => FILES,
        Panel::Worktrees => WORKTREES,
        Panel::Submodules => SUBMODULES,
        Panel::Branches => BRANCHES,
        Panel::Remotes => REMOTES,
        Panel::Tags | Panel::CommitFiles => &[],
        Panel::Commits => COMMITS,
        Panel::Reflog => REFLOG,
        Panel::Stash => STASH,
        Panel::Staging => STAGING,
    }
}

// Each table is a `const` so its `const fn` rows are evaluated at compile time.
const FILES: &[Binding] = &[
    (c(' '), to('-'), "stage"),
    (Key::Enter, to('v'), "stage lines"),
    (c('e'), ENTER, "edit"),
    (c('o'), ENTER, "open file"),
    (c('`'), to('t'), "file tree view"),
    (c('v'), to('V'), "range select"),
    (c('s'), To::Stash, "stash"),
];
const WORKTREES: &[Binding] = &[(c(' '), ENTER, "switch")];
const SUBMODULES: &[Binding] = &[(c(' '), to('-'), "stage")];
const BRANCHES: &[Binding] = &[
    (c(' '), ENTER, "checkout"),
    (c('M'), to('m'), "merge"),
    (c('r'), to('R'), "rebase"),
    (c('R'), to('e'), "rename"),
];
const REMOTES: &[Binding] = &[(c(' '), ENTER, "checkout")];
const COMMITS: &[Binding] = &[
    (c('r'), to('W'), "reword"),
    (c('R'), to('W'), "reword with editor"),
    (Key::Ctrl('j'), to('J'), "move commit down one"),
    (Key::Ctrl('k'), to('K'), "move commit up one"),
];
const REFLOG: &[Binding] = &[(c(' '), to('o'), "checkout")];
const STASH: &[Binding] = &[
    (c(' '), to('a'), "apply"),
    (c('g'), to('p'), "pop"),
    (c('p'), To::Pull, "pull"),
];
const STAGING: &[Binding] = &[
    (c(' '), to('a'), "stage"),
    (c('v'), to('V'), "range select"),
    (c('e'), ENTER, "edit file"),
];
/// lazygit's merge view, on a conflicted file in the reader (`conflicts.rs`): Space picks
/// the hunk side under the cursor, `b` keeps both, `e` edits the file there.
pub(in crate::screen) const LAZYGIT_MERGING: &[Binding] = &[
    (c(' '), ENTER, "pick hunk"),
    (c('b'), to('b'), "pick all hunks"),
    (c('e'), to('e'), "edit file"),
];
/// lazygit's `m` while a merge, rebase or pick is paused: continue, skip or abort.
pub(in crate::screen) const LAZYGIT_OPERATION: &[Binding] =
    &[(c('m'), to('M'), "view merge/rebase options")];
const SUBMODULE_ALIASES: &[Binding] = &[(c('i'), to('u'), "initialize")];
const BRANCH_ALIASES: &[Binding] = &[(c('G'), to('o'), "open pull request")];
// Space toggles, which stages an unstaged file; `o` opens a conflict's ours form through
// Enter, as Dusty Git's `o` does.
const FILES_SHADOWED: &[(Key, Option<Key>)] = &[(c('s'), Some(c(' '))), (c('o'), Some(c('o')))];
// `r` rebases and rewords there, as in lazygit: refresh from another panel.
const REFRESH_SHADOWED: &[(Key, Option<Key>)] = &[(c('r'), None)];

/// Second lazygit keys for an action whose Dusty Git key lazygit uses too: they run it, and
/// the footer and `?` keep showing the shared key (`o`, not `G`).
pub(in crate::screen) fn lazygit_aliases(panel: Panel) -> &'static [Binding] {
    match panel {
        Panel::Submodules => SUBMODULE_ALIASES,
        Panel::Branches => BRANCH_ALIASES,
        _ => &[],
    }
}

/// Dusty Git keys the lazygit preset gives to something else in a panel, and the key that
/// still reaches the same action (`None`: none in that panel, as in lazygit).
pub(in crate::screen) fn lazygit_shadowed(panel: Panel) -> &'static [(Key, Option<Key>)] {
    match panel {
        Panel::Files => FILES_SHADOWED,
        Panel::Branches | Panel::Commits => REFRESH_SHADOWED,
        _ => &[],
    }
}

/// Every key Dusty Git reads in a panel (a list's keys and the ones every list shares),
/// so the tests can check the preset leaves each one reachable.
#[cfg(test)]
pub(in crate::screen) fn dusty_keys(panel: Panel) -> Vec<Key> {
    let shared = "qr:?PcCXSAWHL[]<>/0pzZ";
    let own = match panel {
        Panel::Files => "suatFidDxVvhlot-",
        Panel::Worktrees => "nd",
        Panel::Submodules => "su-",
        Panel::Branches => "nmRdeuo",
        Panel::Remotes => "f",
        Panel::Tags => "d",
        // `y` copies and `o` opens the commit on the forge, on lazygit's own keys.
        Panel::Commits => "FsfdeJKyo",
        Panel::Reflog => "now",
        Panel::Stash => "apdn",
        Panel::Staging => "sua-VdD|hlnN",
        Panel::CommitFiles => "m|yo",
    };
    shared
        .chars()
        .chain(own.chars())
        .map(Key::Char)
        .chain(std::iter::once(Key::Enter))
        .collect()
}

impl Keys {
    /// The lazygit rows in force in a panel: its own, then the global ones it does not
    /// override. Empty in Dusty Git's keys.
    pub(in crate::screen) fn bindings(self, panel: Panel) -> Vec<Binding> {
        if self.preset != Preset::Lazygit {
            return Vec::new();
        }
        let own = || lazygit_panel(panel).iter().chain(lazygit_aliases(panel));
        own()
            .copied()
            .chain(
                LAZYGIT_GLOBAL
                    .iter()
                    .copied()
                    .filter(|(key, ..)| !own().any(|(mine, ..)| mine == key)),
            )
            .collect()
    }

    /// What a pressed key runs in a panel: `None` keeps Dusty Git's meaning.
    pub(in crate::screen) fn translate(self, panel: Panel, key: Key) -> Option<To> {
        self.bindings(panel)
            .into_iter()
            .find(|(pressed, ..)| *pressed == key)
            .map(|(_, to, _)| to)
    }

    /// The key that runs Dusty Git's `key` in a panel, as the footer and `?` show it: the
    /// lazygit key bound to it, the key itself when nothing took it, or the key that still
    /// reaches it. `None` when the preset leaves the action out of that panel.
    pub(in crate::screen) fn shown(self, panel: Panel, key: Key) -> Option<Key> {
        let bindings = self.bindings(panel);
        let alias = |pressed: &Key| {
            lazygit_aliases(panel)
                .iter()
                .any(|(other, ..)| other == pressed)
        };
        if let Some((pressed, ..)) = bindings
            .iter()
            .find(|(pressed, to, _)| *to == To::Key(key) && !alias(pressed))
        {
            return Some(*pressed);
        }
        if !bindings.iter().any(|(pressed, ..)| *pressed == key) {
            return Some(key);
        }
        lazygit_shadowed(panel)
            .iter()
            .find(|(shadowed, _)| *shadowed == key)
            .and_then(|(_, cover)| *cover)
    }

    /// A footer or sheet key column in the keys in force: each token of `s u -` maps on
    /// its own (`Space u`); words such as `Esc` or `C-Enter` stay. `None` when no token is
    /// left.
    pub(in crate::screen) fn show(self, panel: Panel, keys: &str) -> Option<String> {
        if self.preset != Preset::Lazygit {
            return Some(keys.to_owned());
        }
        let mut tokens: Vec<String> = Vec::new();
        for token in keys.split(' ') {
            let shown = match Key::parse(token) {
                Some(key) => match self.shown(panel, key) {
                    Some(key) => key.label(),
                    None => continue,
                },
                None => token.to_owned(),
            };
            if !tokens.contains(&shown) {
                tokens.push(shown);
            }
        }
        (!tokens.is_empty()).then(|| tokens.join(" "))
    }

    /// A list of `(keys, label)` hints in the keys in force, without the ones the preset
    /// leaves out.
    pub(in crate::screen) fn show_all<L: Copy>(
        self,
        panel: Panel,
        hints: &[(&str, L)],
    ) -> Vec<(String, L)> {
        hints
            .iter()
            .filter_map(|(keys, label)| Some((self.show(panel, keys)?, *label)))
            .collect()
    }

    /// The lazygit keys that run an action Dusty Git keeps in another panel, for `?`.
    pub(in crate::screen) fn extras(self, panel: Panel) -> Vec<(String, &'static str)> {
        self.bindings(panel)
            .into_iter()
            .filter_map(|(key, to, _)| match to {
                To::Stash => Some((key.label(), "Stash all changes")),
                To::Pull => Some((key.label(), "Pull (confirm)")),
                To::Key(_) => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::plain(code)
    }

    #[test]
    fn the_leader_runs_a_chord_or_cancels_without_a_side_effect() {
        let mut workspace = Workspace::new(std::env::temp_dir().join("dusty-keymap-no-repo"));
        let keys = workspace.key_table();
        assert_eq!(keys.keys(Named::ToggleSidebar).as_deref(), Some("Space e"));
        assert_eq!(keys.keys(Named::Toggle), None);
        assert_eq!(workspace.chord_key(&key(KeyCode::Char('e'))), Step::Pass);
        assert_eq!(workspace.chord_key(&key(KeyCode::Char(' '))), Step::Wait);
        assert!(workspace.leader_pending());
        assert_eq!(
            workspace.chord_key(&key(KeyCode::Char('e'))),
            Step::Run(Named::ToggleSidebar)
        );
        assert!(!workspace.leader_pending());
        assert_eq!(workspace.chord_key(&key(KeyCode::Char(' '))), Step::Wait);
        assert_eq!(workspace.chord_key(&key(KeyCode::Char('j'))), Step::Cancel);
        assert_eq!(workspace.chord_key(&key(KeyCode::Char(' '))), Step::Wait);
        assert_eq!(workspace.chord_key(&key(KeyCode::Escape)), Step::Cancel);
        assert!(!workspace.leader_pending());
        assert_eq!(workspace.chord_key(&key(KeyCode::Enter)), Step::Pass);
    }

    #[test]
    fn the_editor_leader_and_the_lazygit_preset_switch_the_table() {
        assert_eq!(Keys::new("space", Preset::Dusty).leader(), ' ');
        assert_eq!(Keys::new("<space>", Preset::Dusty).leader(), ' ');
        assert_eq!(Keys::new("\\", Preset::Dusty).leader(), '\\');
        assert_eq!(Keys::new("<weird>", Preset::Dusty).leader(), ' ');
        let lazy = Keys::new("space", Preset::from_name("lazygit"));
        assert_eq!(lazy.leader(), ',');
        assert_eq!(lazy.keys(Named::ToggleSidebar).as_deref(), Some(", e"));
        assert_eq!(lazy.keys(Named::Toggle).as_deref(), Some("Space"));
        let mut workspace = Workspace::new(std::env::temp_dir().join("dusty-keymap-no-repo"));
        workspace.set_keys(lazy);
        assert_eq!(
            workspace.chord_key(&key(KeyCode::Char(' '))),
            Step::Run(Named::Toggle)
        );
        assert_eq!(workspace.chord_key(&key(KeyCode::Char(','))), Step::Wait);
        assert_eq!(
            workspace.chord_key(&key(KeyCode::Char('e'))),
            Step::Run(Named::ToggleSidebar)
        );
    }

    fn lazygit() -> Keys {
        Keys::new("space", Preset::Lazygit)
    }

    #[test]
    fn the_lazygit_preset_runs_each_mapped_key_as_its_dusty_git_action() {
        use Panel::*;
        let keys = lazygit();
        let k = Key::Char;
        let expected: &[(Panel, Key, To)] = &[
            (Files, k(' '), To::Key(k('-'))),
            (Files, Key::Enter, To::Key(k('v'))),
            (Files, k('e'), To::Key(Key::Enter)),
            (Files, k('o'), To::Key(Key::Enter)),
            (Files, k('`'), To::Key(k('t'))),
            (Files, k('v'), To::Key(k('V'))),
            (Files, k('s'), To::Stash),
            (Files, k('R'), To::Key(k('r'))),
            (Worktrees, k(' '), To::Key(Key::Enter)),
            (Submodules, k(' '), To::Key(k('-'))),
            (Submodules, k('i'), To::Key(k('u'))),
            (Branches, k('G'), To::Key(k('o'))),
            (Branches, k(' '), To::Key(Key::Enter)),
            (Branches, k('M'), To::Key(k('m'))),
            (Branches, k('r'), To::Key(k('R'))),
            (Branches, k('R'), To::Key(k('e'))),
            (Remotes, k(' '), To::Key(Key::Enter)),
            (Tags, k('R'), To::Key(k('r'))),
            (Commits, k('r'), To::Key(k('W'))),
            (Commits, k('R'), To::Key(k('W'))),
            (Reflog, k(' '), To::Key(k('o'))),
            (Stash, k(' '), To::Key(k('a'))),
            (Stash, k('g'), To::Key(k('p'))),
            (Staging, k(' '), To::Key(k('a'))),
            (Staging, k('v'), To::Key(k('V'))),
            (Staging, k('e'), To::Key(Key::Enter)),
            (CommitFiles, k('R'), To::Key(k('r'))),
            (Commits, Key::Ctrl('j'), To::Key(k('J'))),
            (Commits, Key::Ctrl('k'), To::Key(k('K'))),
            (Stash, k('p'), To::Pull),
            (Files, Key::Ctrl('z'), To::Key(k('Z'))),
            (Commits, Key::Ctrl('z'), To::Key(k('Z'))),
        ];
        for (panel, pressed, action) in expected {
            assert_eq!(
                keys.translate(*panel, *pressed),
                Some(*action),
                "{panel:?} {pressed:?}"
            );
        }
        // Keys lazygit shares with Dusty Git, or that it has no word for, stay Dusty Git's.
        for (panel, pressed) in [
            (Files, 'a'),
            (Files, 'c'),
            (Files, 'A'),
            (Files, 'd'),
            (Files, 'u'),
            (Files, 'P'),
            (Branches, 'n'),
            (Branches, 'd'),
            (Branches, 'u'),
            (Branches, 'o'),
            (Commits, 'A'),
            (Commits, 'F'),
            (Commits, 's'),
            (Commits, 'f'),
            (Commits, 'd'),
            (Commits, 'e'),
            (Commits, 'y'),
            (Commits, 'o'),
            (CommitFiles, 'y'),
            (Files, 'p'),
            (Files, 'z'),
            (Tags, 'P'),
            (Reflog, 'n'),
            (Reflog, 'w'),
            (Stash, 'd'),
            (Remotes, 'f'),
            (Tags, 'd'),
        ] {
            assert_eq!(
                keys.translate(panel, k(pressed)),
                None,
                "{panel:?} {pressed}"
            );
        }
        // The default keys translate nothing.
        let dusty = Keys::default();
        for panel in Panel::ALL {
            for key in dusty_keys(panel) {
                assert_eq!(dusty.translate(panel, key), None);
                assert_eq!(dusty.shown(panel, key), Some(key));
            }
        }
    }

    #[test]
    fn no_lazygit_panel_binds_a_key_twice_or_loses_an_action() {
        let keys = lazygit();
        for panel in Panel::ALL {
            let bindings = keys.bindings(panel);
            for (at, (pressed, ..)) in bindings.iter().enumerate() {
                assert!(
                    !bindings[at + 1..]
                        .iter()
                        .any(|(other, ..)| other == pressed),
                    "{panel:?} binds {pressed:?} twice"
                );
            }
            let listed = || lazygit_panel(panel).iter().chain(lazygit_aliases(panel));
            for (pressed, ..) in listed() {
                assert_eq!(
                    listed().filter(|(other, ..)| other == pressed).count(),
                    1,
                    "{panel:?} lists {pressed:?} twice"
                );
            }
            // Each Dusty Git key is shown as a key that runs it, unless the preset says
            // what took it.
            let shadowed = lazygit_shadowed(panel);
            for key in dusty_keys(panel) {
                match keys.shown(panel, key) {
                    Some(shown) if shadowed.iter().any(|(gone, _)| *gone == key) => {
                        assert!(shadowed.contains(&(key, Some(shown))), "{panel:?} {key:?}");
                    }
                    Some(shown) => {
                        let runs = keys.translate(panel, shown).unwrap_or(To::Key(shown));
                        assert_eq!(
                            runs,
                            To::Key(key),
                            "{panel:?}: {shown:?} should run {key:?}"
                        );
                    }
                    None => assert!(
                        shadowed.contains(&(key, None)),
                        "{panel:?} lost {key:?} without saying so"
                    ),
                }
            }
            // No two Dusty Git actions show the same key, a cover aside.
            let mut seen: Vec<(Key, Key)> = Vec::new();
            for key in dusty_keys(panel) {
                if shadowed.iter().any(|(gone, _)| *gone == key) {
                    continue;
                }
                if let Some(shown) = keys.shown(panel, key) {
                    assert!(
                        !seen.iter().any(|(other, at)| *at == shown && *other != key),
                        "{panel:?}: {shown:?} shown for two actions"
                    );
                    seen.push((key, shown));
                }
            }
        }
    }

    #[test]
    fn hints_and_sheet_rows_follow_the_keys_in_force() {
        let keys = lazygit();
        assert_eq!(keys.show(Panel::Files, "s u -").as_deref(), Some("Space u"));
        assert_eq!(keys.show(Panel::Files, "v").as_deref(), Some("Enter"));
        assert_eq!(keys.show(Panel::Files, "V").as_deref(), Some("v"));
        assert_eq!(keys.show(Panel::Branches, "R").as_deref(), Some("r"));
        assert_eq!(keys.show(Panel::Branches, "e").as_deref(), Some("R"));
        assert_eq!(keys.show(Panel::Branches, "m").as_deref(), Some("M"));
        assert_eq!(keys.show(Panel::Branches, "o").as_deref(), Some("o"));
        assert_eq!(keys.show(Panel::Submodules, "u").as_deref(), Some("u"));
        assert_eq!(keys.show(Panel::Branches, "r"), None);
        assert_eq!(keys.show(Panel::Files, "r").as_deref(), Some("R"));
        assert_eq!(keys.show(Panel::Stash, "p").as_deref(), Some("g"));
        assert_eq!(keys.show(Panel::Files, "Esc").as_deref(), Some("Esc"));
        assert_eq!(keys.show(Panel::Commits, "A W F").as_deref(), Some("A r F"));
        assert_eq!(keys.show(Panel::Commits, "J K").as_deref(), Some("C-j C-k"));
        assert_eq!(keys.show(Panel::Files, "z Z").as_deref(), Some("z C-z"));
        assert_eq!(keys.show(Panel::Stash, "p").as_deref(), Some("g"));
        assert_eq!(
            keys.extras(Panel::Stash),
            vec![("p".to_owned(), "Pull (confirm)")]
        );
        assert_eq!(
            keys.show(Panel::Files, "C-h/j/k/l").as_deref(),
            Some("C-h/j/k/l")
        );
        assert_eq!(
            keys.extras(Panel::Files),
            vec![("s".to_owned(), "Stash all changes")]
        );
        let dusty = Keys::default();
        assert_eq!(dusty.show(Panel::Files, "s u -").as_deref(), Some("s u -"));
        assert!(dusty.extras(Panel::Files).is_empty());
        let hints = [("s", "stage"), ("r", "refresh"), ("Esc", "back")];
        assert_eq!(
            keys.show_all(Panel::Branches, &hints),
            vec![("s".to_owned(), "stage"), ("Esc".to_owned(), "back")]
        );
    }
}
