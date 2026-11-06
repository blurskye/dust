//! Recorded editing actions and replay validation.

use crate::editor::CommandInvocation;
use crate::editor::InvocationSource;

#[derive(Debug, Eq, PartialEq)]
pub(in crate::app) enum ReplayError {
    NoChange,
    RepeatLimit,
    NoMacro,
    RecursionLimit,
    StepLimit,
}

pub(in crate::app) fn repeated(mut invocation: CommandInvocation) -> CommandInvocation {
    invocation.source = InvocationSource::Repeat;
    invocation.target = Default::default();
    invocation
}

pub(in crate::app) fn replayed(mut invocation: CommandInvocation) -> CommandInvocation {
    invocation.source = InvocationSource::Macro;
    invocation.target = Default::default();
    invocation
}
