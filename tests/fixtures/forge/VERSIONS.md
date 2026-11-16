# Forge fixtures: hosts, versions, probes

Recorded 2026-11-14 with `scripts/forge-record` (GET and read-only GraphQL queries only, ≤ 1 request/s
per host, ≤ 2 pages per list, text bodies capped at 256 KiB). Every fixture passes
`scripts/forge-record <provider> x x/y --out tests/fixtures/forge/<provider>/ --check`.

## Hosts

Only `gitea/` and `forgejo/versions/17/` are kept: `server.py --world gitea|forgejo` answers from
them. The `github/` and `gitlab/` recordings were dropped once the `legacy`, `inbox`, `actions` and
`gitlab` worlds replaced them (no test loaded them); the rows below still say what was recorded, and
`scripts/forge-record` makes them again. Recorded entries no test or `forge-gitea` scenario requested
were dropped too, and nested repository objects keep only their identity fields (`id`, `owner`,
`name`, `full_name`, `private`, `fork`, `parent`, `html_url`, `clone_url`, `ssh_url`,
`default_branch`).

| Dir | Host | Server version | Recorded at | Source repo → redacted | Auth |
|---|---|---|---|---|---|
| `github/` | github.com (`https://api.github.com`) | n/a (no version endpoint) | 2026-11-14T05:20:02Z | `charmbracelet/bubbletea` → `orbit/orbit` | gh token, held in memory, reads only |
| `gitlab/` | gitlab.com (`/api/v4`, `/api/graphql`) | not visible anonymously (`/version` 401, GraphQL `metadata` null) | 2026-11-14T05:31:33Z | `gitlab-org/charts/gitlab-runner` → `orbit/team/orbit` | anonymous |
| `gitea/` | gitea.com (`/api/v1`) | `29.0.0+dev-160-g55186ca39b` | 2026-11-14T05:15:56Z | `gitea/runner` → `orbit/orbit` | anonymous |
| `forgejo/versions/17/` | code.forgejo.org (`/api/v1`) | `17.0.0-26-363f3dc30b+gitea-1.22.0` | 2026-11-14T05:28:29Z | `forgejo/runner` → `orbit/orbit` | anonymous |

Hosts not recorded:

- **forgejo.example.org**: skipped. Every anonymous API read, including `/api/v1/version`, answers
  `403 {"message":"Only signed in user is allowed to call APIs."}` (re-checked 2026-11-14).
  Recording it needs a token; F0 used none.
- **codeberg.org** (Forgejo 17.0.0-100): `/api/v1/version` answers, but
  `GET /repos/forgejo/forgejo/actions/runs` returned 504 after ~30 s on every try, so the Forgejo
  fixture comes from code.forgejo.org (same major). Codeberg's live swagger is still used as
  evidence below.
- **Forgejo 7–16**: no reachable public instance per major; the [p] table uses each release's
  swagger instead (`templates/swagger/v1_json.tmpl` on Forgejo `v7.0`–`v12.0` branches,
  published `swagger.v1.json` for 13.0.5, 14.0.5, 15.0.11, 16.0.7; live 17.0.0/17.0.1).
  Gitea uses `release/v1.20`–`release/v1.27` swagger templates plus gitea.com's live
  `29.0.0+dev` swagger.

Forgejo 13–17 all report `X.Y.Z…+gitea-1.22.0`: gate on the Forgejo major, never on
the `+gitea-` suffix. Gitea's main branch now reports `29.0.0+dev` (major-only numbering).

## Redaction

Logins → cast `maya, alex, sam, leo, nina` (then `maya2`, …), display names → capitalised cast,
emails → `<login>@example.invalid`, avatars → `https://<host>/avatars/<login>.png`, the source
repo → `orbit/orbit` (`orbit/team/orbit` on GitLab), timestamps shifted so the newest equals
`recorded_at − 9 min`. Bot accounts such as `codecov` keep their names. `requests.json` → `redaction.hashes` holds sha256 of every replaced identifier;
`--check` fails if any of them reappears.

## [p] resolutions (FORGE.md §4.2)

"≥ N" means the endpoint first appears in that release's swagger; "anon" is what the recorded host
answered without a token.

### Gitea

