use crate::app::tests::*;




#[test]
pub(in crate::app) fn a_name_no_worktree_matches_is_the_name_of_a_new_one() {
    let (_app_dirs, mut app) = app();
    let new_row = crate::workspace::PickerItem {
        badge: String::new(),
        kind: crate::workspace::PickerItemKind::Entry,
        id: crate::workspace::PickerItemId("git-worktree-new".into()),
        label: "New worktree…".into(),
        detail: String::new(),
        source: String::new(),
        action: crate::workspace::PickerAction::None,
    };
    let payload = crate::app::picker::PickerPayload::Command(
        "workspace.git.worktree.create".into(),
    );
    app.install_picker("Git worktrees".into(), vec![(new_row, payload)]);
    assert_eq!(
        app.search.new_worktree_name(),
        None,
        "the row itself is selected"
    );
    for key in ["x", "y", "z"] {
        app.handle_event(UiEvent::Key {
            key: key.parse().unwrap(),
            kind: crate::ui::KeyInputKind::Press,
        });
    }
    assert_eq!(app.search.new_worktree_name().as_deref(), Some("xyz"));
    app.install_picker("Files".into(), Vec::new());
    assert_eq!(
        app.search.new_worktree_name(),
        None,
        "only the worktree list creates"
    );
}
use crate::app::picker::PickerPayload;
use crate::ui::KeyInputKind;
use crate::ui::UiEvent;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;

