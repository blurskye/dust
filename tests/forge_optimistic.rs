//! Writes show at once and roll back when the forge refuses them.
#![cfg(unix)]
use dusty::editor::input::KeyCode;

#[path = "support/forge_fake.rs"]
mod forge_fake;
#[path = "support/forge_screen.rs"]
mod forge_screen;
use forge_screen::*;

// The Conversation card: `☺ you commented · sending…` until the forge answers.
const SENDING: &str = "you commented · sending…";

#[test]
fn a_refused_comment_rolls_back_and_a_confirmed_one_stays() {
    if fixture_child(
        "a_refused_comment_rolls_back_and_a_confirmed_one_stays",
        serde_json::json!({
            "calls":[],"comments":["Optimistic fixture ready"],"issues":navigation_issues(),
            "fail_comments":1,
            "gates":[
                {"contains":"comment again","name":"second"},
                {"contains":"First optimistic","name":"first"}
            ]
        }),
    ) {
        return;
    }
    let mut screen = open();
    goto(&mut screen, 0, 1);
    wait(&mut screen, "Optimistic fixture ready");
    key(&mut screen, KeyCode::Char('c'));
    paste(&mut screen, "First optimistic comment");
    control(&mut screen, KeyCode::Enter);
    screen.poll();
    let sent = view(&mut screen);
    assert!(
        sent.contains(SENDING),
        "the comment shows before GitHub answers:\n{sent}"
    );
    assert!(sent.contains("First optimistic comment"), "{sent}");
    release("first");
    wait(
        &mut screen,
        "GitHub rejected the comment · your draft is kept",
    );
    let refused = view(&mut screen);
    assert!(
        !refused.contains(SENDING),
        "the refused comment is rolled back:\n{refused}"
    );
    assert!(
        refused.contains("GitHub rejected the comment · your draft is kept"),
        "{refused}"
    );

    key(&mut screen, KeyCode::Char('c'));
    key(&mut screen, KeyCode::End);
    paste(&mut screen, " again");
    control(&mut screen, KeyCode::Enter);
    screen.poll();
    assert!(view(&mut screen).contains(SENDING));
    release("second");
    wait_for(&mut screen, "the confirmed comment from GitHub", |frame| {
        frame.contains("First optimistic comment again") && !frame.contains(SENDING)
    });
}