| Cell | Endpoint | Resolution | Evidence |
|---|---|---|---|
| Workflows rail | `GET actions/workflows` | **≥ 1.24** (as written) | swagger 1.24+; gitea.com 401 anon |
| Runs list | `GET actions/runs` | **≥ 1.25** (FORGE says 1.24 — wrong) | absent from 1.24 swagger; gitea.com 401 anon |
| Runs fallback | `GET actions/tasks` | **≥ 1.23** (FORGE says 1.22 — wrong) | swagger; gitea.com 200 anon |
| Run → jobs | `GET actions/runs/{run}`, `/jobs` | **≥ 1.25** (FORGE says 1.24) | swagger; gitea.com 200 anon |
| Single job | `GET actions/jobs/{job}` | **≥ 1.25** | swagger; gitea.com 401 anon |
| Job log | `GET actions/jobs/{job}/logs` | **≥ 1.24** (as written) | swagger; gitea.com 401 anon |
| Rerun all / failed | `POST actions/runs/{run}/rerun`, `/rerun-failed-jobs` | **≥ 1.26** | swagger |
| Rerun one job | `POST actions/runs/{run}/jobs/{job}/rerun` | **≥ 1.26**; path is under the run, not `actions/jobs/{job}/rerun` | swagger |
| Cancel | `POST actions/runs/{run}/cancel` | **No** in 1.20–1.27; only main (`29.0.0+dev`) | swagger |
| Approve fork run | `POST actions/runs/{run}/approve` | **No** before 29 (main only) | swagger |
| Attempts | `GET actions/runs/{run}/attempts/{n}` | **≥ 1.27** | swagger |
| Dispatch | `POST actions/workflows/{id}/dispatches` | **≥ 1.24** (FORGE says 1.23 — wrong) | swagger |
| Artifacts | `GET actions/runs/{run}/artifacts`, `actions/artifacts/{id}/zip` | **≥ 1.24** (as written) | swagger; gitea.com 200 anon |
| PR for branch | `GET pulls/{base}/{head}` | **≥ 1.22**; base-repo heads only (fork heads 404) | swagger; recorded 404 + 200 |

### Forgejo

| Cell | Endpoint | Resolution | Evidence |
|---|---|---|---|
| Workflows rail | `GET actions/workflows` | **No** in 7–17 (hide the rail; group runs by `workflow_id`) | swagger; code.forgejo.org 404 |
| Runs list | `GET actions/runs` | **≥ 12.0** | swagger 12.0 branch; 200 anon |
| Runs fallback | `GET actions/tasks` | **≥ 8.0** (FORGE says 7.0 — wrong) | swagger; 200 anon |
| Run | `GET actions/runs/{run}` | **≥ 12.0** | swagger; 200 anon |
| Run → jobs | `GET actions/runs/{run}/jobs` | **≥ 16.0** (bare list) | swagger; 200 anon |
| Single job | `GET actions/jobs/{job}` | **≥ 17.0** | swagger; 200 anon |
| Job log | `GET actions/jobs/{job}/logs` | **≥ 16.0**; a job that never ran answers `500 {"message":""}` | swagger; recorded 500 + 200 |
| Run log archive | `GET actions/runs/{run}/logs` | ≥ 16.0 (not used by FORGE.md) | swagger |
| Rerun all | `POST actions/runs/{run}/rerun` | **≥ 17.0**; no `rerun-failed-jobs` | swagger |
| Rerun one job | `POST actions/jobs/{job}/rerun` | **≥ 17.0** | swagger |
| Cancel | `POST actions/runs/{run}/cancel` | **≥ 16.0** | swagger |
| Dispatch | `POST actions/workflows/{file}/dispatches` | **≥ 8.0** (FORGE says 10.0 — earlier than written) | swagger |
| Artifacts | `GET actions/runs/{run}/artifacts`, `actions/artifacts/{id}/zip` | **≥ 16.0** | swagger; 200 anon |
| PR for branch | `GET pulls/{base}/{head}` | ≥ 7.0; fork heads 404 | swagger; recorded 404 |

### GitLab

| Cell | Resolution | Evidence |
|---|---|---|
| Request changes | **Yes, GraphQL only**: `mergeRequestRequestChanges(input: {projectPath, iid})` (undo: `mergeRequestDestroyRequestedChanges`). There is no REST write. Reviewer state reads via REST `GET merge_requests/{iid}/reviewers` → `state` (`unreviewed`, `reviewed`, `requested_changes`, `approved`, `unapproved`), 200 anon. Probe by introspecting `Mutation.mergeRequestRequestChanges`, not by version (gitlab.com hides its version from anonymous callers). | gitlab.com GraphQL introspection; `MergeRequestReviewState.REQUESTED_CHANGES`, `DetailedMergeStatus.REQUESTED_CHANGES`; recorded `pull-request-*.json` (`part: reviewers`); https://docs.gitlab.com/api/graphql/reference/#mutationmergerequestrequestchanges |

## Synthesized entries

Writes are never sent to a real forge, and some reads need a token. These entries are written by
hand from the official docs (`"synthesized": true`, `doc` holds the URL). Where the same request
was also recorded anonymously as 401, the synthesized one carries `"auth": "token"` and the
recorded one `"auth": "none"`.

