# Complexity-aware Linear synchronization

Rectilinear synchronizes Linear as a sequence of shallow, independently paginated operations. A team sync runs these families in order:

1. workspace teams (when resolving the requested team);
2. projects filtered through `accessibleTeams`, followed by each project's teams, members, labels, and milestones;
3. the workspace label catalog;
4. cycles filtered by team;
5. issue core records filtered by team;
6. each changed issue's labels, relationships, and comments.

Issues fetch project, project-milestone, and cycle membership as scalar object references. Comments, relationships, and project sub-connections are never nested into the team issue or project pages.

## Complexity budget and page sizes

`SyncQueryConfig` plans every operation against a conservative default target of 7,000 points, leaving 30% headroom below Linear's 10,000-point ceiling. The defaults are 25 projects, 50 issues, 50 project members or labels, and 100 nodes for shallow connections. Planning weights deliberately overestimate scalar selections and nested references.

The following environment variables are available for troubleshooting and deterministic tests:

- `RECTILINEAR_LINEAR_COMPLEXITY_TARGET` (clamped to at most 9,000);
- `RECTILINEAR_LINEAR_MIN_PAGE_SIZE`;
- `RECTILINEAR_LINEAR_<OPERATION>_PAGE_SIZE`, where the operation is `TEAMS`, `LABELS`, `PROJECTS`, `PROJECT_TEAMS`, `PROJECT_MEMBERS`, `PROJECT_LABELS`, `PROJECT_MILESTONES`, `CYCLES`, `ISSUES`, `COMMENTS`, or `RELATIONS`;
- `RECTILINEAR_LINEAR_VERBOSE=1` (equivalent to using `sync --verbose`).

If Linear rejects a request for complexity, the paginator halves that operation's page size and retries the same cursor. Reduction continues to the configured minimum. A one-node rejection fails with the operation and requested field set because page-size reduction cannot repair structural nesting.

Transport failures and rate-limit responses retry the same cursor with bounded exponential delay. Authentication, validation, and other API errors fail immediately with operation and cursor context.

## Persistence, retries, and partial failure

Each page is persisted before the next page is requested. Rows receive a unique synchronization token. Stale rows are removed only after the corresponding entity family completes, so an interruption cannot erase the last complete local view. Re-running a partial sync is idempotent: pages may be fetched again, but primary keys, connection-node de-duplication, and upserts prevent duplicates.

`sync_family_state` records `running`, `complete`, or `failed` status for projects, project milestones, labels, cycles, issues, issue labels, relationships, and comments. The team's incremental `updatedAt` cursor advances only after every required family succeeds. A full sync reconciles issues missing from the complete result, while incremental sync only refreshes changed issues.

Pagination uses `updatedAt` ordering where Linear exposes it. Rectilinear removes duplicate node IDs seen across page boundaries. Linear does not expose a workspace-wide snapshot token, so records that change during a traversal are converged by the next incremental sync rather than inferred from names or stale cache entries.

## Membership paths and archive behavior

The supported Linear GraphQL paths are:

- issue project: `issue.project { id name }`;
- issue milestone: `issue.projectMilestone { id name }`;
- issue cycle: `issue.cycle { id name number }`;
- project team ownership: `project.teams`;
- cycle team ownership: `cycle.team { id key }`.

Projects are filtered with `ProjectFilter.accessibleTeams`; multi-team ownership is retained from the independently paginated `project.teams` connection. Rectilinear does not infer any membership from names.

Full syncs include archived entities by default. `--include-archived` provides the same behavior for a requested incremental traversal. Archive flags are passed independently to projects, milestones, cycles, issues, and comments where Linear exposes them.

## Before/after fixture

The deterministic large-workspace planner test models the workspace shape that previously produced `72,400 > 10,000`. The old approach issued one nested request and failed before persistence. The decomposed fixture plans every request below 7,000; for example, a 1,200-issue team uses 24 issue pages of 50 nodes, with an estimated maximum of 5,850 points per issue request, followed by separately bounded label, relation, and comment requests.

Run the demonstration and edge-case suite with:

```text
cargo test -p rectilinear-core linear::pagination
```

Live large-workspace validation remains opt-in because normal tests never require a Linear credential:

```text
rectilinear sync --team ENG --full --include-archived --verbose
```
