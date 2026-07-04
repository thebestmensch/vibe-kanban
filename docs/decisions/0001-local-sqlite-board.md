# ADR 0001 (JM-714): Kanban board backend — port to local SQLite

- Status: accepted (2026-07-04)
- Reviewed by: Claude devil's-advocate (verdict REVISE → revisions folded in), Codex GPT-5.x plan critique (6 Important findings → folded in)

## Context

Fork of vibe-kanban (upstream BloopAI, frozen since April 2026 sunset). Goal: fully-local, single-user agent-orchestration board on a Mac. At HEAD the kanban board is a signed-in cloud feature — and already retired even there: `ProjectKanban.tsx` renders `ProjectSunsetPage` (export-only) and `KanbanContainer.tsx` (the 1160-line board) is imported by nothing in the live route tree. Local mode has workspaces but no board.

Verified facts (2026-07-04 recon, three independent passes + two adversarial reviews):

1. **Frontend seam.** `KanbanContainer` consumes `useProjectContext()` — plain typed arrays + insert/update/remove fns. Electric/TanStack-DB coupling lives in `ProjectProvider.tsx` (10 `useShape` calls) + `integrations/electric/hooks.ts`, **plus the board's render path**: `SharedAppLayout.tsx:131`, `NavbarContainer.tsx:210`, `OrgProvider`, `UserProvider` each call `useShape` too. Write path: `bulkUpdateIssues` (drag reorder) called from 3 sites; `remoteApi.makeRequest` hits `${getRemoteApiUrl()}/v1/*` with bearer auth — the live contract is `/v1/*` on the cloud base, NOT the local `/api/remote/*` proxy.
2. **Route shapes exist locally, persistence doesn't.** `crates/server/src/routes/remote/*` defines full CRUD for issues/projects/statuses/tags/assignees/relationships/PRs/workspaces — every handler forwards to the cloud via `RemoteClient`.
3. **Local push channel exists but is narrower than Electric.** `deployment.events()` → SSE `GET /api/events`; hook tables today are only `workspaces`/`execution_processes`/`scratch` (`crates/services/src/services/events/types.rs:20`), and `msg_store` drops lagged messages without recovery. Board entities need their own snapshot-on-connect + refetch semantics.
4. **A fully-local board shipped historically.** Cloud pivot: `cce14bf0e` "Remote kanban" (2026-02-02). The SQLite `tasks` table + read-only `Task` model survive at HEAD — orphaned but still referenced by sqlx checks (`task.rs:35`) and a `workspace_repo.rs:235` join.
5. **crates/remote anatomy:** 24.3k LOC Rust, ~6–7k board-domain. Reads hard-wired to ElectricSQL (16 shapes, auth-gated proxy, no REST read path); writes REST with a Postgres-txid handshake that retires optimistic client state when the txid appears on the shape stream (`collections.ts:629-758` ↔ `db/issues.rs:394,514`). Minimal self-host = Postgres(logical) + Electric + remote-server; single-user auth first-class (`SELF_HOST_LOCAL_AUTH_*`). `sort_order` is `DOUBLE PRECISION` (fractional indexing — single-card moves are one write). `simple_id` is trigger-generated, joining `projects → organizations` for the prefix and incrementing `projects.issue_counter`.

## Decision

**Port the board to local SQLite (Option A).** Re-implement board persistence in `crates/db`/`crates/server`, keep the cloud entity schemas verbatim (ts-rs type parity), source the frontend from local REST + SSE behind the existing `ProjectContextValue` contract.

Load-bearing sub-decisions (were open questions; resolved per review):

