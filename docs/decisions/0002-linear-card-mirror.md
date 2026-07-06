# ADR 0002 (JM-718): Linear card mirror — outbound status push + per-project account binding

- Status: accepted (2026-07-05)
- Reviewed by: Claude devil's-advocate (verdict REVISE → revisions folded in), Codex GPT-5.x plan critique (2 Critical + 6 Important → folded in). Both dispatched on the pre-implementation design per the ticket AC (credential-storage surface).

## Context

Fork of vibe-kanban, fully-local single-user agent-orchestration board on a Mac. JM-714 shipped the local SQLite board (`issues` / `project_statuses` / `projects`, one seeded `organizations` + `users` row). JM-732 backed project create/rename/reorder and feature-gated team adornments. This ticket delivers the core workflow vision: **a kanban card mirrors its Linear ticket's status**, board-driven, across **two Linear accounts** (jm personal + oom work).

Grounding facts (verified 2026-07-05, three independent read-only investigations):

1. **Card = `issues` row.** Local-board card is an `issues` row (`crates/db/migrations/20260318000000_local_board.sql`), NOT the legacy `tasks` table (the ticket's "tasks table" wording is stale — `tasks` is orphaned). Column/status = `issues.status_id` FK → `project_statuses` (rows Todo / In Progress / In Review / Done / Cancelled, seeded per project — each project has its own status rows with distinct UUIDs). Column-move = `PATCH /v1/issues/{id}` `{status_id, sort_order}` (`update_issue`, `crates/server/src/routes/board_v1/mutations.rs:95`) **and** drag-drop `POST /v1/issues/bulk` (`bulk_update_issues`, `mutations.rs:111`) — bulk also rewrites unchanged destination cards.
2. **Sync-loop template = `pr_monitor.rs`.** `tokio::interval` (60s) + `Arc<Notify>` hybrid in a `select!`; drains a `needs_sync` DB flag each tick; spawned at `crates/local-deployment/src/lib.rs:263`; DB via models layer; `backon` exponential retry (1s→30s, 3×) in the client layer; classifies environmental vs real errors, treats remote-404 as reconciliation. GitHub creds are ambient `gh` CLI — Linear has no CLI, so tokens must be stored.
3. **Config = single global `Config` → `config.json`.** Versioned JSON. `GitHubConfig` is a single-account blob (the anti-pattern to avoid). **Critical exposure:** `/api/info` returns `UserSystemInfo { config: Config }` and `PUT /config` accepts+persists the whole `Config` (`crates/server/src/routes/config.rs:88,180`) — anything on `Config` is readable via the info route and round-trips through config save.
4. **`projects` has nullable-scalar precedent** (`default_agent_working_dir`, `remote_project_id`, `organization_id`); frontend card badges render in `packages/ui/src/components/KanbanCardContent.tsx` (row 5, `PrBadge` is the copy target).
5. **Accounts (from operator context):** each Linear account is effectively single-team (oom = OneOnMe team; jm = one team shared across projects). Linear `WorkflowState` is scoped per-team; issue ids are globally-unique UUIDs.

## Decision

**Ship the outbound half of the mirror in this ticket. Board owns status; Linear owns existence.** A card is linked to a Linear issue (manually in v1), and a board column-move pushes the mapped Linear workflow-state. Inbound auto-import and the Claude-variant per-project binding are split out (see §Scope split).

1. **New `crates/linear/`** — concrete Linear GraphQL client (`reqwest`; no CLI). Keyed account collection (not a single blob). Outbound-only v1 methods: `update_issue_state(account, issue_id, state_id)`, `list_workflow_states(account, team)` (mapping UI), `resolve_issue_by_identifier(account, "OOM-123")` (manual-link resolve). `backon` retry mirroring `git-host`.
2. **Credential storage: `config.json` keyed map, with redaction.** New `linear: LinearConfig { accounts: HashMap<String, LinearAccount> }` on `Config`. `LinearAccount { token, workspace_name, team_id, state_map: HashMap<StatusId, LinearStateId> }`, keyed by account key (`"personal"`/`"work"`). Plaintext at rest = consistent with the existing `GitHubConfig` PAT, accepted for local-only (operator confirmed "nothing extreme"; keychain is a later hardening option). **But the plaintext-on-disk posture must NOT extend to the wire:** tokens are redacted from the `/api/info` `Config` response, and account connect/remove go through dedicated routes that never echo the token — `PUT /config` must not be the token round-trip path.
3. **`state_map` keyed by `project_statuses.id`, not name.** Status names are per-project, user-editable, non-unique — a name key breaks on rename/reorder/hide. The map is `StatusId → LinearStateId`. **Unmapped-state fallback (outbound):** a card whose `status_id` has no mapping is skipped and surfaced (badge/log), never silently dropped.
4. **Card ↔ issue link.** Migration on `issues`: `linear_issue_id TEXT NULL`, `linear_issue_identifier TEXT NULL` (e.g. `OOM-123`), `linear_url TEXT NULL`, `linear_state_id TEXT NULL` (last-synced state, for drift/idempotency), `linear_sync_pending INTEGER NOT NULL DEFAULT 0`. Partial `UNIQUE INDEX ON issues(linear_issue_id) WHERE linear_issue_id IS NOT NULL` (dedup backstop; also guards a card being linked to an already-linked issue).
5. **Per-project account binding.** Migration on `projects`: `linear_account_key TEXT NULL`. The outbound loop resolves the account for a card via its project's `linear_account_key`.
6. **Sync trigger lives at the MODEL layer, not the route handler.** `Issues::update` **and** `Issues::bulk_update` set `linear_sync_pending = 1` only when the changeset **actually changes** `status_id` (old ≠ new) **and** the row has a `linear_issue_id`; then nudge the loop's `Notify`. One choke point covers both the single-update and drag-drop-bulk paths — hooking only `update_issue` would silently miss cross-column drags.
7. **Outbound sync loop `crates/services/.../linear_monitor.rs`** (cloned shell from `pr_monitor`, outbound arm only): `interval(60s)` + `Notify`. Each tick drains `issues WHERE linear_sync_pending = 1 AND linear_issue_id IS NOT NULL`; for each, resolve account → map `status_id`→`state_id` → `update_issue_state`. On success: clear `pending`, record `linear_state_id`. On **not-found** (issue deleted in Linear): unlink the card (clear link fields), clear `pending`, surface an "unlinked in Linear" badge — never retry a dead issue forever. On **RATELIMITED / transient**: leave `pending` for the next tick (backon handles short retries).
8. **Echo-loop invariant (explicit + tested):** `linear_sync_pending` is set **only** by the user-driven HTTP mutation path (via the model-layer hook in §6). The `linear_monitor` loop's own writes never set it. There is no inbound status writer in this ticket, so no echo path exists; a test asserts a card touched by the loop does not re-enter the drain.
9. **Manual link flow (v1 link establishment).** Since inbound auto-import is split out, links are established by a "Link to Linear issue" card action: operator supplies an identifier (`OOM-123`) → `resolve_issue_by_identifier` fetches id/url/current-state → link fields set. This is the operator opting into each link, which also bounds the shared-workspace reassignment hazard (§conflict rule).
10. **Frontend:** Linear identifier+URL badge on `KanbanCardContent` (copy `PrBadge`); settings UI for account connect (token+workspace+team), per-project account selector, state-map editor (columns → Linear states); card "Link to Linear" affordance.
11. **This ADR** committed to `docs/decisions/0002-linear-card-mirror.md`.

Sub-decisions (ADR-recorded per AC):

- **Token storage:** `config.json` keyed map, redacted on the wire (§Decision-2). Keychain deferred.
- **Poll vs webhook:** the outbound path is `Notify`-driven (immediate on move) with a 60s safety interval; no inbound poll in this ticket. Webhook (needs public ingress the local Mac lacks) deferred to JM-734.
- **Conflict rule: board-wins, outbound-authoritative — "Linear owns existence, board owns status."** Linear-side status edits on a linked card are not reconciled until the board next moves the card (documented drift; a "last synced" indicator surfaces it). **Shared-workspace caution:** on the oom work account, moving a card overwrites the Linear ticket's state — which may be a colleague's in-flight ticket. v1 mitigates via manual-link-only (you link what you drive) + per-account outbound; a stricter guard (outbound-confirm on work account) is a follow-up if it bites.
- **Single team per account (v1 constraint):** `state_map` + `team_id` model one team per account, matching the operator's real accounts (fact 5). Multi-team-per-account keys `state_map` by `(team_id, status_id)` — deferred; noted so the `config.json` shape can migrate cleanly.
- **Scope:** status + link only. No comment/description/attachment sync. `LinearService` stays concrete/Linear-only — no premature `IssueHost` trait.

## Scope split (post-review)

The original 8-pt ticket was two tickets in a trenchcoat (both reviewers). Split:

- **This ticket (JM-718): outbound-only.** Card→Linear status push, link badge, manual link, per-project account binding, state-map editor.
- **JM-734: inbound import.** Assigned/labeled Linear tickets → cards. Owns the sharp edges: `list_assigned_issues` with Relay pagination + `RATELIMITED` policy, dedup via the unique index + `INSERT … ON CONFLICT DO NOTHING`, the N:1 account→project routing rule, and the reverse `LinearState → per-project status_id` map with a mandatory fallback (`status_id` is NOT NULL).
- **JM-735: per-project Claude-account auto-binding.** JM-716's deferred bullet, un-folded. Shares only the "nullable `projects` column" idea; its resolution (executor spawn) is a disjoint subsystem, so it owns its own column + resolution + UI.

## Alternatives rejected

- **Build inbound in the same ticket:** nearly all the failure modes (dedup, account→project routing, reverse-map + missing-column fallback, pagination/rate-limits) live inbound; outbound alone satisfies the round-trip DoD. Split → JM-734.
- **Fold JM-716's Claude-variant resolution here:** the only shared artifact is a nullable `projects` column; the resolution logic is a different subsystem with its own interactions (workspace-create drafts, retries, follow-ups, PR-created workspaces, existing sessions). Split → JM-735.
- **Sync trigger in the route handler:** misses the `/v1/issues/bulk` drag-drop path → silent non-sync. Model layer covers both.
- **`state_map` by status name / `extension_metadata` JSON link / DB-table credentials / bidirectional last-write-wins / webhook-first:** each rejected above or in §Decision (fragile key, un-indexable, no security gain, echo/conflict surface, no local ingress).

## Implementation plan (thin slices — each: `cargo check`/test + Codex adversarial review before commit)

1. **Schema + config:** migration (`issues` link cols + `linear_sync_pending` + partial unique index; `projects.linear_account_key`), `LinearConfig` keyed map on `Config` + version bump, token redaction on `/api/info`, sqlx prepare.
2. **`crates/linear`:** GraphQL client (`update_issue_state`, `list_workflow_states`, `resolve_issue_by_identifier`), keyed `LinearService`, `backon` retry, unit tests vs mocked responses.
3. **Sync loop + model hook:** `linear_monitor.rs` outbound drain (+ deleted→unlink, ratelimit→retry), spawn + `Notify` at `local-deployment:263`, model-layer pending hook on `Issues::update` + `bulk_update` (actual-change detection).
4. **Settings + link routes + API types:** account connect/list(redacted)/remove, per-project binding, state-map editor, manual link-by-identifier; ts-rs regen.
5. **Frontend:** Linear badge + settings UI + link affordance; visual QA at milestone.

**DoD:** a jm-project card and an oom-project card each round-trip a status change to the correct Linear workspace; ADR committed; `cargo build` green; migration applies cleanly.
