---
name: epic-supervision
description: Supervise a GenAImage roadmap epic by dispatching developer and reviewer subagents, tracking progress, opening pull requests and merging them. Use when asked to complete an epic or a batch of sub-issues as a supervisor who does not write code.
---

# Epic supervision

In this role you plan, dispatch, decide and merge. You do not edit repository
code yourself. This procedure has been used to deliver several epics. Its
learnings come from what slowed those epics down.

## 1. Plan

1. Read the epic and its native sub-issues:
   `gh api repos/eemaldi/genaimage/issues/<epic>/sub_issues`. Then read each
   sub-issue body. Check that every external "Blocked by" issue is closed.
2. Before planning pages or features, compare the issue scope with what the
   current API and core actually provide, using `docs/api/openapi.json` and the
   core. Missing backend capabilities are the main cause of late scope growth.
3. Ask the user only about decisions that change the design. Examples are
   routing style, whether a page PR may include small API changes, how many
   developers run in parallel, and whether to close the epic at the end.
4. Track each work item as a todo with dependencies. Run independent items in
   waves of at most three. Split hard and very hard issues into Easy or Medium
   components (see section 2).
5. If the user approves a new runtime dependency, make the approval depend on
   measuring the release binary first. In epic 7, `jsonschema` took the binary
   over the 25 MiB budget even without default features. It was only replaced
   by a small hand-written validator after the runtime was built on it.
6. When parallel branches each add a SQLite migration, have each add one
   self-contained entry to `MIGRATIONS`. The branch merged second renumbers
   its entry and its upgrade test. Unshipped migrations on one branch are
   merged into a single migration before its PR.
7. Never check out a branch in your own worktree. Inspect other refs with
   `git show origin/main:<path>`, `git ls-tree` or `git -C <worktree>`.

## 2. Pick a model tier

Use the user's pool if they gave one. The usual pool is:

| Difficulty | Model | Reasoning |
| --- | --- | --- |
| Easy | `gpt-6-luna` | `max` |
| Medium | `mai-code-2` | `high` |
| Hard and very hard | No single model. Break the task down. | — |

Use the `long_context` tier whenever the chosen model supports it.

You must not give a hard or very hard task to a single subagent. Break it into
simpler components that are Easy or Medium. Give each component its own
subagent, with a clear contract between components. Run each component's tasks
in order on the same branch, or on separate branches if they are truly
independent. Here are splits that worked:

- Backend prerequisite, then UI.
- A form's widgets, then review and submit.
- Core and server, then the client and portal.

Give anything that touches run submission, reconciliation, idempotency,
provenance or numeric exactness to Medium, never Easy. Review it at Medium.

Reviewers work at the developer's tier or above, which in practice means
`mai-code-2` at `high`. If a reviewer is asked to review a whole split feature,
point it at the integrated branch.

## 3. Write the developer prompt

Every developer prompt contains the following:

- **Setup:** `git -C <main checkout> fetch origin`, then
  `git worktree add <repo>.worktrees/epic<N>-<issue> -b epic<N>/<issue>-<slug> origin/main`.
  Tell the developer to work only in that worktree.
- **Required reading:** `AGENTS.md`, ADR 0001, `docs/architecture.md`, the
  issue, and the exact files and helpers it should reuse.
- **Parallel work:** which other developers are active and which directories
  they own, so edits to shared files stay minimal.
- **Scope:** the issue checklist, plus concrete guardrail expectations and the
  tests that must exist.
- **Delivery:**
  - `python3 scripts/ci_check.py` must pass.
  - Commits are conventional, with real newlines (no literal `\n`).
  - Each commit ends with the trailer
    `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`.
  - Push the branch, but do not open a PR or merge.
- **Final reply:** SHAs, a summary, any public helpers or contracts that later
  issues will use, the exact validation results, the JS size and any caveats.

When later issues depend on an earlier one, ask the earlier developer to report
the contracts it created. Examples are URL formats such as
`#/generate?workflow=<id>` and helper exports such as `buildGenerateUrl(run)`.
Pass those contracts verbatim to the dependent developers.

Once a branch is approved and only waiting to merge, dependent work can start
from it to save a wave. Tell the developer to branch from `origin/<branch>`,
then after the merge run `git rebase --onto origin/main <old head> HEAD` and
confirm that `git log origin/main..HEAD` contains only its own commits.

When a merge or a decision changes a shared contract, send a heads-up to every
in-flight developer it affects. For example, `LlmRoles::text()` stopped
requiring tool support, so tool callers must call `check_request`.

## 4. Review loop

1. Start a `code-review` agent on `git diff origin/main...<branch>`. Give it
   the guardrails to focus on and require a verdict of `APPROVE` or
   `CHANGES_REQUESTED`, with severity and `file:line` for each finding.
2. Send the findings back to the **same** developer with `write_agent`, and
   state a decision on every finding:
   - **Accept** real correctness or guardrail problems.
   - **Overrule** with a reason when a finding conflicts with project direction
     or a decision already made. For example, raw provenance is not redacted in
     a loopback-only tool, and Rust source compatibility is not kept for the
     internal `genaimage-api` crate.
   - **Spin off** a backend prerequisite to a separate developer and PR when a
     UI finding can only be fixed server-side. Examples are a re-run endpoint,
     workflow revision pinning and explicit reconcile outcomes.
3. Return the fix to the **same** reviewer, which keeps its context. Name your
   decisions so it does not re-raise overruled points.
