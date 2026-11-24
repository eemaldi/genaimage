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

## 5. Open the PR and merge

1. Check that the branch is current with
   `git merge-base --is-ancestor origin/main origin/<branch>`. If it is not,
   ask the developer to rebase.
2. Fill in `.github/pull_request_template.md`. Include
   `Closes #<issue>` (or `Refs #<issue>` if the issue must stay open),
   `Part of #<epic>`, each exact validation command with its result, and the
   checklist.
3. `gh pr create`, then wait about 20 seconds before running
   `gh pr checks <pr> --watch`. Starting the watch immediately can report "no
   checks" and exit.
4. Merge with `gh pr merge <pr> --merge`. Check the issue is closed and close it
   with a comment if GitHub has not.
5. Tell every open branch that main has moved.

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
   on `origin/main`.
3. Close the epic with a comment listing the PRs, the final checks and the
   budgets.
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
- Tell the user when review rounds are what is slowing progress, and offer the
  High-only rule. Do not wait to be asked why it is slow.
