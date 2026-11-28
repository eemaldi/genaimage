---
name: epic-supervision
description: Supervises a GenAImage roadmap epic by dispatching fresh developer and reviewer subagents, tracking progress, opening and merging pull requests, keeping session memory, and ending each session with a retrospective that updates repository instructions and skills. Use when asked to complete an epic or a batch of sub-issues as a supervisor who does not write code.
---

# Epic supervision

In this role you plan, dispatch, decide and merge. You do not edit repository
code yourself. You do maintain the session memory, instruction files and skills.
This procedure has been used to deliver several epics. Its learnings come from
what slowed those epics down.

Copy this checklist and keep it current:

```text
- [ ] Open the session memory and consolidate any left from earlier sessions
- [ ] Plan the epic
- [ ] For each work item: develop, review, open the PR, merge
- [ ] Close the epic
- [ ] Consolidate the session (every session, even if the epic is unfinished)
```

## Subagents are single-use

**Do not resume subagents. Every time a subagent reports, it is finished. If you
need that role again, start a fresh one.** This applies to developers and
reviewers, including review fixes, rebases, follow-up questions and recovery.

A fresh subagent knows nothing about earlier work, so every prompt after the
first one for a work item carries a handoff brief:

- the role, issue, difficulty and exact task;
- the existing worktree, branch and latest SHA;
- the required reading, scope, delivery rules and final reply format from
  section 3;
- the public contracts it depends on, verbatim;
- the latest validation results;
- every review finding so far, with your decision and reason;
- whether `origin/main` has moved, what the merge changed and whether a rebase
  is needed.

Run only one subagent at a time in a worktree.

## Session memory

Save memory throughout the session, as things happen. Do not leave it to the
end, because the session can be interrupted.

Keep one untracked file per session in the main checkout:
`.supervision/memory/<YYYY-MM-DD>-epic<N>.md`. Only you write to it.

- **Work items:** for each one, the worktree, branch, latest SHA, state,
  contracts, validation results and review decisions. Handoff briefs and
  recovery are built from this.
- **Learnings:** one line each, tagged `tool`, `process`, `feedback` or
  `opinion`, with the evidence and the source (user, supervisor or subagent).
  - `tool`: commands, flags and tool behaviour that worked or failed.
  - `process`: what sped the work up or slowed it down.
  - `feedback`: corrections, approvals and preferences from the user, and
    recurring review findings.
  - `opinion`: judgements and suggestions that are not yet verified.

Update it after each user message that steers the work, each subagent report,
each decision and each merge. Never record secrets or personal data. Write paths
as inline code, not Markdown links, because the repository link check also reads
this file.

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

## 2. Pick a model

Use the user's pool if they gave one. Otherwise use only these models, for
developers and reviewers alike:

| Difficulty | Model | Claude Code `model` | GitHub Copilot model ID | Effort |
| --- | --- | --- | --- | --- |
| Easy | Haiku 5.5 | `haiku` | `claude-haiku-5.5` | `medium` |
| Medium | Haiku 5.5 | `haiku` | `claude-haiku-5.5` | `high` |
| Hard | Sonnet 5.5 | `sonnet` | `claude-sonnet-5.5` | `medium` |
| Very hard | Sonnet 5.5 | `sonnet` | `claude-sonnet-5.5` | `high` |

Set the model and the effort explicitly on every dispatch, so that a subagent
does not inherit yours. Use a long-context tier when the client offers one for
the model.

Split large issues into components with a clear contract between them, and
give each component its own subagent. Run the components in order on the same
branch, or on separate branches if they are truly independent. Here are splits
that worked:

- Backend prerequisite, then UI.
- A form's widgets, then review and submit.
- Core and server, then the client and portal.

Treat anything that touches run submission, reconciliation, idempotency,
provenance or numeric exactness as at least Hard, so Sonnet 5.5 develops and
reviews it.

Reviewers work at the developer's difficulty or above, and never below Hard.
If a reviewer is asked to review a whole split feature, point it at the
integrated branch.

## 3. Write the developer prompt

Every developer prompt contains the following:

