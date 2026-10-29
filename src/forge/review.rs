//! The review room: a pinned revision, anchors that are verified or absent, and inputs that keep
//! their secrets out of every rendered form (W-23).
//!
//! The contract: "A review room pins the inspected revision and revalidates it before every write:
//! approvals, comments, inline comments and merges are refused if the head moved, and the new head
//! is shown." ... "Discussions anchor only to a verified path, side and line; a thread whose
//! coordinates no longer exist remains readable without a fabricated anchor." ... "Dispatching a
//! workflow validates typed inputs where a schema exists and never places secret input values into
//! logs, history or persisted form state."

/// Which side of the diff a coordinate belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeSide {
    Old,
    New,
}

/// A path, side and line a discussion claims to be anchored at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Coordinates {
    pub path: String,
    pub side: ForgeSide,
    pub line: u64,
}

/// The coordinates the inspected revision actually HAS. An anchor is only kept if it is in here.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VerifiedCoordinates {
    entries: Vec<Coordinates>,
}

impl VerifiedCoordinates {
    #[must_use]
    pub fn new(entries: Vec<Coordinates>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn contains(&self, coordinates: &Coordinates) -> bool {
        self.entries.contains(coordinates)
    }
}

/// A discussion as it was read. `anchor` is what this revision can HONESTLY say about where it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Discussion {
    pub id: u64,
    pub body: String,
    pub coordinates: Option<Coordinates>,
    pub anchor: Option<Coordinates>,
}

/// Keep every thread readable and drop only the anchors this revision cannot verify: a thread whose
/// coordinates are gone is NOT deleted and does NOT receive a fabricated line.
#[must_use]
pub fn anchored(discussions: Vec<Discussion>, verified: &VerifiedCoordinates) -> Vec<Discussion> {
    discussions
        .into_iter()
        .map(|mut discussion| {
            discussion.anchor = discussion
                .coordinates
                .clone()
                .filter(|coordinates| verified.contains(coordinates));
            discussion
        })
        .collect()
}

/// Why a write could not be validated. The refusal CARRIES the head that moved it, because the
/// contract requires the new head to be SHOWN rather than merely mentioned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewRefusal {
    /// The inspected revision is no longer the head.
    HeadMoved { inspected: String, head: String },
}

/// A revision the review room pinned when it started looking.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedRevision(String);

impl PinnedRevision {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The proof that the pinned revision IS the head, taken at the moment of a write.
///
/// It has no public constructor and its fields are private: the only way to obtain one is
/// [`ReviewRoom::revalidate`], and a write takes it BY VALUE so one validation cannot authorise a
/// second write. "Revalidated before every write" is therefore a property of the types rather than
/// a rule a future caller has to remember.
#[derive(Debug, Eq, PartialEq)]
pub struct ValidatedReview {
    revision: String,
}

impl ValidatedReview {
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

/// What the room is looking at.
#[derive(Debug)]
pub struct ReviewRoom {
    pinned: PinnedRevision,
}

impl ReviewRoom {
    /// Pin the revision being inspected.
    #[must_use]
    pub fn inspect(revision: impl Into<String>) -> Self {
        Self {
            pinned: PinnedRevision(revision.into()),
        }
    }

    #[must_use]
    pub fn pinned(&self) -> &PinnedRevision {
        &self.pinned
    }

    /// Revalidate against the head AS IT IS NOW. A write cannot be made without the value this
    /// returns, and the value is consumed by the write it authorises.
    pub fn revalidate(&self, head: &str) -> Result<ValidatedReview, ReviewRefusal> {

        if head != self.pinned.0 {
            return Err(ReviewRefusal::HeadMoved {
                inspected: self.pinned.0.clone(),
                head: head.to_owned(),
            });
        }
        Ok(ValidatedReview {
            revision: head.to_owned(),
        })
    }

    /// Approve the reviewed revision. The validation is consumed, so a later write needs a later
    /// validation.
    pub fn approve(&self, validated: ValidatedReview) -> String {
        format!("approve {}", validated.revision())
    }

    /// Comment on the reviewed revision, on the same terms.
    pub fn comment(&self, validated: ValidatedReview, body: &str) -> String {
        format!("comment {} {body}", validated.revision())
    }

    /// Merge the reviewed revision, on the same terms.
    pub fn merge(&self, validated: ValidatedReview) -> String {
        format!("merge {}", validated.revision())
    }
}

/// A workflow input's value, which is either shown or never shown.
#[derive(Clone, Eq, PartialEq)]
pub enum InputValue {
    Plain(String),
    Secret(String),
}

impl InputValue {
    /// The value as it may be rendered. A secret renders as a placeholder and NOTHING else - not
    /// even a prefix, because a prefix is a leak.
    #[must_use]
    pub fn rendered(&self) -> &str {
        match self {
            Self::Plain(value) => value,
            Self::Secret(_) => "<redacted>",
        }
    }
}

impl std::fmt::Debug for InputValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plain(value) => formatter.debug_tuple("Plain").field(value).finish(),
            Self::Secret(_) => formatter.debug_tuple("Secret").field(&"<redacted>").finish(),
        }
    }
}

/// One named input to a workflow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowInput {
    pub name: String,
    pub value: InputValue,
}

/// The schema a workflow declares, where one exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputSchema {
    pub required: Vec<String>,
}

/// Why a dispatch was refused before anything was sent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputRefusal {
    Missing(String),
    Unknown(String),
}

/// Validate typed inputs against the declared schema.
pub fn validate_inputs(
    inputs: &[WorkflowInput],
    schema: &InputSchema,
) -> Result<(), InputRefusal> {
    for required in &schema.required {
        if !inputs.iter().any(|input| &input.name == required) {
            return Err(InputRefusal::Missing(required.clone()));
        }
    }
    for input in inputs {
        if !schema.required.contains(&input.name) {
            return Err(InputRefusal::Unknown(input.name.clone()));
        }
    }
    Ok(())
}

/// The line a dispatch may write to a log: names and rendered values, so a secret cannot reach it.
#[must_use]
pub fn dispatch_log(action: &str, inputs: &[WorkflowInput]) -> String {
    let rendered = inputs
        .iter()
        .map(|input| format!("{}={}", input.name, input.value.rendered()))
        .collect::<Vec<_>>()
        .join(" ");
    format!("dispatch {action} {rendered}")
}
