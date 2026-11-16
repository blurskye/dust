//! The Conflicts chapter: one lesson per operation that stops on a conflict (merge, rebase,
//! cherry-pick, pull) and one on picking per hunk. Each starts on a
//! practice repository already stopped, and ends only when the repository shows the
//! conflict resolved and the operation finished.
use super::*;

/// `notes.txt` with two conflicts five lines apart, so Git keeps them separate.
const NOTES: &str = "notes.txt";

fn notes(first: &str, last: &str) -> String {
    format!("{first}\ntwo\nthree\nfour\nfive\nsix\n{last}\n")
}

/// `topic` and `practice` both change the greeting; a cherry-pick of `topic` stops.
fn picking(f: &Fixture) -> Setup {
    diverged(f)?;
    f.attempt(&["cherry-pick", "topic"])?;
    Ok(None)
}

/// `origin/practice` and `practice` both change the greeting; `git pull` merges and stops.
fn pulling(f: &Fixture) -> Setup {
    f.git(&["switch", "--quiet", "-c", "teammate"])?;
    f.write(GREET, &greeting("Hello from origin"))?;
    f.commit("Greet from origin")?;
    f.git(&["push", "--quiet", "origin", "teammate:practice"])?;
    f.git(&["switch", "--quiet", "practice"])?;
    f.git(&["branch", "--quiet", "-D", "teammate"])?;
    f.write(GREET, &greeting("Hello from practice"))?;
    f.commit("Greet from practice")?;
    f.attempt(&["pull", "--no-rebase", "--no-edit", "origin", "practice"])?;
    Ok(None)
}

/// A merge of `topic` stops with two conflicts in one file.
fn two_conflicts(f: &Fixture) -> Setup {
    f.write(NOTES, &notes("one", "seven"))?;
    f.commit("Add notes")?;
    f.git(&["switch", "--quiet", "-c", "topic"])?;
    f.write(NOTES, &notes("one from topic", "seven from topic"))?;
    f.commit("Notes from topic")?;
    f.git(&["switch", "--quiet", "practice"])?;
    f.write(NOTES, &notes("one from practice", "seven from practice"))?;
    f.commit("Notes from practice")?;
    f.attempt(&["merge", "--no-edit", "topic"])?;
    Ok(None)
}

fn merged(p: &mut Probe) -> bool {
    p.clean() && p.commit(0).is_some_and(|commit| commit.parent_count() == 2)
}

/// Both sides kept somewhere in the notes, every conflict resolved and staged.
fn picked_per_hunk(p: &mut Probe) -> bool {
    let text = p
        .repo
        .workdir()
        .and_then(|root| std::fs::read_to_string(root.join(NOTES)).ok())
        .unwrap_or_default();
    !p.conflicted()
        && !text.contains("<<<<<<<")
        && text.contains("from practice")
        && text.contains("from topic")
}

pub(super) const CHAPTER: Chapter = Chapter {
    name: "Conflicts",
    lessons: &[
        fresh(
            "Merge conflict",
            "2, greet.rs, 0: b keeps both sides of the hunk; then C, Enter merges.",
            merging,
            merged,
        )
        .teaching(&["Operation · Take both", "Operation · Continue"]),
        fresh(
            "Rebase conflict",
            "2, greet.txt, 0: on the theirs line Enter (lazygit: Space) takes it; C, Enter.",
            rebase_stopped,
            |p| {
                let subjects = p.subjects();
                subjects
                    .iter()
                    .any(|subject| subject == "Polish greet and farewell")
                    && subjects
                        .iter()
                        .any(|subject| subject == "Shout the greeting")
            },
        )
        .teaching(&[
            "Operation · Take the side under the cursor",
            "Operation · Continue",
        ]),
        fresh(
            "Cherry-pick conflict",
            "2, greet.rs, 0: t takes the picked commit's side; C, Enter finishes the pick.",
            picking,
            |p| p.clean() && p.subject(0) == "Greet from topic",
        )
        .teaching(&["Operation · Take ours · theirs", "Operation · Continue"]),
        fresh(
            "Pull conflict",
            "2, greet.rs, 0: o keeps yours. M opens the options: Enter, Enter merges.",
            pulling,
            merged,
        )
        .teaching(&[
            "Operation · Take ours · theirs",
            "Operation · Continue · skip · abort menu",
        ]),
        fresh(
            "Pick per hunk",
            "2, notes.txt, 0: o on one conflict, t on the other. e edits there instead.",
            two_conflicts,
            picked_per_hunk,
        )
        .teaching(&[
            "Operation · Take ours · theirs",
            "Operation · Edit at the conflict",
        ]),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Every lesson starts stopped on a real conflict, in the operation it names.
    #[test]
    fn each_lesson_starts_on_its_conflict() {
        let dir = TempDir::new().unwrap();
        for (index, lesson) in CHAPTER.lessons.iter().enumerate() {
            let fixture = Fixture::new(&dir.path().join(index.to_string())).unwrap();
            (lesson.setup)(&fixture).unwrap();
            let repository = Repository::open(&fixture.root).unwrap();
            let state = repository.state();
            assert!(
                repository.index().unwrap().has_conflicts(),
                "{}: no conflict",
                lesson.title
            );
            let text = |path: &str| std::fs::read_to_string(fixture.root.join(path)).unwrap();
            match lesson.title {
                "Merge conflict" => assert_eq!(state, RepositoryState::Merge),
                "Rebase conflict" => assert!(
                    matches!(
                        state,
                        RepositoryState::Rebase
                            | RepositoryState::RebaseInteractive
                            | RepositoryState::RebaseMerge
                    ),
                    "{state:?}"
                ),
                "Cherry-pick conflict" => assert_eq!(state, RepositoryState::CherryPick),
                "Pull conflict" => {
                    assert_eq!(state, RepositoryState::Merge);
                    // The merge brings in what the remote has.
                    assert_eq!(
                        repository.refname_to_id("MERGE_HEAD").unwrap(),
                        repository
                            .refname_to_id("refs/remotes/origin/practice")
                            .unwrap()
                    );
                }
                "Pick per hunk" => {
                    assert_eq!(state, RepositoryState::Merge);
                    assert_eq!(
                        super::super::super::conflicts::hunks(&text(NOTES)).len(),
                        2,
                        "{}",
                        text(NOTES)
                    );
                }
                other => panic!("no starting check for {other}"),
            }
            assert!(
                !lesson.teaches.is_empty(),
                "{} teaches nothing",
                lesson.title
            );
            for item in lesson.teaches {
                assert!(item.starts_with("Operation · "), "{item}");
            }
        }
    }
}
