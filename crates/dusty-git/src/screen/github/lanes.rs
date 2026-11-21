//! The screen's forge lanes: a few workers so a slow read on one tab never
//! holds up another tab's read. Writes stay one at a time and alone: a write starts only when
//! every lane is idle, and no read starts while one runs. Identical reads are joined below
//! (`forge::inflight`) and by key here (`Workspace::pending`).

use super::Key;
use super::Op;
use super::Request;
use crate::forge::ForgeJobs;
use crate::forge::ForgeReply;
use crate::forge::provider::Conditional;
use std::path::PathBuf;

/// Reads the screen runs at once (`DUSTY_FORGE_LANES` overrides it; 1 is one at a time).
pub(super) const LANES: usize = 4;

fn lane_count() -> usize {
    std::env::var("DUSTY_FORGE_LANES")
        .ok()
        .and_then(|lanes| lanes.parse::<usize>().ok())
        .map_or(LANES, |lanes| lanes.clamp(1, 16))
}

/// The host, repository and scope epoch a request was started under.
pub(super) type Scope = (String, String, u64);

pub(super) struct Running {
    pub request: Request,
    pub scope: Scope,
    id: u64,
}

pub(super) struct Lanes {
    jobs: Vec<ForgeJobs>,
    running: Vec<Option<Running>>,
    next: u64,
    /// Tests: requests are recorded as running and never sent.
    #[cfg(test)]
    dry: bool,
}

impl Default for Lanes {
    fn default() -> Self {
        let lanes = lane_count();
        Self {
            jobs: (0..lanes).map(|_| ForgeJobs::default()).collect(),
            running: (0..lanes).map(|_| None).collect(),
            next: 0,
            #[cfg(test)]
            dry: false,
        }
    }
}

impl Lanes {
    /// Nothing the screen still waits for is running (a cancelled read may still hold its
    /// worker until its request returns).
    pub fn count(&self) -> usize {
        self.running.len()
    }

    /// Free lanes a background check leaves to the reads somebody waits for.
    pub fn reserve(&self) -> usize {
        usize::from(self.count() > 1)
    }

    pub fn idle(&self) -> bool {
        self.running.iter().all(Option::is_none)
    }

    pub fn requests(&self) -> impl Iterator<Item = &Request> {
        self.running
            .iter()
            .flatten()
            .map(|running| &running.request)
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.requests().any(|request| &request.key == key)
    }

    pub fn writing(&self) -> bool {
        self.requests()
            .any(|request| matches!(request.op, Op::Write(..)))
    }

    /// Lanes a new request could start on now.
    pub fn free(&self) -> usize {
        self.running
            .iter()
            .zip(&self.jobs)
            .filter(|(running, jobs)| running.is_none() && !jobs.busy())
            .count()
    }

    /// Start `request` on a free lane; a `background` read's requests yield to the others'.
    #[allow(clippy::result_large_err)] // the refused request goes back to its queue whole
    pub fn start(
        &mut self,
        root: PathBuf,
        request: Request,
        conditional: Conditional,
        scope: Scope,
        background: bool,
    ) -> Result<(), (Request, String)> {
        let Some(lane) =
            (0..self.count()).find(|&lane| self.running[lane].is_none() && !self.jobs[lane].busy())
        else {
            return Err((request, "a forge request is already running".into()));
        };
        self.next += 1;
        let id = self.next;
        #[cfg(test)]
        if self.dry {
            self.running[lane] = Some(Running { request, scope, id });
            return Ok(());
        }
        let started = match &request.op {
            Op::Read(target, read) => self.jobs[lane].fetch_as(
                root,
                id,
                target.clone(),
                read.clone(),
                conditional,
                background,
            ),
            Op::Write(target, write) => {
                self.jobs[lane].write(root, id, target.clone(), write.clone())
            }
        };
        match started {
            Ok(()) => {
                self.running[lane] = Some(Running { request, scope, id });
                Ok(())
            }
            Err(error) => Err((request, error)),
        }
    }

