# Progressive issue synchronization

Rectilinear 0.7 separates the authoritative issue index from rich issue
hydration. The index is sufficient to display an issue and classify it by
Linear workflow state (`state_name` and `state_type`). It contains identity,
title, URL, team, creation/update timestamps, and `archived_at`.

Before hydration, `description`, assignee, project, milestone, cycle, and branch
may be absent; priority is `0`, labels are empty, relations/comments are absent,
and their persisted hydration state explains whether that means pending,
retryable, unavailable, or genuinely hydrated.

## Generic client call sequence

Exact `async` spelling depends on the UniFFI integration wrapper:

```swift
let index = try await engine.syncTeamIndex(
    teamKey: "CUT", full: false, workspaceId: workspaceID)

// Existing list/search APIs are usable as soon as the call above returns.
let issues = try engine.listAllIssues(
    team: "CUT", filter: nil, limit: 100, offset: 0,
    workspaceId: workspaceID)

// Project metadata is independently team-scoped and reconciled only after a
// complete traversal.
let projects = try await engine.syncTeamProjects(
    teamKey: "CUT", workspaceId: workspaceID)

// Run bounded batches from the app's background scheduler.
let batch = try await engine.hydratePendingIssues(
    teamKey: "CUT", workspaceId: workspaceID, limit: 50,
    policy: .openAndRecent)

// Ordinary selection access fetches only pending, due, or policy-stale data.
let selected = try await engine.hydrateIssueWithMode(
    issueId: issues[0].id, workspaceId: workspaceID, mode: .ifNeeded)
let state = try engine.getIssueHydrationState(
    issueId: issues[0].id, workspaceId: workspaceID)

// Keep force refresh behind an explicit user action. It retries every family,
// including one recovery attempt for earlier permanent failures.
let refreshed = try await engine.hydrateIssueWithMode(
    issueId: issues[0].id, workspaceId: workspaceID, mode: .forceRefresh)
```

The generic progressive client sequence is:

1. Synchronize the issue index.
2. Present locally indexed issues.
3. Synchronize team project metadata independently.
4. Hydrate bounded background batches.
5. Hydrate selected issues using `IfNeeded`.
6. Generate embeddings only for fully detail-hydrated issues.
7. Offer `ForceRefresh` as a separate explicit action.

`IfNeeded` hydrates pending resources, due retryable resources, and resources
made stale by the existing refresh policy. It leaves fresh resources and
permanent permission/unavailable states alone. The compatibility
`hydrateIssue(issueId:workspaceId:)` method retains the original force-refresh
behavior for generated clients that already call it.

`syncTeam` remains the compatibility operation. It refreshes projects, the
label catalog, cycles, the bounded issue index, and then uses the `all`
hydration policy. New clients should use the progressive sequence above.

## Project metadata

`sync_team_projects(team_key, workspace_id)` reuses the existing team-scoped
project synchronizer and returns project and milestone counts. Its successful
traversal preserves the same project/milestone reconciliation guarantees as
the workspace-wide `sync_projects`; an interrupted traversal does not perform
destructive reconciliation. `sync_projects` remains available for callers that
intentionally refresh the entire workspace catalog.

## Embeddings

Only issues whose `details` hydration resource is `hydrated` are eligible for
embedding. Pending, running, retryable, permission-denied, and unavailable
details are excluded, including when callers request a forced embedding pass.
Legacy rich issues migrated as detail-hydrated remain eligible.

Each chunk set records both its embedding model and the hash of the exact
title/description content used to produce it. A model or content-hash change
selects the issue for transactional chunk replacement; unchanged content with
the same model is left intact. Index changes do not delete valid chunks. If an
indexed title changes, details are first rehydrated, then the stale content hash
causes regeneration. Existing chunks from pre-0.7 development databases are
preserved and marked for a one-time regeneration because they lack an exact
source hash.

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
rectilinear projects sync --team CUT
rectilinear hydrate --team CUT --open-only --limit 50
rectilinear hydrate --issue CUT-123
rectilinear hydrate --issue CUT-123 --force-refresh
```
