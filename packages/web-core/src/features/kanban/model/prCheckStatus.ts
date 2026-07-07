import type { CheckStatus } from 'shared/types';

/**
 * Defensive read of the LOCAL board fallback's extra `check_status` field.
 *
 * The local `pull_requests` fallback row (see `board.rs` `BoardPullRequestRow`)
 * carries a `check_status` field that is NOT declared on the generated Electric
 * `PullRequest` type. Both PR-mapping paths in `KanbanContainer` — issue-level
 * (`issueCardPullRequests`, JM-749) and workspace-level (`prsByWorkspaceId`,
 * JM-751) — need it, so the cast + "Electric mode lacks this field" rationale
 * live here once. In remote/Electric mode the field is absent and this returns
 * `null`, so no check badge renders.
 */
export function getPrCheckStatus(pr: unknown): CheckStatus | null {
  return (pr as { check_status?: CheckStatus | null }).check_status ?? null;
}