| Provider | Read/Write | Request | Doc |
|---|---|---|---|
| github | Viewer | `GET /user` | https://docs.github.com/en/rest/users/users#get-the-authenticated-user |
| github | Notifications | `GET /notifications` | https://docs.github.com/en/rest/activity/notifications#list-notifications-for-the-authenticated-user |
| github | Comment | `POST issues/13/comments` | https://docs.github.com/en/rest/issues/comments#create-an-issue-comment |
| github | SetLabels, SetState | `PATCH issues/13` | https://docs.github.com/en/rest/issues/issues#update-an-issue |
| github | SubmitReview | `POST pulls/13/reviews` | https://docs.github.com/en/rest/pulls/reviews#create-a-review-for-a-pull-request |
| github | Merge (200, 409) | `PUT pulls/13/merge` | https://docs.github.com/en/rest/pulls/pulls#merge-a-pull-request |
| github | InlineComment | `POST pulls/13/comments` | https://docs.github.com/en/rest/pulls/comments#create-a-review-comment-for-a-pull-request |
| github | Reply | GraphQL `addPullRequestReviewThreadReply` | https://docs.github.com/en/graphql/reference/mutations#addpullrequestreviewthreadreply |
| github | Rerun | `POST actions/runs/91/rerun`, `/rerun-failed-jobs` | https://docs.github.com/en/rest/actions/workflow-runs#re-run-a-workflow |
| github | CancelRun | `POST actions/runs/91/cancel` | https://docs.github.com/en/rest/actions/workflow-runs#cancel-a-workflow-run |
| github | Dispatch | `POST actions/workflows/31/dispatches` | https://docs.github.com/en/rest/actions/workflows#create-a-workflow-dispatch-event |
| github | MarkRead | `PATCH /notifications/threads/9001` | https://docs.github.com/en/rest/activity/notifications#mark-a-thread-as-read |
| gitlab | Viewer, Notifications (token) | `GET /user`, `GET /todos` | https://docs.gitlab.com/api/users/#retrieve-the-current-user, https://docs.gitlab.com/api/todos/#list-all-to-do-items |
| gitlab | Version, Labels, Milestones, Collaborators (token) | `GET /version`, `labels`, `milestones`, `members/all` | https://docs.gitlab.com/api/version/, https://docs.gitlab.com/api/labels/#list-labels, https://docs.gitlab.com/api/milestones/#list-project-milestones, https://docs.gitlab.com/api/members/ |
| gitlab | Threads, Timeline notes, Issue notes (token) | `GET merge_requests/13/discussions`, `/notes`, `issues/13/notes` | https://docs.gitlab.com/api/discussions/, https://docs.gitlab.com/api/notes/ |
| gitlab | Job, JobLog (token) | `GET jobs/42`, `jobs/42/trace` | https://docs.gitlab.com/api/jobs/#retrieve-a-single-job, https://docs.gitlab.com/api/jobs/#get-a-log-file |
| gitlab | Comment | `POST merge_requests/13/notes` | https://docs.gitlab.com/api/notes/#create-new-merge-request-note |
| gitlab | SetLabels, SetState | `PUT merge_requests/13` | https://docs.gitlab.com/api/merge_requests/#update-a-merge-request |
| gitlab | SubmitReview | `POST merge_requests/13/approve`; GraphQL `mergeRequestRequestChanges` | https://docs.gitlab.com/api/merge_request_approvals/#approve-merge-request, https://docs.gitlab.com/api/graphql/reference/#mutationmergerequestrequestchanges |
| gitlab | Merge (200, 409) | `PUT merge_requests/13/merge` | https://docs.gitlab.com/api/merge_requests/#merge-a-merge-request |
| gitlab | InlineComment, Reply | `POST merge_requests/13/discussions`, `…/discussions/{id}/notes` | https://docs.gitlab.com/api/discussions/ |
| gitlab | Rerun, CancelRun, Dispatch | `POST pipelines/91/retry`, `/cancel`, `POST pipeline` | https://docs.gitlab.com/api/pipelines/ |
| gitlab | MarkRead | `POST /todos/9001/mark_as_done` | https://docs.gitlab.com/api/todos/#mark-a-to-do-item-as-done |
| gitea, forgejo | Viewer, Notifications (token) | `GET /user`, `GET /notifications` | `https://<host>/api/swagger#/user/userGetCurrent`, `#/notification/notifyGetList` |
| gitea | Workflows, Runs (token) | `GET actions/workflows`, `actions/runs` | https://gitea.com/api/swagger#/repository/ActionsListRepositoryWorkflows, `#/repository/getWorkflowRuns` |
| gitea | Run, Run jobs, Job, JobLog, Artifacts (GitHub-shaped samples) | `GET actions/runs/91`, `/jobs`, `actions/jobs/42`, `/logs`, `actions/runs/91/artifacts` | https://gitea.com/api/swagger#/repository |
| gitea, forgejo | Comment | `POST issues/13/comments` | `#/issue/issueCreateComment` |
| gitea, forgejo | SetLabels | `PUT issues/13/labels` | `#/issue/issueReplaceLabels` |
| gitea, forgejo | SetState | `PATCH issues/13` | `#/issue/issueEditIssue` |
| gitea, forgejo | SubmitReview, InlineComment, Reply | `POST pulls/13/reviews` | `#/repository/repoCreatePullReview` |
| gitea, forgejo | Merge (200, 409) | `POST pulls/13/merge` | `#/repository/repoMergePullRequest` |
| gitea, forgejo | Rerun, CancelRun | `POST actions/runs/91/rerun`, `/cancel` | `#/repository/rerunWorkflowRun` (Forgejo: `RerunWorkflowRun`), `#/repository/cancelWorkflowRun` |
| gitea, forgejo | Dispatch | `POST actions/workflows/ci.yml/dispatches` | `#/repository/ActionsDispatchWorkflow` |
| gitea, forgejo | MarkRead | `PATCH /notifications/threads/9001?to-status=read` | `#/notification/notifyReadThread` |
| gitea, forgejo | List (closed) | `GET issues?state=closed&type=issues`, `GET pulls?state=closed`: three closed issues; two closed and one merged pull request, made from the first recorded open row (the list header's `Closed` side) | `#/issue/issueListIssues`, `#/repository/repoListPullRequests` |

Gitea/Forgejo doc anchors are relative to `https://gitea.com/api/swagger` and
`https://code.forgejo.org/api/swagger`.

## Behaviour worth knowing

- **gitlab.com refuses anonymous REST reads** of `/version`, `labels`, `milestones`,
  `members/all`, MR/issue `notes`, `discussions`, `resource_*_events`, `jobs/{id}` and
  `jobs/{id}/trace` (401) even on a public project, while MRs, diffs, pipelines and pipeline jobs
  answer. GraphQL serves labels, milestones, discussions (with diff positions) and job trace
  summaries anonymously; the fixture records both (`op=ProjectLabels`, `ProjectMilestones`,
  `MergeRequestThreads`, `PipelineJobs`).
- **A fork MR's head pipeline lives in the source project**: `projects/<target>/pipelines/<id>/jobs`
  is 404, `projects/<head_pipeline.project_id>/pipelines/<id>/jobs` is 200. `merge_requests/{iid}/pipelines`
  lists them from the target project.
- **gitea.com (29 dev) gates part of Actions behind a token**: `actions/workflows`, `actions/runs`,
  `actions/jobs/{id}`, `actions/jobs/{id}/logs` → 401; `actions/tasks`, `actions/runs/{id}`,
  `actions/runs/{id}/jobs`, `actions/runs/{id}/artifacts` → 200.
- **Forgejo's Actions JSON is not GitHub-shaped**. Runs: `{workflow_runs, total_count}` with
  `index_in_repo`, `status` holding the result (`success`, `failure`, …), `commit_sha`,
  `prettyref`, `trigger_user`, `started`/`stopped`/`duration`, `need_approval`, `workflow_id`.
  Jobs: a bare list with `handle`, `runs_on`, `needs`, `attempt`, `task_id`. Artifacts: a bare list.
  Gitea's are GitHub-shaped (`{total_count, jobs}`, `conclusion`, `started_at`, `run_attempt`).
- Forgejo `actions/jobs/{id}/logs` answers 500 with an empty message for a job that never ran; the
  fixture keeps that response next to a 200 log.

## Headers and pagination

| Host | Kept headers | Pagination |
|---|---|---|
| api.github.com | `content-type`, `etag`, `last-modified`, `link`, `x-ratelimit-{limit,remaining,reset,used,resource}` | `link` with `rel="next"`/`"last"`; next URLs use `/repositories/<numeric id>/…`, not `owner/repo` |
| gitlab.com | `content-type`, `etag`, `link`, `x-page`, `x-next-page` (empty on the last page), `x-total`, `x-total-pages`, `ratelimit-{limit,name,observed,remaining,reset}` | `link` (adds `id=` and the server's default params); `x-total`/`x-total-pages` omitted on very large sets (global `/merge_requests`); anonymous `ratelimit-name: throttle_unauthenticated_api`, limit 500 |
| gitea.com | `content-type`, `link`, `x-total-count` | `link` with `limit=`; `x-total-count` on lists |
| code.forgejo.org | `content-type`, `link`, `x-total-count` (`last-modified` on one read) | same as Gitea |

Keys are `METHOD /path?query` with the query sorted and `quote_plus`-encoded, `per_page`/`limit`
dropped; GraphQL keys are `POST <path> op=<Operation>` (GitLab: `/api/graphql`, GitHub: `/graphql`).
GitHub's PR diff and commit diff share a key with their JSON reads and are told apart by
`"accept": "application/vnd.github.diff"`.
