//! Tutor lessons: what a lesson teaches, and how it knows the user actually did it.
//!
//! Progress is derived from real
//! observed events in the exercise rather than from a quiz, so nothing a user merely answers can
//! complete a step. And a lesson that depends on a feature is available only once that feature
//! exists, so no chapter teaches something the editor cannot do.
use std::collections::BTreeSet;

/// A capability a lesson teaches. `exists` is the gate: a lesson is offered only when every
/// feature it teaches exists.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TutorFeature {
    /// Motions, buffers and the command line: present in this revision.
    Editing,
    /// Staging a change: present in this revision.
    Staging,
    /// Writing a commit: present in this revision.
    Commit,
    /// Remote fetch, update and push. Not implemented in this revision (todo 10).
    Remotes,
    /// Conflict resolution and recovery. Not implemented in this revision (todo 11).
    Conflicts,
}

impl TutorFeature {
    #[must_use]
    pub fn exists(self) -> bool {
        match self {
            Self::Editing | Self::Staging | Self::Commit => true,
            Self::Remotes | Self::Conflicts => false,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Editing => "editing",
            Self::Staging => "staging",
            Self::Commit => "commit",
            Self::Remotes => "remotes",
            Self::Conflicts => "conflicts",
        }
    }
}

/// A step completes when the user performs a real action. There is deliberately no variant for
/// "answered the question": a quiz is not evidence, so it cannot be named here.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TutorStep {
    /// Completes when a change is really staged, observed from the mutation outcome.
    StageAChange,
    /// Completes when a commit is really created.
    WriteACommit,
    /// Completes when a remote update really runs. Gated with `TutorFeature::Remotes`.
    UpdateFromRemote,
}

impl TutorStep {
    /// The command whose success completes this step, so a lesson can show the effective binding
    /// for the key the user must actually press.
    #[must_use]
    pub fn command(self) -> &'static str {
        match self {
            Self::StageAChange => "workspace.git.hunk.stage",
            Self::WriteACommit => "workspace.git.commit.submit",
            Self::UpdateFromRemote => "workspace.git.remote.update",
        }
    }

    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::StageAChange => "Stage one change",
            Self::WriteACommit => "Write a commit",
            Self::UpdateFromRemote => "Update from the remote",
        }
    }
}

/// A real action observed in the session. One variant per thing that actually happened, so a
/// step cannot be completed by anything else.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TutorAction {
    Staged,
    Committed,
}

impl TutorAction {
    #[must_use]
    pub fn completes(self) -> TutorStep {
        match self {
            Self::Staged => TutorStep::StageAChange,
            Self::Committed => TutorStep::WriteACommit,
        }
    }
}

pub struct TutorLesson {
    pub id: &'static str,
    pub title: &'static str,
    pub chapter: u8,
    pub features: &'static [TutorFeature],
    pub steps: &'static [TutorStep],
}

impl TutorLesson {
    /// Whether every feature this lesson teaches exists. A lesson that fails this is not
    /// teachable and says which feature is missing rather than shipping a wrong lesson.
    #[must_use]
    pub fn missing_feature(&self) -> Option<TutorFeature> {
        self.features.iter().copied().find(|f| !f.exists())
    }

    #[must_use]
    pub fn is_available(&self) -> bool {
        self.missing_feature().is_none()
    }
}

/// The chapters, in order. Chapters 1 and 2 teach what this revision can do; chapter 3 teaches
/// remotes and is therefore gated rather than shipped, which the surface states outright.
pub const LESSONS: &[TutorLesson] = &[
    TutorLesson {
        id: "chapter1.stage",
        title: "Chapter 1 · Stage a change",
        chapter: 1,
        features: &[TutorFeature::Staging],
        steps: &[TutorStep::StageAChange],
    },
    TutorLesson {
        id: "chapter2.commit",
        title: "Chapter 2 · Write a commit",
        chapter: 2,
        features: &[TutorFeature::Commit],
        steps: &[TutorStep::WriteACommit],
    },
    TutorLesson {
        id: "chapter3.remote",
        title: "Chapter 3 · Update from a remote",
        chapter: 3,
        features: &[TutorFeature::Remotes],
        steps: &[TutorStep::UpdateFromRemote],
    },
];

/// What the user has actually done, in this session, in the practice sandbox.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TutorProgress {
    completed: BTreeSet<TutorStep>,
}

impl TutorProgress {
    /// Record a real action. Returns whether it completed something NEW, so a caller can say so
    /// once rather than on every repetition.
    pub fn observe(&mut self, action: TutorAction) -> bool {
        self.completed.insert(action.completes())
    }

    #[must_use]
    pub fn is_complete(&self, step: TutorStep) -> bool {
        self.completed.contains(&step)
    }

    #[must_use]
    pub fn completed(&self) -> usize {
        self.completed.len()
    }
}
