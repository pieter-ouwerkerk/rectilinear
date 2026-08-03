# Progressive issue synchronization

Rectilinear 0.7 separates the authoritative issue index from rich issue
hydration. The index is sufficient to display an issue and classify it by
Linear workflow state (`state_name` and `state_type`). It contains identity,
title, URL, team, creation/update timestamps, and `archived_at`.

Before hydration, `description`, assignee, project, milestone, cycle, and branch
may be absent; priority is `0`, labels are empty, relations/comments are absent,
and their persisted hydration state explains whether that means pending,
retryable, unavailable, or genuinely hydrated.

## Sepia call sequence

The generated Swift API is designed for this sequence (exact `async` spelling
depends on the UniFFI integration wrapper):

```swift
let index = try await engine.syncTeamIndex(
    teamKey: "CUT", full: false, workspaceId: workspaceID)

// Existing list/search APIs are usable as soon as the call above returns.
let issues = try engine.listAllIssues(
    team: "CUT", filter: nil, limit: 100, offset: 0,
    workspaceId: workspaceID)

// Run bounded batches from the app's background scheduler.
let batch = try await engine.hydratePendingIssues(
    teamKey: "CUT", workspaceId: workspaceID, limit: 50,
    policy: .openAndRecent)

// A selection bypasses normal priority and permanent-failure suppression once.
let selected = try await engine.hydrateIssue(
    issueId: issues[0].id, workspaceId: workspaceID)
let state = try engine.getIssueHydrationState(
    issueId: issues[0].id, workspaceId: workspaceID)
```

`syncTeam` remains the compatibility operation. It refreshes projects, the
label catalog, cycles, the bounded issue index, and then uses the `all`
hydration policy. New clients should use the progressive sequence above.

## Checkpoints and reconciliation

`sync_team_index` reads the last committed `synced_through_at`, subtracts a
five-minute overlap, fixes an upper timestamp two seconds behind the local
clock, and traverses that bounded window ordered by `updatedAt`. Relay cursors
are used only during that traversal. The upper bound is committed only after
every page is persisted, including an empty final page. Full traversals remove
locally cached issues missing from the complete remote result; interrupted
traversals never reconcile.

The overlap makes upserts intentionally repeat and idempotent. It covers clock
skew, equal timestamps, and an issue moving near a run boundary. Linear does
not expose hard deletions through an incremental timestamp feed, so hard
deletions remain visible locally until a successful periodic full index sync.

## Comment strategy

Selected-issue hydration fetches that issue's comments directly. Background
hydration uses the same per-issue connection only for the bounded policy set:
open issues and recently completed issues by default, never every historical
completed issue. Persisted comment hydration is refreshed after 15 minutes for
eligible issues even when the parent issue's `updatedAt` did not change, so
comment changes do not depend on an issue-index update. The `all` policy is the
explicit opt-in for old completed/canceled issues.

This avoids a workspace-wide comment scan and retains exact deletion
reconciliation for each refreshed issue. A future global comments feed could
reduce open-issue request count further, but would need its own bounded durable
timestamp checkpoint and a separate policy for comment hard deletions.

## Manual verification

```bash
rectilinear sync --team CUT --index-only
rectilinear hydrate --team CUT --open-only --limit 50
rectilinear hydrate --issue CUT-123
```