    /// Abandon the running request for `key`: its answer is dropped when it comes.
    pub fn cancel(&mut self, key: &Key) -> Option<Request> {
        let lane = (0..self.count()).find(|&lane| {
            self.running[lane]
                .as_ref()
                .is_some_and(|running| &running.request.key == key)
        })?;
        self.jobs[lane].cancel();
        self.running[lane].take().map(|running| running.request)
    }

    /// Finished requests, with their answers; answers to abandoned requests are dropped.
    pub fn replies(&mut self) -> Vec<(Running, ForgeReply)> {
        let mut done = Vec::new();
        for lane in 0..self.count() {
            while let Some(event) = self.jobs[lane].try_reply() {
                if self.running[lane]
                    .as_ref()
                    .is_some_and(|running| running.id == event.request_id)
                    && let Some(running) = self.running[lane].take()
                {
                    done.push((running, event.reply));
                }
            }
        }
        done
    }

    /// Lanes that record what starts instead of sending it.
    #[cfg(test)]
    pub fn dry() -> Self {
        Self::dry_with(LANES)
    }

    /// `count` dry lanes.
    #[cfg(test)]
    pub fn dry_with(count: usize) -> Self {
        Self {
            jobs: (0..count).map(|_| ForgeJobs::default()).collect(),
            running: (0..count).map(|_| None).collect(),
            next: 0,
            dry: true,
        }
    }

    /// A dry lane's request answered: its lane is free again.
    #[cfg(test)]
    pub fn finish(&mut self, key: &Key) -> Option<Request> {
        let lane = (0..self.count()).find(|&lane| {
            self.running[lane]
                .as_ref()
                .is_some_and(|running| &running.request.key == key)
        })?;
        self.running[lane].take().map(|running| running.request)
    }

    /// A reply as if `request` had been started under `scope` and answered.
    #[cfg(test)]
    pub fn answer_for_test(&mut self, request: Request, scope: Scope, reply: ForgeReply) {
        self.next += 1;
        let id = self.next;
        self.jobs[0].reply_for_test(PathBuf::new(), id, reply);
        self.running[0] = Some(Running { request, scope, id });
    }
}

#[cfg(test)]
mod tests {
    use super::super::Workspace;
    use super::*;
    use crate::forge::ForgeTarget;
    use crate::forge::model::ItemKind;
    use crate::forge::request::{Read, Write};

    fn read(key: Key) -> Request {
        Request {
            key,
            op: Op::Read(ForgeTarget::remote(), Read::Repository),
        }
    }

    fn write(number: u64) -> Request {
        Request {
            key: Key::Write,
            op: Op::Write(
                ForgeTarget::remote(),
                Write::Comment {
                    kind: ItemKind::PullRequest,
                    number,
                    body: "hello".into(),
                },
            ),
        }
    }

    fn workspace() -> Workspace {
        let mut workspace = Workspace::new(PathBuf::new());
        workspace.active = Lanes::dry();
        workspace
    }

    fn running(workspace: &Workspace) -> Vec<Key> {
        workspace
            .active
            .requests()
            .map(|request| request.key.clone())
            .collect()
    }

    #[test]
    fn reads_run_side_by_side_up_to_the_lane_count() {
        let mut workspace = workspace();
        for id in 1..=6 {
            workspace.queue.push_back(read(Key::Item(2, id)));
        }
        workspace.start_reads();
        assert_eq!(running(&workspace).len(), LANES);
        assert_eq!(workspace.queue.len(), 6 - LANES);
        // One answer frees one lane, and the next read takes it.
        workspace.active.finish(&Key::Item(2, 1));
        workspace.start_reads();
        assert_eq!(running(&workspace).len(), LANES);
        assert_eq!(workspace.queue.len(), 6 - LANES - 1);
    }

    #[test]
    fn background_checks_leave_a_lane_for_the_read_the_user_waits_for() {
        let mut workspace = workspace();
        for id in 1..=6 {
            workspace.fresh.implicit.insert(Key::Item(2, id));
            workspace.queue.push_back(read(Key::Item(2, id)));
        }
        workspace.start_reads();
        assert_eq!(running(&workspace).len(), LANES - 1, "one lane stays free");
        // What the user opens starts at once, behind the queued checks.
        workspace.queue.push_back(read(Key::Workflows));
        workspace.start_reads();
        assert!(workspace.active.contains(&Key::Workflows));
        assert_eq!(workspace.active.free(), 0);
    }