4. After about three rounds on one branch, switch the reviewer to a High-only
   rule. Block only on silent substitution, data loss, false success, duplicate
   or blind submission, or mutation of the source record. Record Medium and Low
   findings as follow-ups.
5. When a split feature adds a new component to a branch, such as the server
   half after the core, give it to the same reviewer under full rules again.
   Apply the High-only rule only to fix deltas.
6. If you lean towards a design change, put it to the reviewer as a
   "supervisor note to evaluate" rather than ordering it. In epic 7, this
   settled base64 image content, a distinct `awaiting_confirmation` event and
   preview tokens before any client depended on the old form.
7. Overrule a High finding only when the path fails closed. Write down why,
   and record the issue as a follow-up in the PR. Example: a race between a
   preview check and the claim, where `submit_run` re-validates the pinned
   revision and nothing can be submitted wrongly.

## 5. Open the PR and merge

1. Fill in `.github/pull_request_template.md`. Include
   `Closes #<issue>` (or `Refs #<issue>` if the issue must stay open),
   `Part of #<epic>`, each exact validation command with its result, the
   checklist, and any decision you overruled or recorded as a follow-up.
2. Run `ship_pr.sh <branch> <issue> "<title>" <body-file>` from this skill's
   directory. It:
   - refuses a branch that is behind `origin/main`, or a body that doesn't
     reference the issue;
   - opens the PR, waits 20 seconds and then watches the checks (starting
     the watch immediately can report "no checks" and exit);
   - merges with `--merge` only if every check passes;
   - reports the issue state and the new `origin/main`.

   Close the issue with a comment if GitHub has not.
3. Tell every open branch that main has moved, and what the merge changed
   (migration numbers, shared modules, generated files).

## 6. Rebase rules for developers

- Shared generated files conflict on almost every merge: `docs/api/openapi.json`
  and `apps/portal/src/lib/api/schema.d.ts`. Always regenerate them with
  `cargo run -p genaimage-server --no-default-features --example openapi` and
  `python3 scripts/generate_portal_types.py`. Never merge them by hand.
- A change to shared UI primitives (for example the `Dialog` focus trap) must
  keep earlier regression tests passing.

## 7. Recovering after an interruption

Background agents can disappear if the session is interrupted. `read_agent`
then reports "No agent found".

1. Run `list_agents`. For each in-flight item, check its worktree with
   `git status -sb` and `git log`, and check the remote branch.
2. Start fresh agents with a **resume** prompt. It describes the remote SHA,
   the uncommitted local changes and any half-finished rebase. It tells the
   agent to keep the correct work, delete build leftovers such as
   `*.o`/`*.res`, and finish the remaining task.
3. If a reviewer was lost, start a new one with a summary of the earlier
   decisions.

## 8. Close the epic

1. Check that every sub-issue is closed.
2. Run `python3 scripts/ci_check.py` and `python3 scripts/perf_check.py --quick`
   on `origin/main`, in a detached worktree
   (`git worktree add --detach <repo>.worktrees/epic<N>-final origin/main`).
   Run them yourself with `bash` and log to files, not in a synchronous
   subagent, so an interruption doesn't lose the results.
3. Comment on the epic with the PRs, the final checks, the budgets, the key
   decisions and the follow-ups. Do this even if someone else closed the epic
   first, then close it if it is still open.
4. Remove the epic worktrees. Removing them is slow because `node_modules` and
   `target` are large. Also delete untracked `apps/` build leftovers from
   older branches.
5. Give the user a summary that includes every supervisor decision they may
   want to revisit.

## Learnings

- Page issues often hide backend prerequisites. In epic 6, Jobs needed explicit
  reconcile outcomes and a server-side re-run, and Generate needed workflow
  revision pinning. Check this before dispatching, not after review.
- The UI must never rebuild a submission from current workflow data. Re-run is
  a server operation that verifies fingerprints, and Generate pins
  `workflow_revision`.
- Reviewers repeatedly found the same classes of bug: unbounded fetches,
  stale events overwriting newer snapshots, lost focus in dialogs, and lossy
  number handling (large seeds, `2.0` versus `2`, `-0.0`). Name these classes
  in the developer prompt from the start.
- Epic 7 (assistant and LLM work) added these classes. Name them in prompts
  for similar work:
  - **Secrets:** derived `Debug` on any type holding a key, provider error
    bodies that echo the key, and secrets in settings responses.
  - **Exact numbers in validation:** bounds and `enum` equality compared
    through `f64`. This was found three times: in the tool schema, `enum`
    equality and workflow bounds.
  - **False success:**
    - a reused `done` event for a paused turn;
    - a stream that ended without a terminator, treated as complete;
    - `ok` with no `run_id`;
    - persisted failures decoded as success;
    - a 409 hidden by a recovery reload.
  - **Atomicity:** a check done before an atomic claim (TOCTOU); a lease
    released while detached `spawn_blocking` work continues; a panic that
    skips cleanup.
  - **Snapshot drift:** validating against a different manifest than the one
    the model was shown, or values arriving through a preset that bypass a
    filter.
  - **Byte limits:** character limits that exceed the request body limit, and
    bounded item counts with unbounded item sizes.
  - **Weak tests:** expectations copied from the observed output, assertions
    on event names only, and timeouts that don't cover the whole test.
- Ask the reviewer of a confirmation or submission UI to check persisted
  states after a reload, not just the live stream. Both epic 7 UI rounds found
  bugs that appeared only after a reload.
- Tell the user when review rounds are what is slowing progress, and offer the
  High-only rule. Do not wait to be asked why it is slow.
