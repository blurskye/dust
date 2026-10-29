//! The review room: the revision pin, anchors that are verified-or-absent, and inputs that keep
//! their secrets. Nothing here reaches a network.

use super::review::{
    Coordinates, Discussion, ForgeSide, InputRefusal, InputSchema, InputValue, ReviewRoom,
    ReviewRefusal, VerifiedCoordinates, WorkflowInput, anchored, dispatch_log, validate_inputs,
};

#[test]
fn a_pinned_revision_is_admitted_and_a_write_consumes_its_validation() {
    let room = ReviewRoom::inspect("abc123");
    let validated = room
        .revalidate("abc123")
        .expect("the pinned revision IS the head");
    assert_eq!(validated.revision(), "abc123");
    // The three writes the contract names all require the validation that only revalidating makes.
    assert_eq!(room.approve(validated), "approve abc123");
    let validated = room.revalidate("abc123").expect("a second write needs a second validation");
    assert_eq!(room.comment(validated, "looks right"), "comment abc123 looks right");
    let validated = room.revalidate("abc123").expect("and a third");
    assert_eq!(room.merge(validated), "merge abc123");
}

#[test]
fn a_moved_head_is_refused_and_the_new_head_is_carried() {
    let room = ReviewRoom::inspect("abc123");
    // The refusal must SHOW the new head, not merely say the revision was stale.
    assert_eq!(
        room.revalidate("def456"),
        Err(ReviewRefusal::HeadMoved {
            inspected: "abc123".into(),
            head: "def456".into(),
        }),
    );
    match room.revalidate("def456") {
        Ok(_) => panic!("a moved head must not produce a validation"),
        Err(ReviewRefusal::HeadMoved { inspected, head }) => {
            assert_eq!(inspected, "abc123", "the inspected revision is reported");
            assert_eq!(head, "def456", "and the NEW head is carried, so it can be shown");
        }
    }
}

#[test]
fn a_thread_whose_coordinates_are_gone_stays_readable_without_an_anchor() {
    let here = Coordinates {
        path: "src/main.rs".into(),
        side: ForgeSide::New,
        line: 12,
    };
    let gone = Coordinates {
        path: "src/main.rs".into(),
        side: ForgeSide::New,
        line: 999,
    };
    let verified = VerifiedCoordinates::new(vec![here.clone()]);
    let discussions = vec![
        Discussion {
            id: 1,
            body: "this line needs a guard".into(),
            coordinates: Some(here.clone()),
            anchor: None,
        },
        Discussion {
            id: 2,
            body: "old comment on a line that moved".into(),
            coordinates: Some(gone),
            anchor: None,
        },
        Discussion {
            id: 3,
            body: "a thread with no coordinates at all".into(),
            coordinates: None,
            anchor: None,
        },
    ];
    let anchored = anchored(discussions, &verified);
    assert_eq!(anchored.len(), 3, "every thread stays readable");
    // The verified one keeps its EXACT coordinates.
    assert_eq!(anchored[0].anchor.as_ref(), Some(&here));
    // The gone one is still here, and is NOT given a fabricated line.
    assert_eq!(anchored[1].body, "old comment on a line that moved");
    assert_eq!(anchored[1].anchor, None, "a gone coordinate produces NO anchor");
    assert!(
        anchored[1].coordinates.is_some(),
        "what the thread CLAIMED is still recorded, it is just not presented as an anchor"
    );
    assert_eq!(anchored[2].anchor, None);
}

#[test]
fn a_secret_input_is_absent_from_every_rendered_form_while_a_plain_one_is_visible() {
    let inputs = vec![
        WorkflowInput {
            name: "target".into(),
            value: InputValue::Plain("production".into()),
        },
        WorkflowInput {
            name: "deploy_token".into(),
            value: InputValue::Secret("ghp_supersecret".into()),
        },
    ];
    let debug = format!("{inputs:?}");
    let log = dispatch_log("deploy", &inputs);
    for rendered in [&debug, &log] {
        assert!(
            !rendered.contains("ghp_supersecret"),
            "the secret must not reach a rendered form: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "the secret is shown as redacted: {rendered}");
        // And a PLAIN value IS visible, so redacting everything would also fail.
        assert!(
            rendered.contains("production"),
            "a plain input's value is visible: {rendered}"
        );
    }
    assert_eq!(inputs[1].value.rendered(), "<redacted>");
    assert_eq!(inputs[0].value.rendered(), "production");
}

#[test]
fn typed_inputs_are_validated_against_a_declared_schema() {
    let schema = InputSchema {
        required: vec!["target".into(), "deploy_token".into()],
    };
    let complete = vec![
        WorkflowInput {
            name: "target".into(),
            value: InputValue::Plain("production".into()),
        },
        WorkflowInput {
            name: "deploy_token".into(),
            value: InputValue::Secret("ghp_supersecret".into()),
        },
    ];
    assert!(validate_inputs(&complete, &schema).is_ok());
    assert_eq!(
        validate_inputs(&complete[..1], &schema),
        Err(InputRefusal::Missing("deploy_token".into())),
        "a required input that was not given is refused by NAME"
    );
    let mut extra = complete;
    extra.push(WorkflowInput {
        name: "surprise".into(),
        value: InputValue::Plain("x".into()),
    });
    assert_eq!(
        validate_inputs(&extra, &schema),
        Err(InputRefusal::Unknown("surprise".into())),
        "an input the schema does not declare is refused rather than forwarded"
    );
}
