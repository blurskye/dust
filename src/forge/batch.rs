//! GraphQL alias batching: many per-item queries become one request
//! with an alias per item. Interactive batches take up to 50 aliases and wait up to 50 ms for
//! company; background batches take 25 and wait 500 ms. Callers that overlap in time on the
//! same group (forge, repository, document) join one batch; a full batch goes out at once.

use crate::forge::request::ForgeError;
use crate::forge::transport::Class;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

/// Aliases per request and how long the first caller waits for others.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub aliases: usize,
    pub window: Duration,
}

pub fn limits(class: Class) -> Limits {
    match class {
        Class::Interactive => Limits {
            aliases: 50,
            window: Duration::from_millis(50),
        },
        Class::Background => Limits {
            aliases: 25,
            window: Duration::from_millis(500),
        },
    }
}

/// Item number → its aliased result (`Value::Null` when the forge had none).
pub type Answers = BTreeMap<u64, Value>;
type Outcome = Result<Answers, ForgeError>;

#[derive(Default)]
struct Batch {
    outcome: Mutex<Option<Outcome>>,
    ready: Condvar,
}

struct Open {
    numbers: Vec<u64>,
    batch: Arc<Batch>,
}

fn open() -> &'static Mutex<HashMap<String, Open>> {
    static OPEN: OnceLock<Mutex<HashMap<String, Open>>> = OnceLock::new();
    OPEN.get_or_init(|| Mutex::new(HashMap::new()))
}

fn wait(batch: &Batch, cancel: &AtomicBool) -> Outcome {
    let mut outcome = batch
        .outcome
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    loop {
        if let Some(outcome) = outcome.as_ref() {
            return outcome.clone();
        }
        if cancel.load(Ordering::Acquire) {
            return Err(ForgeError::Cancelled);
        }
        outcome = batch
            .ready
            .wait_timeout(outcome, Duration::from_millis(20))
            .unwrap_or_else(|error| error.into_inner())
            .0;
    }
}

fn pick(answers: &Answers, numbers: &[u64], into: &mut Answers) {
    for number in numbers {
        if let Some(value) = answers.get(number) {
            into.insert(*number, value.clone());
        }
    }
}

/// Answer `numbers` through batches of `group`. `send` sends one batch (at most
/// `limits(class).aliases` numbers) and returns what it found.
pub fn submit(
    group: &str,
    class: Class,
    numbers: &[u64],
    cancel: &AtomicBool,
    send: &dyn Fn(&[u64]) -> Outcome,
) -> Outcome {
    let limits = limits(class);
    let key = format!("{group}|{class:?}");
    let mut wanted = Vec::new();
    for number in numbers {
        if !wanted.contains(number) {
            wanted.push(*number);
        }
    }
    let mut answers = Answers::new();
    // Full batches go out now; the rest joins a batch that is still collecting, or opens one
    // others may join for one window.
    let mut chunks = wanted.chunks(limits.aliases).collect::<Vec<_>>();
    let rest = match chunks.last() {
        Some(last) if last.len() < limits.aliases => chunks.pop().map(<[u64]>::to_vec),
        _ => None,
    };
    for chunk in chunks {
        if cancel.load(Ordering::Acquire) {
            return Err(ForgeError::Cancelled);
        }
        answers.extend(send(chunk)?);
    }
    let Some(mine) = rest else {
        return Ok(answers);
    };
    // Joining and opening happen under one lock, so callers arriving together never both
    // open (and send) their own batch.
    let batch = {
        let mut open = open().lock().unwrap_or_else(|error| error.into_inner());
        match open.get_mut(&key) {
            Some(collecting) if collecting.numbers.len() + mine.len() <= limits.aliases => {
                for number in &mine {
                    if !collecting.numbers.contains(number) {
                        collecting.numbers.push(*number);
                    }
                }
                Err(collecting.batch.clone())
            }
            // One is collecting but has no room for ours: send ours alone.
            Some(_) => Ok(None),
            None => {
                let batch = Arc::new(Batch::default());
                open.insert(
                    key.clone(),
                    Open {
                        numbers: mine.clone(),
                        batch: batch.clone(),
                    },
                );
                Ok(Some(batch))
            }
        }
    };
    let batch = match batch {
        Err(joined) => {
            pick(&wait(&joined, cancel)?, &mine, &mut answers);
            return Ok(answers);
        }
        Ok(None) => {
            answers.extend(send(&mine)?);
            return Ok(answers);
        }
        Ok(Some(batch)) => batch,
    };
    let started = Instant::now();
    while started.elapsed() < limits.window && !cancel.load(Ordering::Acquire) {
        let full = open()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&key)
            .is_some_and(|open| open.numbers.len() >= limits.aliases);
        if full {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let numbers = open()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&key)
        .map(|open| open.numbers)
        .unwrap_or_else(|| mine.clone());
    let outcome = if cancel.load(Ordering::Acquire) {
        Err(ForgeError::Cancelled)
    } else {
        send(&numbers)
    };
    *batch
        .outcome
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(outcome.clone());
    batch.ready.notify_all();
    // Joiners' numbers went out in this batch too; they pick their own answers.
    pick(&outcome?, &mine, &mut answers);
    Ok(answers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn overlapping_callers_share_one_batch_and_full_batches_split() {
        static SENT: AtomicUsize = AtomicUsize::new(0);
        let send = |numbers: &[u64]| -> Outcome {
            SENT.fetch_add(1, Ordering::SeqCst);
            Ok(numbers
                .iter()
                .map(|number| (*number, Value::from(*number * 10)))
                .collect())
        };
        let cancel = AtomicBool::new(false);
        let threads = (0..4u64)
            .map(|index| {
                std::thread::spawn(move || {
                    let cancel = AtomicBool::new(false);
                    submit(
                        "batch-test",
                        Class::Interactive,
                        &[index * 2, index * 2 + 1],
                        &cancel,
                        &send,
                    )
                    .unwrap()
                })
            })
            .collect::<Vec<_>>();
        for (index, thread) in threads.into_iter().enumerate() {
            let answers = thread.join().unwrap();
            assert_eq!(answers.len(), 2);
            assert_eq!(answers[&(index as u64 * 2)], Value::from(index as u64 * 20));
        }
        assert!(SENT.load(Ordering::SeqCst) <= 2, "{SENT:?}");
        SENT.store(0, Ordering::SeqCst);
        let many = (0..60).collect::<Vec<u64>>();
        let answers = submit("batch-test-many", Class::Background, &many, &cancel, &send).unwrap();
        assert_eq!(answers.len(), 60);
        assert_eq!(SENT.load(Ordering::SeqCst), 3);
    }
}
