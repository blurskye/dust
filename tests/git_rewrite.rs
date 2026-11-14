//! Real repositories only: immutable rewrite plans, native commit machinery,
//! byte-exact index/worktree preservation and retained recovery objects.
use dust::git::rewrite::{Kind, Outcome, Plan, Worker};
use git2::Oid;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn command(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    command.current_dir(root).args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", if cfg!(windows) { "NUL" } else { "/dev/null" });
    for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        command.env_remove(key);
    }
    command.output().unwrap()
}
fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = command(root, args);
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    output.stdout
}
fn text(root: &Path, args: &[&str]) -> String {
    String::from_utf8(git(root, args)).unwrap().trim_end().to_owned()
}
fn head(root: &Path) -> Oid { Oid::from_str(&text(root, &["rev-parse", "HEAD"])).unwrap() }
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "--template=", "-b", "main"]);
    git(root, &["config", "user.name", "Committer"]);
    git(root, &["config", "user.email", "committer@example.invalid"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "core.autocrlf", "false"]);
    git(root, &["config", "core.fsmonitor", "false"]);
    let hooks = root.join("empty-hooks");
    fs::create_dir(&hooks).unwrap();
    git(root, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    fs::write(root.join("f"), b"original\n").unwrap();
    git(root, &["add", "f"]);
    git(root, &["commit", "--author=Original Author <original@example.invalid>", "-m", "Original subject", "-m", "Original body"]);
    dir
}
fn stage_with_residual(root: &Path) {
    fs::write(root.join("f"), b"explicit staged content\n").unwrap();
    git(root, &["add", "f"]);
    fs::write(root.join("f"), b"unstaged residual\n\0binary\xff").unwrap();
    fs::write(root.join("untracked"), b"unrelated\0\xfe").unwrap();
}
fn index(root: &Path) -> Vec<u8> {
    let repo = git2::Repository::open(root).unwrap();
    fs::read(repo.index().unwrap().path().unwrap()).unwrap()
}
fn run(plan: Plan, message: &str) -> Result<Outcome, String> {
    let mut worker = Worker::default();
    worker.start(plan, message.into())?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(result) = worker.poll() { return result; }
        assert!(Instant::now() < deadline, "rewrite worker timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn assert_no_locks(root: &Path) {
    let repo = git2::Repository::open(root).unwrap();
    assert!(!repo.path().join("index.lock").exists());
    assert!(!repo.path().join("HEAD.lock").exists());
    if let Some(reference) = repo.find_reference("HEAD").unwrap().symbolic_target() {
        assert!(!repo.commondir().join(format!("{reference}.lock")).exists());
    }
}
fn assert_compact_rewrite_form(screen: &mut dust::git::screen::GitScreen, title: &str, source: bool) {
    let frame = screen.frame(64, 20);
    let rendered = (0..20).map(|y| frame.grid.row_text(y)).collect::<Vec<_>>().join("\n");
    let submit = if title.starts_with("Amend") { " Amend " } else if title.starts_with("Reword") { " Reword " } else { " Fixup " };
    for text in [title, "Message", "Branch", "main", submit, " Cancel ", "Ctrl+Enter", "Esc"] {
        assert!(rendered.contains(text), "missing {text:?} at 64 columns:\n{rendered}");
    }
    if source {
        assert!(rendered.contains("Source") && rendered.contains("Index"), "{rendered}");
    }
    for jargon in ["local only", "Staged-content policy", "autosquash", "undo source", "refs/dust/", "captured staged", "keep unstaged", "(pinned)"] {
        assert!(!rendered.contains(jargon), "unexpected {jargon:?}:\n{rendered}");
    }
    let (x, y, _) = frame.cursor.expect("form controls keep their native cursor");
    assert!(x < 64 && y < 20);
}
fn assert_author_and_parents(root: &Path, before: Oid, after: Oid) {
    let repo = git2::Repository::open(root).unwrap();
    let before = repo.find_commit(before).unwrap();
    let after = repo.find_commit(after).unwrap();
    assert!(before.author() == after.author(), "original author metadata must survive");
    assert_eq!(before.parent_ids().collect::<Vec<_>>(), after.parent_ids().collect::<Vec<_>>());
}

#[test]
fn amend_root_commits_only_explicit_stage_and_retains_residual_and_recovery() {
    let dir = repo(); let root = dir.path();
    let before = head(root);
    stage_with_residual(root);
    let bytes = index(root);
    let result = run(Plan::capture(root, Kind::Amend).unwrap(), "Amended\n\nBody\n").unwrap();
    assert_eq!(head(root), result.head);
    assert_ne!(before, result.head);
    assert_eq!(git(root, &["show", "HEAD:f"]), b"explicit staged content\n");
    assert_eq!(index(root), bytes);
    assert_eq!(fs::read(root.join("f")).unwrap(), b"unstaged residual\n\0binary\xff");
    assert_eq!(fs::read(root.join("untracked")).unwrap(), b"unrelated\0\xfe");
    assert_eq!(text(root, &["rev-parse", &result.recovery]), before.to_string());
    assert_eq!(text(root, &["rev-list", "--parents", "-1", "HEAD"]).split_whitespace().count(), 1);
    assert_author_and_parents(root, before, result.head);
    assert_eq!(text(root, &["diff", "--cached", "--name-only"]), "");
    assert_no_locks(root);
}

#[test]
fn reword_is_byte_exact_for_index_and_files_including_staged_and_untracked_binary() {
    let dir = repo(); let root = dir.path();
    stage_with_residual(root);
    let before = head(root);
    let tree = text(root, &["rev-parse", "HEAD^{tree}"]);
    let bytes = index(root);
    let staged = git(root, &["diff", "--cached", "--binary"]);
    let result = run(Plan::capture(root, Kind::Reword).unwrap(), "Reworded\n\nA new body\n").unwrap();
    assert_eq!(text(root, &["rev-parse", "HEAD^{tree}"]), tree);
    assert_eq!(index(root), bytes);
    assert_eq!(git(root, &["diff", "--cached", "--binary"]), staged);
    assert_eq!(fs::read(root.join("f")).unwrap(), b"unstaged residual\n\0binary\xff");
    assert_eq!(fs::read(root.join("untracked")).unwrap(), b"unrelated\0\xfe");
    assert_eq!(text(root, &["show", "-s", "--format=%B", "HEAD"]), "Reworded\n\nA new body");
    assert_author_and_parents(root, before, result.head);
    assert_no_locks(root);
}

#[test]
fn fixup_targets_exact_selected_ancestor_without_rewriting_history() {
    let dir = repo(); let root = dir.path();
    let target = head(root);
    fs::write(root.join("second"), b"second\n").unwrap();
    git(root, &["add", "second"]); git(root, &["commit", "-m", "Second"]);
    let before = head(root);
    stage_with_residual(root);
    let bytes = index(root);
    let plan = Plan::capture(root, Kind::Fixup(target)).unwrap();
    let result = run(plan, "ignored; selected target owns the fixup message").unwrap();
    assert_eq!(text(root, &["show", "-s", "--format=%s", "HEAD"]), "fixup! Original subject");
    assert_eq!(text(root, &["rev-parse", "HEAD^"]), before.to_string());
    assert_eq!(git(root, &["show", "HEAD:f"]), b"explicit staged content\n");
    assert_eq!(index(root), bytes);
    assert_eq!(fs::read(root.join("f")).unwrap(), b"unstaged residual\n\0binary\xff");
    assert_eq!(text(root, &["rev-list", "--count", "HEAD"]), "3");
    assert_eq!(result.head, head(root));
    assert_no_locks(root);
}

#[test]
fn detached_reword_and_amend_keep_detached_identity_and_author() {
    for kind in [Kind::Reword, Kind::Amend] {
        let dir = repo(); let root = dir.path();
        git(root, &["checkout", "--detach"]);
        let before = head(root);
        stage_with_residual(root);
        let bytes = index(root);
        let result = run(Plan::capture(root, kind).unwrap(), "Detached rewrite").unwrap();
        assert!(!command(root, &["symbolic-ref", "-q", "HEAD"]).status.success());
        assert_eq!(text(root, &["rev-parse", "main"]), before.to_string());
        assert_eq!(index(root), bytes);
        assert_author_and_parents(root, before, result.head);
        assert_no_locks(root);
    }
}

#[test]
fn both_merge_parents_and_original_author_survive_amend_and_reword() {
    for kind in [Kind::Reword, Kind::Amend] {
        let dir = repo(); let root = dir.path();
        git(root, &["checkout", "-b", "side"]);
        fs::write(root.join("side"), b"side\n").unwrap();
        git(root, &["add", "side"]); git(root, &["commit", "-m", "Side"]);
        git(root, &["checkout", "main"]);
        fs::write(root.join("main"), b"main\n").unwrap();
        git(root, &["add", "main"]); git(root, &["commit", "-m", "Main"]);
        git(root, &["merge", "--no-ff", "side", "-m", "Merge"]);
        let before = head(root);
        stage_with_residual(root);
        let result = run(Plan::capture(root, kind).unwrap(), "Rewritten merge").unwrap();
        assert_eq!(text(root, &["show", "-s", "--format=%P", "HEAD"]).split_whitespace().count(), 2);
        assert_author_and_parents(root, before, result.head);
        assert_no_locks(root);
    }
}

#[test]
fn changed_head_index_and_symbolic_branch_are_refused_without_mutation() {
    for change in ["head", "index", "branch"] {
        let dir = repo(); let root = dir.path();
        stage_with_residual(root);
        let plan = Plan::capture(root, Kind::Amend).unwrap();
        match change {
            "head" => { git(root, &["commit", "--allow-empty", "-m", "External"]); }
            "index" => { fs::write(root.join("other"), b"other").unwrap(); git(root, &["add", "other"]); }
            _ => { git(root, &["branch", "other"]); git(root, &["symbolic-ref", "HEAD", "refs/heads/other"]); }
        }
        let before = head(root); let bytes = index(root);
        let result = run(plan, "Must not publish").unwrap_err();
        assert!(result.contains("changed"), "{result}");
        assert_eq!(head(root), before); assert_eq!(index(root), bytes);
        assert_no_locks(root);
    }
}

#[test]
fn foreign_index_lock_is_not_removed_and_retry_after_unlock_succeeds() {
    let dir = repo(); let root = dir.path();
    let before = head(root); let bytes = index(root);
    let plan = Plan::capture(root, Kind::Reword).unwrap();
    fs::write(root.join(".git/index.lock"), b"foreign owner").unwrap();
    assert!(run(plan.clone(), "Retry").unwrap_err().contains("lock"));
    assert_eq!(fs::read(root.join(".git/index.lock")).unwrap(), b"foreign owner");
    assert_eq!(head(root), before); assert_eq!(index(root), bytes);
    fs::remove_file(root.join(".git/index.lock")).unwrap();
    run(plan, "Retry").unwrap();
    assert_no_locks(root);
}

#[test]
fn unborn_and_empty_fixup_and_nonancestor_targets_are_refused() {
    let dir = repo(); let root = dir.path();
    assert!(Plan::capture(root, Kind::Fixup(head(root))).is_err());
    let target = head(root);
    git(root, &["checkout", "--orphan", "unborn"]);
    assert!(Plan::capture(root, Kind::Reword).is_err());
    fs::write(root.join("f"), b"unrelated root\n").unwrap();
    git(root, &["add", "f"]); git(root, &["commit", "-m", "Unrelated"]);
    stage_with_residual(root);
    assert!(Plan::capture(root, Kind::Fixup(target)).unwrap_err().contains("current HEAD history"));
}

#[test]
fn paused_operation_is_refused_at_open_and_again_at_submit() {
    let dir = repo(); let root = dir.path();
    let plan = Plan::capture(root, Kind::Reword).unwrap();
    let before = head(root); let bytes = index(root);
    fs::write(root.join(".git/MERGE_HEAD"), format!("{before}\n")).unwrap();
    assert!(Plan::capture(root, Kind::Amend).is_err());
    assert!(run(plan, "Blocked").unwrap_err().contains("current Git operation"));
    assert_eq!(head(root), before); assert_eq!(index(root), bytes);
    assert_no_locks(root);
}

#[cfg(unix)]
fn hook(root: &Path, name: &str, source: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("empty-hooks").join(name);
    fs::write(&path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn native_hook_rejection_keeps_head_index_and_residual_then_allows_retry() {
    let dir = repo(); let root = dir.path();
    stage_with_residual(root);
    let before = head(root); let bytes = index(root);
    hook(root, "commit-msg", "#!/bin/sh\necho deliberately-rejected >&2\nexit 1\n");
    let plan = Plan::capture(root, Kind::Amend).unwrap();
    assert!(run(plan.clone(), "Rejected").unwrap_err().contains("deliberately-rejected"));
    assert_eq!(head(root), before); assert_eq!(index(root), bytes);
    assert_eq!(fs::read(root.join("f")).unwrap(), b"unstaged residual\n\0binary\xff");
    assert_no_locks(root);
    fs::remove_file(root.join("empty-hooks/commit-msg")).unwrap();
    run(plan, "Accepted after retry").unwrap();
}

#[cfg(unix)]
#[test]
fn hooks_run_and_cannot_retarget_head_or_real_index_while_commit_is_prepared() {
    let dir = repo(); let root = dir.path();
    let before = head(root); let bytes = index(root);
    git(root, &["branch", "other"]);
    hook(root, "pre-commit", "#!/bin/sh\nunset GIT_DIR GIT_INDEX_FILE GIT_WORK_TREE\nif git symbolic-ref HEAD refs/heads/other 2>/dev/null; then exit 41; fi\nif git add f 2>/dev/null; then exit 42; fi\necho hook-ran > hook-marker\n");
    let result = run(Plan::capture(root, Kind::Reword).unwrap(), "Hook checked").unwrap();
    assert_eq!(text(root, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(fs::read(root.join("hook-marker")).unwrap(), b"hook-ran\n");
    assert_eq!(index(root), bytes);
    assert_author_and_parents(root, before, result.head);
    assert_no_locks(root);
}

#[cfg(unix)]
#[test]
fn hook_tree_change_is_refused_before_publication_and_prepared_commit_is_recoverable() {
    let dir = repo(); let root = dir.path();
    let before = head(root); let bytes = index(root);
    hook(root, "pre-commit", "#!/bin/sh\nprintf 'hook tree\\n' > from-hook\ngit add from-hook\n");
    let error = run(Plan::capture(root, Kind::Reword).unwrap(), "Must stay message-only").unwrap_err();
    assert!(error.contains("hook changed"), "{error}");
    assert_eq!(head(root), before); assert_eq!(index(root), bytes);
    let refs = text(root, &["for-each-ref", "--format=%(refname)", "refs/dust/rewrite-result/"]);
    assert!(!refs.is_empty());
    assert_eq!(git(root, &["show", &format!("{}:from-hook", refs.lines().next().unwrap())]), b"hook tree\n");
    // Hook-created work is deliberately retained, never silently discarded.
    assert_eq!(fs::read(root.join("from-hook")).unwrap(), b"hook tree\n");
    assert_no_locks(root);
}

#[cfg(unix)]
#[test]
fn signing_is_not_bypassed_and_failure_keeps_real_state_unchanged() {
    let dir = repo(); let root = dir.path();
    let before = head(root); let bytes = index(root);
    git(root, &["config", "commit.gpgsign", "true"]);
    git(root, &["config", "gpg.program", "/definitely/not/a/signer"]);
    assert!(run(Plan::capture(root, Kind::Reword).unwrap(), "Signed").is_err());
    assert_eq!(head(root), before); assert_eq!(index(root), bytes);
    assert_no_locks(root);
}

#[test]
fn linked_worktree_keeps_other_checkout_head_and_index_untouched() {
    let dir = repo(); let root = dir.path();
    let linked = dir.path().join("linked");
    git(root, &["worktree", "add", "-b", "linked", linked.to_str().unwrap()]);
    let original = head(root); let original_index = index(root);
    stage_with_residual(&linked);
    let bytes = index(&linked);
    let before = head(&linked);
    let result = run(Plan::capture(&linked, Kind::Reword).unwrap(), "Linked message").unwrap();
    assert_eq!(head(root), original); assert_eq!(index(root), original_index);
    assert_eq!(index(&linked), bytes);
    assert_author_and_parents(&linked, before, result.head);
    assert_eq!(text(&linked, &["symbolic-ref", "HEAD"]), "refs/heads/linked");
    assert_no_locks(&linked);
}

#[test]
fn ref_lock_failure_keeps_foreign_lock_and_index_then_recovers() {
    let dir = repo(); let root = dir.path();
    let plan = Plan::capture(root, Kind::Reword).unwrap();
    let before = head(root); let bytes = index(root);
    let lock = root.join(".git/refs/heads/main.lock");
    fs::write(&lock, b"foreign ref owner").unwrap();
    assert!(run(plan.clone(), "Blocked").is_err());
    assert_eq!(fs::read(&lock).unwrap(), b"foreign ref owner");
    assert_eq!(head(root), before); assert_eq!(index(root), bytes);
    assert!(!root.join(".git/index.lock").exists());
    assert!(!root.join(".git/HEAD.lock").exists());
    fs::remove_file(lock).unwrap();
    run(plan, "Retry after ref unlock").unwrap();
    assert_no_locks(root);
}

#[test]
fn reword_preserves_staged_deletion_and_intent_to_add_bytes() {
    let dir = repo(); let root = dir.path();
    git(root, &["rm", "--cached", "f"]);
    fs::write(root.join("intent"), b"not staged content\n").unwrap();
    git(root, &["add", "--intent-to-add", "intent"]);
    let bytes = index(root);
    let staged = git(root, &["diff", "--cached", "--binary"]);
    run(Plan::capture(root, Kind::Reword).unwrap(), "Message only with special index").unwrap();
    assert_eq!(index(root), bytes);
    assert_eq!(git(root, &["diff", "--cached", "--binary"]), staged);
    assert_eq!(git(root, &["show", "HEAD:f"]), b"original\n");
    assert_eq!(fs::read(root.join("intent")).unwrap(), b"not staged content\n");
}

#[test]
fn screen_bindings_host_veto_stale_form_and_retry_preserve_draft() {
    use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
    use dust::git::screen::GitScreen;
    use dust::ui::{KeyInputKind, UiEvent};
    fn key(screen: &mut GitScreen, c: char, control: bool) {
        screen.event(UiEvent::Key {
            key: KeyEvent { code: KeyCode::Char(c), modifiers: Modifiers { control, ..Modifiers::default() } },
            kind: KeyInputKind::Press,
        });
    }
    fn view(screen: &mut GitScreen) -> String {
        let frame = screen.frame(140, 45);
        (0..45).map(|y| frame.grid.row_text(y)).collect::<Vec<_>>().join("\n")
    }
    fn settle(screen: &mut GitScreen) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            screen.poll();
            if !screen.busy() { return; }
            assert!(Instant::now() < deadline, "{}", view(screen));
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let dir = repo(); let root = dir.path();
    let before = head(root);
    let mut screen = GitScreen::open(root).unwrap();
    // Initial status reads are not a write, but let the ordinary workers finish.
    for _ in 0..50 { screen.poll(); std::thread::sleep(Duration::from_millis(5)); }
    screen.write_refusal = Some("Unsaved host buffer".into());
    key(&mut screen, 'W', false);
    assert!(view(&mut screen).contains("Unsaved host buffer"));
    screen.write_refusal = None;
    key(&mut screen, 'W', false);
    assert!(view(&mut screen).contains("Reword HEAD"));
    assert_compact_rewrite_form(&mut screen, "Reword HEAD", false);
    key(&mut screen, 'u', true);
    screen.event(UiEvent::Paste("Draft that must survive stale submission".into()));
    fs::write(root.join("external"), b"new stage\n").unwrap(); git(root, &["add", "external"]);
    let bytes = index(root);
    screen.event(UiEvent::Key {
        key: KeyEvent { code: KeyCode::Enter, modifiers: Modifiers { control: true, ..Modifiers::default() } },
        kind: KeyInputKind::Press,
    });
    settle(&mut screen);
    let rendered = view(&mut screen);
    assert!(rendered.contains("index changed"), "{rendered}");
    assert!(rendered.contains("Draft that must survive stale submission"), "{rendered}");
    assert_eq!(head(root), before); assert_eq!(index(root), bytes);
    screen.event(UiEvent::Key { key: KeyEvent::plain(KeyCode::Escape), kind: KeyInputKind::Press });
    key(&mut screen, 'A', false);
    assert!(view(&mut screen).contains("Amend HEAD"));
    assert_compact_rewrite_form(&mut screen, "Amend HEAD", true);
}

#[test]
fn dropping_ui_worker_does_not_abort_an_accepted_commit() {
    let dir = repo(); let root = dir.path();
    let before = head(root);
    let mut worker = Worker::default();
    worker.start(Plan::capture(root, Kind::Reword).unwrap(), "Owned until completion".into()).unwrap();
    drop(worker);
    let deadline = Instant::now() + Duration::from_secs(15);
    while head(root) == before || root.join(".git/index.lock").exists() {
        assert!(Instant::now() < deadline, "worker did not complete after receiver dropped");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(text(root, &["show", "-s", "--format=%s", "HEAD"]), "Owned until completion");
    assert_no_locks(root);
}

#[cfg(unix)]
#[test]
fn reword_preserves_symlink_target_and_file_modes() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = repo(); let root = dir.path();
    symlink("f", root.join("link")).unwrap();
    fs::set_permissions(root.join("f"), fs::Permissions::from_mode(0o755)).unwrap();
    git(root, &["add", "f", "link"]);
    let bytes = index(root);
    run(Plan::capture(root, Kind::Reword).unwrap(), "Do not touch modes or links").unwrap();
    assert_eq!(index(root), bytes);
    assert_eq!(fs::read_link(root.join("link")).unwrap(), Path::new("f"));
    assert_eq!(fs::metadata(root.join("f")).unwrap().permissions().mode() & 0o777, 0o755);
}

#[test]
fn empty_message_is_refused_before_acceptance_and_empty_root_can_be_reworded() {
    let dir = repo(); let root = dir.path();
    git(root, &["rm", "f"]);
    git(root, &["commit", "--allow-empty", "--amend", "-m", "Empty root"]);
    let bytes = index(root); let before = head(root);
    let plan = Plan::capture(root, Kind::Reword).unwrap();
    let mut worker = Worker::default();
    assert!(worker.start(plan.clone(), " \n".into()).is_err());
    assert!(worker.start(plan.clone(), "NUL\0message".into()).is_err());
    let result = run(plan, "Empty root reworded").unwrap();
    assert_eq!(index(root), bytes);
    assert_author_and_parents(root, before, result.head);
    assert_eq!(text(root, &["ls-tree", "HEAD"]), "");
}

#[test]
fn screen_fixup_binding_uses_selected_history_row_not_head() {
    use dust::editor::input::{KeyCode, KeyEvent, Modifiers};
    use dust::git::screen::GitScreen;
    use dust::ui::{KeyInputKind, UiEvent};
    fn press(screen: &mut GitScreen, code: KeyCode) {
        screen.event(UiEvent::Key { key: KeyEvent::plain(code), kind: KeyInputKind::Press });
    }
    fn view(screen: &mut GitScreen) -> String {
        let frame = screen.frame(140, 45);
        (0..45).map(|y| frame.grid.row_text(y)).collect::<Vec<_>>().join("\n")
    }
    let dir = repo(); let root = dir.path();
    let target = head(root);
    fs::write(root.join("second"), b"second\n").unwrap();
    git(root, &["add", "second"]); git(root, &["commit", "-m", "Second"]);
    let before = head(root);
    stage_with_residual(root);
    let bytes = index(root);
    let mut screen = GitScreen::open(root).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        screen.poll();
        let rendered = view(&mut screen);
        if rendered.contains("Second") && rendered.contains("Original subject") { break; }
        assert!(Instant::now() < deadline, "{rendered}");
        std::thread::sleep(Duration::from_millis(5));
    }
    press(&mut screen, KeyCode::Char('4'));
    press(&mut screen, KeyCode::Char('j'));
    press(&mut screen, KeyCode::Char('F'));
    let rendered = view(&mut screen);
    assert!(rendered.contains("Fixup ·"), "{rendered}");
    assert!(rendered.contains("Source") && rendered.contains("Index"), "{rendered}");
    assert!(rendered.contains(&target.to_string()[..8]), "{rendered}");
    assert_compact_rewrite_form(&mut screen, "Fixup ·", true);
    screen.event(UiEvent::Key {
        key: KeyEvent { code: KeyCode::Enter, modifiers: Modifiers { control: true, ..Modifiers::default() } },
        kind: KeyInputKind::Press,
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        screen.poll();
        if !screen.busy() { break; }
        assert!(Instant::now() < deadline, "{}", view(&mut screen));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(text(root, &["show", "-s", "--format=%s", "HEAD"]), "fixup! Original subject");
    assert_eq!(text(root, &["rev-parse", "HEAD^"]), before.to_string());
    assert_eq!(index(root), bytes);
    let rendered = view(&mut screen);
    assert!(rendered.contains(&format!("Fixup {}", &head(root).to_string()[..8])), "{rendered}");
    assert!(!rendered.contains("undo source") && !rendered.contains("refs/dust/"), "{rendered}");
}

#[cfg(unix)]
#[test]
fn branch_conditional_signing_cannot_be_lost_in_private_commit_context() {
    let dir = repo(); let root = dir.path();
    let before = head(root); let bytes = index(root);
    let signing = root.join("branch-signing-config");
    fs::write(&signing, b"[commit]\n\tgpgSign = true\n[gpg]\n\tprogram = /definitely/not/a/signer\n").unwrap();
    git(root, &["config", "includeIf.onbranch:main.path", signing.to_str().unwrap()]);
    assert!(run(Plan::capture(root, Kind::Reword).unwrap(), "Must require branch signing").is_err());
    assert_eq!(head(root), before); assert_eq!(index(root), bytes);
    assert_no_locks(root);
}

#[cfg(unix)]
#[test]
fn reference_transaction_policy_is_refused_not_silently_bypassed() {
    let dir = repo(); let root = dir.path();
    let before = head(root); let bytes = index(root);
    hook(root, "reference-transaction", "#!/bin/sh\nexit 0\n");
    let result = run(Plan::capture(root, Kind::Reword).unwrap(), "Must honor publication policy").unwrap_err();
    assert!(result.contains("reference-transaction hook"), "{result}");
    assert_eq!(head(root), before); assert_eq!(index(root), bytes);
    assert_eq!(text(root, &["for-each-ref", "--format=%(refname)", "refs/dust/"]), "");
    assert_no_locks(root);
}

fn reflog_ids(root: &Path, name: &str) -> Vec<Oid> {
    let repo = git2::Repository::open(root).unwrap();
    if !repo.reference_has_log(name).unwrap() { return Vec::new(); }
    let log = repo.reflog(name).unwrap();
    log.iter().map(|entry| entry.id_new()).collect()
}

fn check_reflog_transition(checkout: &str, policy: &str, remove_logs: bool) {
    let dir = repo(); let root = dir.path();
    let linked = root.join("linked");
    match checkout {
        "master" => { git(root, &["branch", "-m", "master"]); }
        "linked" => { git(root, &["worktree", "add", "-b", "linked", linked.to_str().unwrap()]); }
        "detached" => { git(root, &["checkout", "--detach"]); }
        _ => {}
    }
    let root = if checkout == "linked" { linked.as_path() } else { root };
    git(root, &["config", "core.logAllRefUpdates", policy]);
    let repo = git2::Repository::open(root).unwrap();
    let reference = repo.find_reference("HEAD").unwrap().symbolic_target().map(str::to_owned);
    if remove_logs {
        if repo.reference_has_log("HEAD").unwrap() { repo.reflog_delete("HEAD").unwrap(); }
        if let Some(name) = &reference
            && repo.reference_has_log(name).unwrap() {
            repo.reflog_delete(name).unwrap();
        }
    }
    let before = head(root);
    let head_log = reflog_ids(root, "HEAD");
    let branch_log = reference.as_deref().map(|name| reflog_ids(root, name));
    let result = run(Plan::capture(root, Kind::Reword).unwrap(), "One rewrite event")
        .unwrap_or_else(|error| panic!("{checkout}, policy {policy}, missing logs {remove_logs}: {error}"));
    let actual = reflog_ids(root, "HEAD");
    if remove_logs {
        assert!(actual.is_empty(), "disabled reflog must not be created");
        assert!(!repo.reference_has_log("HEAD").unwrap());
    } else {
        assert_eq!(actual.len(), head_log.len() + 1, "{checkout}, policy {policy}");
        assert_eq!(actual[0], result.head);
        assert_eq!(text(root, &["rev-parse", "HEAD@{1}"]), before.to_string());
    }
    if let Some(name) = &reference {
        let actual = reflog_ids(root, name);
        if remove_logs {
            assert!(actual.is_empty()); assert!(!repo.reference_has_log(name).unwrap());
        } else {
            assert_eq!(actual.len(), branch_log.unwrap().len() + 1, "{checkout}, policy {policy}");
            assert_eq!(actual[0], result.head); assert_eq!(actual[1], before);
        }
    }
    assert_no_locks(root);
}

#[test]
fn rewrite_reflogs_have_one_transition_on_main_master_linked_and_detached() {
    for checkout in ["main", "master", "linked", "detached"] {
        for policy in ["true", "false"] { check_reflog_transition(checkout, policy, false); }
    }
}

#[test]
fn disabled_reflog_policy_does_not_create_missing_head_or_branch_logs() {
    for checkout in ["main", "master", "linked", "detached"] {
        check_reflog_transition(checkout, "false", true);
    }
}

#[cfg(unix)]
#[test]
fn dev_null_hooks_path_supports_amend_reword_and_fixup() {
    for operation in ["amend", "reword", "fixup"] {
        let dir = repo(); let root = dir.path();
        let original = head(root);
        stage_with_residual(root);
        let bytes = index(root);
        git(root, &["config", "core.hooksPath", "/dev/null"]);
        let kind = match operation { "amend" => Kind::Amend, "reword" => Kind::Reword, _ => Kind::Fixup(original) };
        run(Plan::capture(root, kind).unwrap(), "Hooks disabled").unwrap();
        assert_eq!(index(root), bytes);
        assert_eq!(fs::read(root.join("f")).unwrap(), b"unstaged residual\n\0binary\xff");
        assert_no_locks(root);
    }
}

#[cfg(unix)]
#[test]
fn default_relative_absolute_hooks_run_and_reference_policy_is_still_refused() {
    for mode in ["default", "relative", "absolute"] {
        let dir = repo(); let root = dir.path();
        let hooks = if mode == "default" {
            git(root, &["config", "--unset", "core.hooksPath"]);
            let hooks = root.join(".git/hooks"); fs::create_dir_all(&hooks).unwrap(); hooks
        } else {
            if mode == "relative" { git(root, &["config", "core.hooksPath", "empty-hooks"]); }
            root.join("empty-hooks")
        };
        hook(root, "commit-msg", "#!/bin/sh\nprintf 'hook ran\\n' > path-hook-marker\n");
        if mode == "default" { fs::copy(root.join("empty-hooks/commit-msg"), hooks.join("commit-msg")).unwrap(); }
        run(Plan::capture(root, Kind::Reword).unwrap(), "Resolved hooks path").unwrap();
        assert_eq!(fs::read(root.join("path-hook-marker")).unwrap(), b"hook ran\n");
        hook(root, "reference-transaction", "#!/bin/sh\nexit 0\n");
        if mode == "default" { fs::copy(root.join("empty-hooks/reference-transaction"), hooks.join("reference-transaction")).unwrap(); }
        let before = head(root); let bytes = index(root);
        let result = run(Plan::capture(root, Kind::Reword).unwrap(), "Must honor policy").unwrap_err();
        assert!(result.contains("reference-transaction hook"), "{mode}: {result}");
        assert_eq!(head(root), before); assert_eq!(index(root), bytes);
        assert_no_locks(root);
    }
}

#[cfg(unix)]
#[test]
fn hooks_path_inspection_retains_real_io_failures() {
    use std::os::unix::fs::symlink;
    let dir = repo(); let root = dir.path();
    symlink("hook-cycle", root.join("hook-cycle")).unwrap();
    git(root, &["config", "core.hooksPath", "hook-cycle"]);
    let before = head(root); let bytes = index(root);
    let result = run(Plan::capture(root, Kind::Reword).unwrap(), "Do not mask IO").unwrap_err();
    assert!(result.contains("Cannot inspect reference-transaction hook"), "{result}");
    assert_eq!(head(root), before); assert_eq!(index(root), bytes);
    assert_no_locks(root);
}


#[test]
fn disabled_reflog_policy_preserves_message_less_history_and_adds_one_event() {
    for checkout in ["main", "linked", "detached"] {
        let dir = repo();
        let linked = dir.path().join("linked");
        match checkout {
            "linked" => { git(dir.path(), &["worktree", "add", "-b", "linked", linked.to_str().unwrap()]); }
            "detached" => { git(dir.path(), &["checkout", "--detach"]); }
            _ => {}
        }
        let root = if checkout == "linked" { linked.as_path() } else { dir.path() };
        stage_with_residual(root);
        git(root, &["config", "core.logAllRefUpdates", "false"]);
        let repository = git2::Repository::open(root).unwrap();
        let reference = repository.find_reference("HEAD").unwrap().symbolic_target().map(str::to_owned);
        let mut logs = vec![("HEAD".to_owned(), repository.path().join("logs/HEAD"))];
        if let Some(name) = reference {
            logs.push((name.clone(), repository.commondir().join("logs").join(name)));
        }
        let mut snapshots = Vec::new();
        for (name, path) in logs {
            // Message-less records are valid Git history; linked HEAD has one
            // naturally. Also seed one in ordinary branch and detached logs.
            let mut bytes = fs::read(&path).unwrap();
            let end = bytes.iter().position(|byte| *byte == b'\n').unwrap();
            if let Some(tab) = bytes[..end].iter().position(|byte| *byte == b'\t') {
                bytes.drain(tab..end);
            }
            fs::write(&path, &bytes).unwrap();
            let log = repository.reflog(&name).unwrap();
            assert!(log.iter().any(|entry| entry.message_bytes().is_none()));
            snapshots.push((name, path, bytes, log.len()));
        }
        let original = head(root);
        let index_bytes = index(root);
        let outcome = run(Plan::capture(root, Kind::Reword).unwrap(), "One event with message-less history")
            .unwrap_or_else(|error| panic!("{checkout}: {error}"));
        assert_eq!(head(root), outcome.head);
        assert_eq!(text(root, &["rev-parse", "HEAD@{1}"]), original.to_string());
        for (name, path, prefix, count) in snapshots {
            let actual = fs::read(path).unwrap();
            assert!(actual.starts_with(&prefix), "old log bytes changed: {checkout} {name}");
            let log = repository.reflog(&name).unwrap();
            assert_eq!(log.len(), count + 1, "{checkout} {name}");
            assert_eq!(log.get(0).unwrap().id_new(), outcome.head);
            assert_eq!(log.get(0).unwrap().id_old(), original);
        }
        assert_eq!(index(root), index_bytes);
        assert_eq!(fs::read(root.join("f")).unwrap(), b"unstaged residual\n\0binary\xff");
        assert_no_locks(root);
    }
}

#[test]
fn disabled_reflog_write_error_retains_foreign_lock_and_recovery_diagnostic() {
    let dir = repo();
    let root = dir.path();
    stage_with_residual(root);
    git(root, &["config", "core.logAllRefUpdates", "false"]);
    let original = head(root);
    let index_bytes = index(root);
    let head_log = root.join(".git/logs/HEAD");
    let log_bytes = fs::read(&head_log).unwrap();
    let repository = git2::Repository::open(root).unwrap();
    let reference = repository.find_reference("HEAD").unwrap().symbolic_target().unwrap().to_owned();
    let branch_log = repository.commondir().join("logs").join(&reference);
    let branch_bytes = fs::read(&branch_log).unwrap();
    let branch_count = repository.reflog(&reference).unwrap().len();
    let foreign_lock = root.join(".git/logs/HEAD.lock");
    fs::write(&foreign_lock, b"foreign reflog owner").unwrap();
    let error = run(Plan::capture(root, Kind::Reword).unwrap(), "Locked reflog publication").unwrap_err();
    assert!(error.contains("Cannot publish commit") && error.contains("inspect HEAD"), "{error}");
    assert!(error.contains("refs/dust/rewrite-result/") && error.contains(&format!("refs/dust/rewrite/{original}")), "{error}");
    assert_eq!(head(root), original);
    assert_eq!(index(root), index_bytes);
    assert_eq!(fs::read(&head_log).unwrap(), log_bytes);
    // The branch log is written first, before HEAD's foreign log lock stops
    // publication. This is a real partial effect, not an all-or-nothing failure.
    let partial_branch_bytes = fs::read(&branch_log).unwrap();
    assert!(partial_branch_bytes.starts_with(&branch_bytes));
    let partial_log = repository.reflog(&reference).unwrap();
    assert_eq!(partial_log.len(), branch_count + 1);
    let prepared = partial_log.get(0).unwrap().id_new();
    assert_ne!(prepared, original);
    assert_eq!(partial_log.get(0).unwrap().id_old(), original);
    let prepared_ref = format!("refs/dust/rewrite-result/{prepared}");
    assert_eq!(repository.refname_to_id(&prepared_ref).unwrap(), prepared);
    assert!(repository.find_commit(prepared).is_ok());
    assert!(error.contains(&prepared_ref), "{error}");
    assert_eq!(repository.refname_to_id(&reference).unwrap(), original);
    assert_eq!(fs::read(&foreign_lock).unwrap(), b"foreign reflog owner");
    assert_eq!(fs::read(root.join("f")).unwrap(), b"unstaged residual\n\0binary\xff");
    assert_no_locks(root);
    fs::remove_file(foreign_lock).unwrap();
    let retry = run(Plan::capture(root, Kind::Reword).unwrap(), "Retry after reflog owner releases lock").unwrap();
    assert_eq!(head(root), retry.head);
    assert_ne!(retry.head, prepared);
    // Retry preserves the failed prepared event, but its old OID must be
    // the captured real HEAD, not that never-published historical event.
    assert!(fs::read(&branch_log).unwrap().starts_with(&partial_branch_bytes));
    let retried_log = repository.reflog(&reference).unwrap();
    assert_eq!(retried_log.len(), branch_count + 2);
    assert_eq!(retried_log.get(0).unwrap().id_new(), retry.head);
    assert_eq!(retried_log.get(0).unwrap().id_old(), original);
    assert_eq!(retried_log.get(1).unwrap().id_new(), prepared);
    assert_eq!(text(root, &["rev-parse", "HEAD@{1}"]), original.to_string());
    assert_no_locks(root);
}


fn check_noncanonical_raw_reflogs(checkout: &str, skipped_line: bool) {
    let dir = repo();
    let linked = dir.path().join("linked");
    match checkout {
        "linked" => { git(dir.path(), &["worktree", "add", "-b", "linked", linked.to_str().unwrap()]); }
        "detached" => { git(dir.path(), &["checkout", "--detach"]); }
        _ => {}
    }
    let root = if checkout == "linked" { linked.as_path() } else { dir.path() };
    stage_with_residual(root);
    git(root, &["config", "core.logAllRefUpdates", "false"]);
    git(root, &["config", "user.name", "Zoë  Two Spaces"]);
    let repository = git2::Repository::open(root).unwrap();
    let reference = repository.find_reference("HEAD").unwrap().symbolic_target().map(str::to_owned);
    let mut paths = vec![("HEAD".to_owned(), repository.path().join("logs/HEAD"))];
    if let Some(name) = reference {
        paths.push((name.clone(), repository.commondir().join("logs").join(name)));
    }
    let mut snapshots = Vec::new();
    for (name, path) in paths {
        let mut bytes = fs::read(&path).unwrap();
        let parsed_before = repository.reflog(&name).unwrap().len();
        if skipped_line {
            bytes.extend_from_slice(b"parser-skipped historical bytes \xff \t  \n");
        } else {
            let first_end = bytes.iter().position(|byte| *byte == b'\n').unwrap();
            let first = &bytes[..first_end];
            let prefix_end = first.iter().position(|byte| *byte == b'\t').unwrap_or(first.len());
            let mut extra = first[..prefix_end].to_vec();
            extra.extend_from_slice(b"\thistorical trailing whitespace \t   \n");
            bytes.extend_from_slice(&extra);
        }
        fs::write(&path, &bytes).unwrap();
        let parsed_seeded = repository.reflog(&name).unwrap().len();
        assert_eq!(parsed_seeded, parsed_before + usize::from(!skipped_line));
        snapshots.push((name, path, bytes, parsed_seeded));
    }
    let original = head(root);
    let index_bytes = index(root);
    let outcome = run(Plan::capture(root, Kind::Reword).unwrap(), "Raw history must remain exact")
        .unwrap_or_else(|error| panic!("{checkout}, skipped {skipped_line}: {error}"));
    let commit = repository.find_commit(outcome.head).unwrap();
    let committer = commit.raw_header_bytes().split(|byte| *byte == b'\n')
        .find_map(|line| line.strip_prefix(b"committer ")).unwrap();
    let mut expected_tail = format!("{original} {} ", outcome.head).into_bytes();
    expected_tail.extend_from_slice(committer);
    expected_tail.extend_from_slice(b"\tdust: local commit rewrite\n");
    for (name, path, prefix, parsed) in snapshots {
        let actual = fs::read(path).unwrap();
        assert!(actual.starts_with(&prefix), "historical bytes changed: {checkout} {name}");
        assert_eq!(&actual[prefix.len()..], expected_tail.as_slice());
        let log = repository.reflog(&name).unwrap();
        assert_eq!(log.len(), parsed + 1);
        assert_eq!(log.get(0).unwrap().id_old(), original);
        assert_eq!(log.get(0).unwrap().id_new(), outcome.head);
    }
    assert_eq!(head(root), outcome.head);
    assert_eq!(index(root), index_bytes);
    assert_eq!(fs::read(root.join("f")).unwrap(), b"unstaged residual\n\0binary\xff");
    assert_no_locks(root);
}

#[test]
fn disabled_reflog_policy_preserves_historical_trailing_whitespace_bytes() {
    for checkout in ["main", "linked", "detached"] {
        check_noncanonical_raw_reflogs(checkout, false);
    }
}

#[test]
fn disabled_reflog_policy_preserves_parser_skipped_historical_bytes() {
    for checkout in ["main", "linked", "detached"] {
        check_noncanonical_raw_reflogs(checkout, true);
    }
}
