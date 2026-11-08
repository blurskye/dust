//! Install this guest twice to observe ordered wrappers around native typing.
// SPDX-License-Identifier: Apache-2.0
wit_bindgen::generate!({ path: "../../../wit", world: "editor-plugin" });
use dust::plugin::host;
use std::cell::RefCell;
thread_local! { static NAME: RefCell<String> = const { RefCell::new(String::new()) }; }

struct Trace;
impl Guest for Trace {
    fn activate() -> Result<Vec<String>, String> {
        Ok(vec!["enable".into(), "trace".into()])
    }
    fn execute_command(command: String, call: host::CommandCall) -> Result<String, String> {
        if command == "enable" {
            let name = call.command.split('.').nth(1).ok_or("missing owner")?;
            NAME.with(|stored| *stored.borrow_mut() = name.into());
            host::wrap_command(&host::CommandWrapper {
                command: "edit.insert_text".into(),
                handler: "trace".into(),
                order: if name == "wrappersa" { 10 } else { 20 },
            })?;
        } else {
            let name = NAME.with(|stored| stored.borrow().clone());
            host::notify(&host::NotifyRequest {
                level: host::NoticeLevel::Info,
                message: format!("{name}:{}", if call.after { "after" } else { "before" }),
            })?;
            if call.after {
                if let Some(value) = &call.previous {
                    host::set_command_result(value)?;
                }
            } else {
                host::call_next(call.pending.ok_or("missing continuation")?)?;
            }
        }
        Ok(String::new())
    }
}
export!(Trace);