    #[test]
    fn with_one_lane_the_open_items_read_goes_before_its_timeline_and_any_check() {
        let mut workspace = Workspace::new(PathBuf::new());
        workspace.active = Lanes::dry_with(1);
        workspace.page = 1;
        workspace.opened = Some((1, 7));
        // Queued in the worst order: a cached entry's check, the timeline, then the item.
        workspace.fresh.implicit.insert(Key::Repository);
        workspace.queue.push_back(read(Key::Repository));
        workspace.queue.push_back(read(Key::IssueTimeline(7)));
        workspace.queue.push_back(read(Key::Item(1, 7)));
        workspace.reconcile_reads();
        let mut order = Vec::new();
        for _ in 0..3 {
            workspace.start_reads();
            let started = running(&workspace);
            assert_eq!(started.len(), 1, "one lane, one read");
            workspace.active.finish(&started[0]);
            order.push(started[0].clone());
        }
        assert_eq!(
            order,
            vec![Key::Item(1, 7), Key::IssueTimeline(7), Key::Repository]
        );
    }

    #[test]
    fn leaving_an_item_drops_its_queued_reads_and_abandons_its_running_one() {
        let mut workspace = Workspace::new(PathBuf::new());
        workspace.active = Lanes::dry_with(1);
        workspace.page = 1;
        workspace.opened = Some((1, 7));
        workspace.queue.push_back(read(Key::IssueTimeline(7)));
        workspace.queue.push_back(read(Key::Item(1, 7)));
        workspace.reconcile_reads();
        workspace.start_reads();
        assert_eq!(running(&workspace), vec![Key::Item(1, 7)]);
        // The user moves to #8: #7's timeline is dropped and its item read abandoned.
        workspace.opened = Some((1, 8));
        workspace.queue.push_back(read(Key::Item(1, 8)));
        workspace.reconcile_reads();
        assert!(running(&workspace).is_empty());
        assert_eq!(
            workspace
                .queue
                .iter()
                .map(|request| request.key.clone())
                .collect::<Vec<_>>(),
            vec![Key::Item(1, 8)]
        );
        workspace.start_reads();
        assert_eq!(running(&workspace), vec![Key::Item(1, 8)]);
    }

    #[test]
    fn a_write_runs_alone_and_never_overlaps_another_write() {
        let mut workspace = workspace();
        workspace.queue.push_back(read(Key::Item(2, 1)));
        workspace.start_reads();
        // A write waits for the running read, and nothing behind it starts meanwhile.
        workspace.queue.push_back(write(1));
        workspace.queue.push_back(write(2));
        workspace.queue.push_back(read(Key::Item(2, 2)));
        workspace.start_reads();
        assert_eq!(running(&workspace), vec![Key::Item(2, 1)]);
        workspace.active.finish(&Key::Item(2, 1));
        workspace.start_reads();
        assert_eq!(
            running(&workspace),
            vec![Key::Write],
            "the first write, alone"
        );
        assert_eq!(workspace.queue.len(), 2);
        workspace.start_reads();
        assert_eq!(
            running(&workspace).len(),
            1,
            "the second write waits for the first"
        );
        let first = workspace.active.finish(&Key::Write).unwrap();
        assert!(matches!(
            first.op,
            Op::Write(_, Write::Comment { number: 1, .. })
        ));
        workspace.start_reads();
        assert_eq!(running(&workspace), vec![Key::Write]);
        assert!(workspace.active.writing());
        let second = workspace.active.finish(&Key::Write).unwrap();
        assert!(matches!(
            second.op,
            Op::Write(_, Write::Comment { number: 2, .. })
        ));
        workspace.start_reads();
        assert_eq!(running(&workspace), vec![Key::Item(2, 2)]);
    }
}