- **Setup:** for the first developer on a work item,
  `git -C <main checkout> fetch origin`, then
  `git worktree add <repo>.worktrees/epic<N>-<issue> -b epic<N>/<issue>-<slug> origin/main`.
  Later developers use that existing worktree. Tell the developer to work only
  there.
- **Required reading:** `AGENTS.md`, ADR 0001, `docs/architecture.md`, the
  issue, and the exact files and helpers it should reuse.
- **Parallel work:** which other developers are active and which directories
  they own, so edits to shared files stay minimal.
- **Scope:** the issue checklist, plus concrete guardrail expectations and the
  tests that must exist.
- **Delivery:**
  - `python3 scripts/ci_check.py` must pass.
  - Commits are conventional, with real newlines (no literal `\n`).
  - Each commit ends with the co-author trailer that the client in use
    specifies. For GitHub Copilot it is
    `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`.
  - Push the branch, but do not open a PR or merge.
- **Environment:** subagent shells lack `~/.cargo/bin` on `PATH` and have no
  `$TMPDIR`. Give the `export PATH` line and the scratchpad path for logs and
  temporary files, and forbid writing into `$HOME` or the repository. Also:
  - Run CI as `TMPDIR=/tmp python3 scripts/ci_check.py`; a long `TMPDIR`
    breaks a unix-socket test.
  - Bash calls longer than about 180 s are moved to the background. Start
    long commands writing to a log that ends with a `CI EXIT $?` line, then
    poll it in short foreground loops.
  - Never end the turn with work running in the background. Commit and push
    before replying. In epic 9, Haiku developers twice stopped with CI still
    running and never committed.
  - Stop servers by PID, never `pkill -f` (it killed a reviewer's own shell).
    `$!` inside `( … &)` is the wrapper shell; check the listening PID.
- **Final reply:** SHAs, a summary, any public helpers or contracts that later
  issues will use, the exact validation results, the JS size, any caveats, and
  learnings about tools and processes, plus opinions, for the session memory.

When later issues depend on an earlier one, ask the earlier developer in its
first prompt to report the contracts it creates. Examples are URL formats such
as `#/generate?workflow=<id>` and helper exports such as `buildGenerateUrl(run)`.
Pass those contracts verbatim to the dependent developers.

Once a branch is approved and only waiting to merge, dependent work can start
from it to save a wave. Tell the developer to branch from `origin/<branch>`,
then after the merge run `git rebase --onto origin/main <old head> HEAD` and
confirm that `git log origin/main..HEAD` contains only its own commits.

When a merge or a decision changes a shared contract, send a heads-up to every
in-flight developer it affects. Message only developers that are still running;
otherwise put it in the next handoff brief. For example, `LlmRoles::text()`
stopped requiring tool support, so tool callers must call `check_request`.

## 4. Review loop

1. Start a fresh reviewer on `git diff origin/main...<branch>`. Give it the
   guardrails to focus on. Require a verdict of `APPROVE` or
   `CHANGES_REQUESTED`, with severity and `file:line` for each finding, and
   learnings for the session memory.
2. Decide on every finding:
   - **Accept** real correctness or guardrail problems.
   - **Overrule** with a reason when a finding conflicts with project direction
     or a decision already made. For example, raw provenance is not redacted in
     a loopback-only tool, and Rust source compatibility is not kept for the
     internal `genaimage-api` crate.
   - **Spin off** a backend prerequisite to a separate developer and PR when a
     UI finding can only be fixed server-side. Examples are a re-run endpoint,
     workflow revision pinning and explicit reconcile outcomes.
3. Start a fresh developer with a handoff brief that lists your decisions.
4. When it reports, start a fresh reviewer. Give it the earlier findings and
   decisions, so it verifies the fixes and does not re-raise overruled points.
5. After about three rounds on one branch, give the next reviewer a High-only
   rule. Block only on silent substitution, data loss, false success, duplicate
   or blind submission, or mutation of the source record. Record Medium and Low
   findings as follow-ups.
6. When a split feature adds a new component to a branch, such as the server
   half after the core, have a fresh reviewer assess it under full rules.
   Apply the High-only rule only to fix deltas.
7. If you lean towards a design change, put it to the reviewer as a
   "supervisor note to evaluate" rather than ordering it. In epic 7, this
   settled base64 image content, a distinct `awaiting_confirmation` event and
   preview tokens before any client depended on the old form.
8. Look at visible output yourself before shipping: screenshots, generated
   images, rendered pages. In epic 8 this caught a portal spacing bug and an
   unhelpful screenshot that review had passed. In epic 9, screenshots caught
   stretched inputs and run-together hint text, and a real-browser check
   (headless Playwright against a scratch server) found two dialog and focus
   bugs that the jsdom tests passed.
9. To save a round, have the reviewer of the next component also verify the
   previous fix delta under the High-only rule. For a fix of a few lines (copy,
   CSS, numbers in a document), read the diff yourself and ship; say so in the
   PR.
10. Overrule a High finding only when the path fails closed. Write down why,
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
   - refuses a branch that is behind `origin/main` (start a fresh developer to
     rebase it), or a body that doesn't reference the issue;
   - opens the PR, waits 20 seconds and then watches the checks (starting
     the watch immediately can report "no checks" and exit);
   - merges with `--merge` only if every check passes;
   - reports the issue state and the new `origin/main`.

   It rechecks the issue for 30 seconds, because GitHub closes it after the
   merge. Close it with a comment if it is still open (expected with `Refs`).
3. Record in the session memory that `main` has moved and what the merge changed
   (migration numbers, shared modules, generated files). Send a heads-up to
   running developers. The next subagent on each open branch rebases first.
4. Read the merged PR's "Release binary size" row from the performance job
   (`gh run view <id> --log | grep "Release binary size"`). The job is
   informational and never fails. In epic 9 the binary grew from 22.7 MB to
   25.4 MB over five merges before anyone noticed. `perf_check.py` now emits a
   `::warning` annotation when a budget is over.

## 6. Rebase rules for developers

Include these in the prompt of any developer that rebases.

- Shared generated files conflict on almost every merge: `docs/api/openapi.json`
  and `apps/portal/src/lib/api/schema.d.ts`. Always regenerate them with
  `cargo run -p genaimage-server --no-default-features --example openapi` and
  `python3 scripts/generate_portal_types.py`. Never merge them by hand.
- A change to shared UI primitives (for example the `Dialog` focus trap) must
  keep earlier regression tests passing.

## 7. Recovering after an interruption

Background subagents can disappear if the session is interrupted. Treat every
subagent that has not reported and can no longer be found as lost.

1. Read the session memory. For each in-flight item, check its worktree with
   `git status -sb` and `git log`, and check the remote branch.
2. Start fresh developers with a handoff brief that also describes the remote
   SHA, the uncommitted local changes and any half-finished rebase. Tell each one
   to keep the correct work, delete build leftovers such as `*.o`/`*.res`, and
   finish the remaining task.
3. For a lost review, start a fresh reviewer with the earlier decisions.

Usage limits also end background subagents mid-task; their uncommitted work
survives in the worktree.

When a report arrives but the notification says the subagent still has
background work, check that its worktree is committed and stop it before you
send anyone else there. If a developer ends without committing but its CI log
shows `CI EXIT 0`, start a cheap delivery-only developer to check the diff,
commit and push. If the permission classifier refuses a subagent's
force-push, do not push on its behalf: tell the user, and let a fresh developer
rebuild the branch (`git reset` plus rebase, then `--force-with-lease`).

## 8. Close the epic

1. Check that every sub-issue is closed.
2. Run `python3 scripts/ci_check.py` and `python3 scripts/perf_check.py --quick`
   on `origin/main`, in a detached worktree
   (`git worktree add --detach <repo>.worktrees/epic<N>-final origin/main`).
   Run them yourself with `bash` and log to files, not in a synchronous
   subagent, so an interruption doesn't lose the results. `perf_check.py`
   needs `~/.cargo/bin` on `PATH`.
3. Comment on the epic with the PRs, the final checks, the budgets, the key
   decisions and the follow-ups. Do this even if someone else closed the epic
   first, then close it if it is still open.
4. Remove the epic worktrees. Removing them is slow because `node_modules` and
   `target` are large. Also delete untracked `apps/` build leftovers from
   older branches.
5. Give the user a summary that includes every supervisor decision they may
   want to revisit.

## 9. Consolidate the session

Do this at the end of every session, whether the epic is finished, paused or
abandoned. If a session ended without it, do it at the start of the next one.

1. Read the session memory, including any file not yet consolidated.
2. Run a retrospective: what worked, what slowed the work, what had to be redone
   and why, and which decisions to revisit. Support each conclusion with
   evidence from the session.
3. Choose one destination for each learning. Prefer correcting or removing
   existing text over adding more.

   | Learning | Destination |
   | --- | --- |
   | Fact or rule needed in every session | `AGENTS.md`, keeping `CLAUDE.md` and `.github/copilot-instructions.md` aligned |
   | Rule for particular paths only | Matching files in `.github/instructions/*.instructions.md` and `.claude/rules/` |
   | Repeatable multi-step procedure or domain knowledge | A new or existing skill |
   | Repetitive, deterministic commands | A script, written by a fresh developer, in `scripts/` or in the skill that uses it |
   | Supervision procedure | This skill |
   | One-off or unverified | No change; say why in the retrospective |

4. Edit instruction files and skills in a new worktree on a `retro/<date>-<slug>`
   branch.
   - Keep each instruction file short, ideally under 200 lines.
   - Give each skill `name` and `description` frontmatter only. The name matches
     its directory (lowercase letters, numbers and hyphens, at most 64
     characters). The description is in the third person, says what the skill
     does and when to use it, and is at most 1,024 characters.
   - Keep `SKILL.md` under 500 lines; move detail to files it links to directly.
   - Keep the `.agents/skills/` and `.claude/skills/` copies identical.
5. Run `python3 scripts/ci_check.py`, push, and open a PR containing the
   retrospective. These changes steer every future session, so show the user the
   summary and merge only after they approve.
6. Add `Consolidated: <PR URL, or "no changes">` to the memory file.

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
- Epic 8 (setup, models and distribution) added these:
  - **Atomicity:** a slot reserved before an `.await` needs an RAII guard, and
    a spawned driver needs a panic guard that settles it as failed.
  - **False success:** a boolean "saved" instead of a signature of what was
    saved; a success callback bound to current UI choices instead of what the
    run actually used; a diagnostic that mirrors runtime logic loosely.
  - **Untrusted typed responses:** validate the expected set (for example all
    doctor check ids exactly once) before showing aggregates.
  - **Weak tests:** a tolerance that does not assert that nothing else
    changed.
- When the user wants the product running on their own machine, make that a
  separate work item with its own PR. On a real AMD GPU it found a missing
  `libatomic1` and multi-GPU selection issues that CI on CPU cannot.
- Ask the reviewer of a confirmation or submission UI to check persisted
  states after a reload, not just the live stream. Both epic 7 UI rounds found
  bugs that appeared only after a reload.
- Tell the user when review rounds are what is slowing progress, and offer the
  High-only rule. Do not wait to be asked why it is slow.
- Epic 9 (packs, import, batches, templates, event sinks) added these:
  - **Path confinement:** confine the manifest or config file itself, not
    only the files it declares, and read after discovery with
    `open_regular_beneath`.
  - **Validator sweep:** a new file-backed concept must extend the shared
    validator that validate, the doctor and listings use.
  - **Every consumer:** when an API list gains an unavailable state, check
    every consumer (the Generate picker, the assistant tools).
  - **Writes:**
    - an error after a successful write must say what was written;
    - an "adopt the leftover" path beside a create path turns rollbacks into
      deletion hazards, so `AlreadyExists` on the final publish means "lost
      the race";
    - roll back only what no other call can have adopted.
  - **Silent omissions:**
    - caps applied before sorting or without a diagnostic;
    - echoed caller keys without a length cap.
  - **Weak tests:**
    - threads in one process cannot prove a `flock`, so use a second file
      descriptor or process and a barrier;
    - UI tests that mock `api` miss headers and number spelling.
  - **Drivers:** start drivers and monitors inside the tracked `run_mutation`
    task, as `submit_and_monitor` does.
  - **Assistant tools:** `preview` must be deterministic, because each
    decision re-runs it.
