//! Surface imports exercised by the real component fixture.

use crate::{field_text, host};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static REJECT: AtomicBool = AtomicBool::new(false);
static FORMAT_ERROR: AtomicBool = AtomicBool::new(false);
static DEFER: AtomicBool = AtomicBool::new(false);
static INPUT: AtomicU64 = AtomicU64::new(0);
static SELECT: AtomicU64 = AtomicU64::new(0);

fn value(node: host::ValueNode) -> host::CommandValue { host::CommandValue { nodes: vec![node] } }

pub(super) fn event(event: &str, call: &host::CommandCall) -> Option<Result<String, String>> {
    Some((|| {
        if DEFER.load(Ordering::SeqCst) && matches!(event, "ui-input" | "ui-select") {
            let request = crate::field_integer(&call.arguments, "request").ok_or("missing presenter request")? as u64;
            if event == "ui-input" { INPUT.store(request, Ordering::SeqCst); } else { SELECT.store(request, Ordering::SeqCst); }
            return Ok(String::new());
        }
        match event {
            "ui-input" => host::set_command_result(&value(host::ValueNode::Text("presented".into())))?,
            "ui-select" => host::set_command_result(&value(host::ValueNode::Text("one".into())))?,
            "notify" => return Ok(String::new()),
            "document-write" => {
                if REJECT.load(Ordering::SeqCst) { return Err("provider refused write".into()); }
                host::set_command_result(&value(host::ValueNode::Boolean(true)))?;
            }
            "format" => {
                if FORMAT_ERROR.load(Ordering::SeqCst) { return Err("formatter failed".into()); }
                let before = field_text(&call.arguments, "text").ok_or("missing save text")?;
                host::set_command_result(&host::CommandValue { nodes: vec![
                    host::ValueNode::Integer(0), host::ValueNode::Integer(before.len() as i64), host::ValueNode::Text(format!("{before}!")),
                    host::ValueNode::Object(vec![host::ValueField { name: "start".into(), value: 0 }, host::ValueField { name: "end".into(), value: 1 }, host::ValueField { name: "replacement".into(), value: 2 }]),
                    host::ValueNode::Sequence(vec![3]),
                ] })?;
            }
            "surface-reply" => {
                let replies = host::take_command_replies();
                host::show_message(&format!("surface-reply:{:?}", replies))?;
                return Ok(String::new());
            }
            _ => return Err("not a surface event".into()),
        }
        Ok(String::new())
    })()).filter(|_| matches!(event, "ui-input" | "ui-select" | "notify" | "document-write" | "format" | "surface-reply"))
}

pub(super) fn action(action: &str) -> Result<String, String> {
    match action {
        "input" | "secret" => { host::ui_input(&host::InputRequest { title: "Fixture input".into(), initial: "initial".into(), secret: action == "secret" }, "drive")?; }
        "select" => { host::ui_select(&host::SelectRequest { title: "Fixture select".into(), items: vec![host::SelectItem { id: "one".into(), label: "First".into(), detail: Some("detail".into()) }, host::SelectItem { id: "two".into(), label: "Second".into(), detail: None }] }, "drive")?; }
        "notify" => host::notify(&host::NotifyRequest { level: host::NoticeLevel::Warning, message: "surface warning".into() })?,
        "answer-input" => host::answer_input(INPUT.load(Ordering::SeqCst), Some("later"))?,
        "answer-select" => host::answer_select(SELECT.load(Ordering::SeqCst), Some("one"))?,
        "presenters" | "presenters-deferred" => {
            DEFER.store(action == "presenters-deferred", Ordering::SeqCst);
            host::set_presenter(host::PresenterKind::Input, Some("drive"))?;
            host::set_presenter(host::PresenterKind::Select, Some("drive"))?;
            host::set_presenter(host::PresenterKind::Notifications, Some("drive"))?;
        }
        "document" => { host::open_editable_document(&host::EditableDocument { source: "directory".into(), key: "root".into(), title: "Fixture document".into(), language: "directory".into(), text: "old".into() }, "drive")?; }
        "reject" => { REJECT.store(true, Ordering::SeqCst); }
        "formatter" | "before" => {
            let step = host::Formatter { name: "fixture".into(), handler: "drive".into(), order: 10 };
            if action == "before" { host::register_before_save("directory", &step)?; } else { host::register_formatter("directory", &step)?; }
        }
        "format-error" => { FORMAT_ERROR.store(true, Ordering::SeqCst); }
        "segments" | "segments-update" => {
            for location in [host::SegmentLocation::Statusline, host::SegmentLocation::BufferLine] {
                host::set_chrome_segment(&host::ChromeSegment { name: "fixture".into(), location, order: 5, text: vec![host::StyledText { text: if action == "segments" { "Hello" } else { "Updated" }.into(), style: "warning".into() }, host::StyledText { text: "world".into(), style: "accent".into() }], command: Some("application.notification.dismiss".into()) })?;
            }
        }
        "segments-remove" => {
            host::remove_chrome_segment(host::SegmentLocation::Statusline, "fixture")?;
            host::remove_chrome_segment(host::SegmentLocation::BufferLine, "fixture")?;
        }
        _ => return Err("unknown surface action".into()),
    }
    Ok(String::new())
}