- **Schema:** keep cloud `Issue`/`ProjectStatus`/… shapes verbatim in SQLite. Seed exactly one `organizations` row and one `users` row so FKs and the `issue_prefix` lookup keep meaning. Don't strip FK columns — divergence breaks the ts-rs parity that justifies keeping the schema.
- **`simple_id`:** counter stays in the DB — increment `projects.issue_counter` and assign `simple_id` in the same SQLite transaction, with a UNIQUE constraint as backstop. Not app code: multi-tab + retry-after-timeout creates are real even single-user, and the cloud shipped a counter-dedup bug here (`20260313000000_fix-short-id-counter.sql`) as the cautionary tale.
- **Optimistic writes (the txid replacement):** "persisted" = local REST 200 — SQLite commits synchronously and the server is the single writer, so the txid handshake's job (durability confirmation against an async replication stream) has no local equivalent problem. SSE is broadcast/invalidation, not the write ack. Hand-roll an optimistic overlay ONLY for drag-reorder (the one visibly snap-back-prone interaction); fractional `sort_order` keeps that surface to a single-field write.
- **Live updates:** v1 uses SSE as a coarse invalidation signal (entity-type + project id) driving refetch of the affected collection — not patch-perfect streams. Bounded correctness under `msg_store` lag-drops (a dropped signal only delays a refetch; snapshot-on-connect = initial REST fetch). Patch-level streaming is a later optimization if refetch ever measurably lags.
- **`tasks` table:** defer the drop. `task.rs` and `workspace_repo.rs` still compile against it; retire those dependents first, drop in the post-decision strip PR.
- **Comments / issue detail panel:** out of scope for v1 parity. `KanbanIssuePanelContainer` always mounts comments via Electric-backed `IssueProvider` — v1 stubs comments/reactions as empty local collections so the panel renders; local comment persistence is a follow-up.
- **Multi-step create** (issue → assignees/tags/workspace draft): v1 keeps the existing sequential awaits against local REST (fast, single-user); accepted risk of partial creates on crash — no orphan-cleanup machinery.

## Alternatives rejected

- **B — self-host `crates/remote` permanently** (Postgres + Electric + remote-server via docker-compose): board-domain code already works and single-user auth exists, but the frontend board is sunset even in cloud mode (would need un-retiring anyway), and the steady-state cost is unbounded: 3-service docker dependency on the Mac forever, 24k LOC (17k cloud-ops) to maintain with a frozen upstream, two databases of truth (SQLite workspaces + Postgres board) bridged over HTTP, and every future feature (JM-716/717/718) paying the Postgres+Electric+txid integration tax instead of writing plain SQLite.
- **C — forward-port the pre-pivot local board** (`TaskKanbanBoard.tsx` at `cce14bf0e~1`, backed by the surviving `tasks` table): genuinely cheaper — no Electric decoupling, no shell surgery, backend already local. Rejected on merit: the old task board lacks sub-issues, relationships, PR links, and tags — exactly what JM-716 (dual accounts), JM-717 (PR checks), and JM-718 (Linear mirror) need. The Electric-decoupling tax buys that richness.
- **Electric against SQLite:** non-starter — Electric consumes Postgres logical replication.
- **Thin in-process Postgres:** Option B with a smaller binary; keeps the Electric process, txid contract, and dual-store tax.

## Implementation plan (thin-slice ordering)

**v1 — screenshot AC** (create issue → card → workspace → agent attempt → live activity):
1. SQLite migrations + models: `issues`, `project_statuses` (+ seed org/user rows; project rows already local). `simple_id` counter in-transaction.
2. Local REST: re-back `routes/remote/{issues,projects,project_statuses}` handlers with SQLite (same DTO shapes); emit SSE invalidation events per mutation (extend the events service beyond workspaces/processes/scratch).
3. Frontend: local-backed `useShape` equivalent (REST fetch + SSE-invalidate refetch, same `{data,isLoading,insert,update,remove}` surface) + `ProjectProvider` rewrite behind the unchanged `ProjectContextValue` contract; drag-reorder optimistic overlay; re-point the 3 `bulkUpdateIssues` call sites.
4. Shell decoupling: `SharedAppLayout`/`NavbarContainer`/`OrgProvider`/`UserProvider` off Electric (local projects endpoint + seeded single user); remove sign-in gates (`RootRedirectPage` loggedin branch, `ProjectKanban` `isSignedIn` gate, sunset page); stub comments so the issue panel renders.
5. Verify DoD end-to-end; attach JM-715 screenshot.

**v2 — parity adornments:** `tags`, `issue_tags`, `issue_assignees`, `issue_relationships`, `pull_request_issues` re-backed incrementally (these carry the org/user FK baggage — land behind proof-of-life).

**v3 — strip:** `crates/remote`, `packages/remote-web`, Electric deps, `RemoteClient`/`remote_sync` bridge, cloud onboarding paths, `tasks` table + dependents. Enumerated strip list per ticket AC; builds green after each removal.
