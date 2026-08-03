use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::hash::Hash;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use thiserror::Error;

const DEFAULT_COMPLEXITY_TARGET: usize = 7_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinearOperation {
    Teams,
    Labels,
    Projects,
    ProjectTeams,
    ProjectMembers,
    ProjectLabels,
    ProjectMilestones,
    Cycles,
    Issues,
    Comments,
    Relations,
}

impl LinearOperation {
    pub fn name(self) -> &'static str {
        match self {
            Self::Teams => "teams",
            Self::Labels => "labels",
            Self::Projects => "projects",
            Self::ProjectTeams => "project teams",
            Self::ProjectMembers => "project members",
            Self::ProjectLabels => "project labels",
            Self::ProjectMilestones => "project milestones",
            Self::Cycles => "cycles",
            Self::Issues => "issues",
            Self::Comments => "comments",
            Self::Relations => "issue relations",
        }
    }

    fn environment_name(self) -> &'static str {
        match self {
            Self::Teams => "TEAMS",
            Self::Labels => "LABELS",
            Self::Projects => "PROJECTS",
            Self::ProjectTeams => "PROJECT_TEAMS",
            Self::ProjectMembers => "PROJECT_MEMBERS",
            Self::ProjectLabels => "PROJECT_LABELS",
            Self::ProjectMilestones => "PROJECT_MILESTONES",
            Self::Cycles => "CYCLES",
            Self::Issues => "ISSUES",
            Self::Comments => "COMMENTS",
            Self::Relations => "RELATIONS",
        }
    }

    fn recommended_page_size(self) -> usize {
        match self {
            Self::Projects => 25,
            Self::Issues => 50,
            Self::ProjectMembers | Self::ProjectLabels => 50,
            Self::ProjectTeams => 25,
            Self::Comments | Self::Relations => 100,
            Self::Teams | Self::Labels | Self::ProjectMilestones | Self::Cycles => 100,
        }
    }

    fn estimated_complexity(self) -> (usize, usize) {
        // (fixed request cost, conservative per-node cost). These are planning
        // weights, not Linear's private scoring formula. They deliberately
        // overestimate shallow scalar selections and nested reference fields.
        match self {
            Self::Projects => (100, 220),
            Self::Issues => (100, 115),
            Self::ProjectMembers | Self::ProjectLabels => (50, 80),
            Self::ProjectTeams => (50, 120),
            Self::Comments => (50, 45),
            Self::Relations => (50, 55),
            Self::ProjectMilestones => (50, 60),
            Self::Cycles => (50, 45),
            Self::Labels => (50, 35),
            Self::Teams => (50, 30),
        }
    }

    fn field_set(self) -> &'static str {
        match self {
            Self::Teams => "team identity fields",
            Self::Labels => "label identity, color, and parent",
            Self::Projects => "project scalar metadata, status, and lead",
            Self::ProjectTeams => "project team identity fields",
            Self::ProjectMembers => "project member identity fields",
            Self::ProjectLabels => "project label metadata",
            Self::ProjectMilestones => "milestone scalar metadata and project reference",
            Self::Cycles => "cycle scalar metadata and team reference",
            Self::Issues => {
                "issue scalar metadata, labels, project, milestone, and cycle references"
            }
            Self::Comments => "comment body, author, timestamps, parent, and URL",
            Self::Relations => "relation type and related issue identity",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SyncQueryConfig {
    pub complexity_target: usize,
    pub minimum_page_size: usize,
    pub max_retry_attempts: usize,
    pub retry_base_delay: Duration,
    pub verbose: bool,
    page_size_overrides: HashMap<LinearOperation, usize>,
}

impl Default for SyncQueryConfig {
    fn default() -> Self {
        Self {
            complexity_target: DEFAULT_COMPLEXITY_TARGET,
            minimum_page_size: 1,
            max_retry_attempts: 3,
            retry_base_delay: Duration::from_millis(250),
            verbose: false,
            page_size_overrides: HashMap::new(),
        }
    }
}

impl SyncQueryConfig {
    pub fn from_environment() -> Self {
        let mut config = Self::default();
        if let Some(value) = env_usize("RECTILINEAR_LINEAR_COMPLEXITY_TARGET") {
            config.complexity_target = value.clamp(100, 9_000);
        }
        if let Some(value) = env_usize("RECTILINEAR_LINEAR_MIN_PAGE_SIZE") {
            config.minimum_page_size = value.max(1);
        }
        config.verbose = std::env::var("RECTILINEAR_LINEAR_VERBOSE")
            .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
        for operation in [
            LinearOperation::Teams,
            LinearOperation::Labels,
            LinearOperation::Projects,
            LinearOperation::ProjectTeams,
            LinearOperation::ProjectMembers,
            LinearOperation::ProjectLabels,
            LinearOperation::ProjectMilestones,
            LinearOperation::Cycles,
            LinearOperation::Issues,
            LinearOperation::Comments,
            LinearOperation::Relations,
        ] {
            let key = format!(
                "RECTILINEAR_LINEAR_{}_PAGE_SIZE",
                operation.environment_name()
            );
            if let Some(value) = env_usize(&key) {
                config.page_size_overrides.insert(operation, value.max(1));
            }
        }
        config
    }

    pub fn with_page_size(mut self, operation: LinearOperation, page_size: usize) -> Self {
        self.page_size_overrides.insert(operation, page_size.max(1));
        self
    }

    pub fn page_size(&self, operation: LinearOperation) -> usize {
        if let Some(page_size) = self.page_size_overrides.get(&operation) {
            return (*page_size).max(self.minimum_page_size);
        }
        let (base_cost, per_node_cost) = operation.estimated_complexity();
        let planned = self
            .complexity_target
            .saturating_sub(base_cost)
            .checked_div(per_node_cost)
            .unwrap_or(1)
            .max(1);
        operation
            .recommended_page_size()
            .min(planned)
            .max(self.minimum_page_size)
    }

    pub fn estimated_request_complexity(
        &self,
        operation: LinearOperation,
        page_size: usize,
    ) -> usize {
        let (base_cost, per_node_cost) = operation.estimated_complexity();
        base_cost.saturating_add(per_node_cost.saturating_mul(page_size))
    }
}

fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key).ok()?.parse().ok()
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PageInfo {
    #[serde(rename = "hasNextPage")]
    pub(crate) has_next_page: bool,
    #[serde(rename = "endCursor")]
    pub(crate) end_cursor: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ConnectionPage<T> {
    pub(crate) nodes: Vec<T>,
    pub(crate) page_info: PageInfo,
}

#[derive(Debug, Clone)]
pub(crate) struct PageRequest {
    pub(crate) cursor: Option<String>,
    pub(crate) page_size: usize,
    pub(crate) page_number: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct PageContext {
    pub(crate) page_size: usize,
    pub(crate) cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SyncEvent {
    pub operation: &'static str,
    pub parent: Option<String>,
    pub page_number: usize,
    pub nodes_received: usize,
    pub page_size: usize,
    pub adaptive_reduction: bool,
    pub completed: bool,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PaginationStats {
    pub(crate) pages: usize,
    pub(crate) nodes: usize,
    pub(crate) adaptive_reductions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinearErrorKind {
    Authentication,
    RateLimit,
    Complexity,
    Validation,
    Transport,
    Api,
}

#[derive(Debug, Error)]
#[error("Linear {kind:?} error during {operation}{cursor_context}: {message}")]
pub struct LinearOperationError {
    pub kind: LinearErrorKind,
    pub operation: String,
    pub cursor: Option<String>,
    pub message: String,
    pub retry_after: Option<Duration>,
    cursor_context: String,
}

impl LinearOperationError {
    pub fn new(
        kind: LinearErrorKind,
        operation: impl Into<String>,
        cursor: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        let cursor = cursor.map(ToString::to_string);
        let cursor_context = cursor
            .as_deref()
            .map(|value| format!(" at cursor '{value}'"))
            .unwrap_or_default();
        Self {
            kind,
            operation: operation.into(),
            cursor,
            message: message.into(),
            retry_after: None,
            cursor_context,
        }
    }

    pub fn with_retry_after(mut self, retry_after: Option<Duration>) -> Self {
        self.retry_after = retry_after;
        self
    }
}

pub(crate) fn operation_error(error: &anyhow::Error) -> Option<&LinearOperationError> {
    error.downcast_ref::<LinearOperationError>()
}

pub(crate) async fn paginate<T, K, Fetch, FetchFuture, Persist, PersistFuture, KeyFn, Observe>(
    config: &SyncQueryConfig,
    operation: LinearOperation,
    parent: Option<String>,
    mut fetch: Fetch,
    mut persist: Persist,
    key_of: KeyFn,
    mut observe: Observe,
) -> Result<PaginationStats>
where
    K: Eq + Hash,
    Fetch: FnMut(PageRequest) -> FetchFuture,
    FetchFuture: Future<Output = Result<ConnectionPage<T>>>,
    Persist: FnMut(Vec<T>, PageContext) -> PersistFuture,
    PersistFuture: Future<Output = Result<()>>,
    KeyFn: Fn(&T) -> K,
    Observe: FnMut(SyncEvent),
{
    let mut cursor = None;
    let mut page_size = config.page_size(operation);
    let mut stats = PaginationStats::default();
    let mut retry_attempts = 0;
    let mut seen = HashSet::new();

    loop {
        let request = PageRequest {
            cursor: cursor.clone(),
            page_size,
            page_number: stats.pages + 1,
        };
        let page = match fetch(request.clone()).await {
            Ok(page) => {
                retry_attempts = 0;
                page
            }
            Err(error) => {
                let classified = operation_error(&error);
                if classified.is_some_and(|value| value.kind == LinearErrorKind::Complexity) {
                    if page_size > config.minimum_page_size {
                        page_size = (page_size / 2).max(config.minimum_page_size);
                        stats.adaptive_reductions += 1;
                        observe(SyncEvent {
                            operation: operation.name(),
                            parent: parent.clone(),
                            page_number: request.page_number,
                            nodes_received: 0,
                            page_size,
                            adaptive_reduction: true,
                            completed: false,
                            failure: None,
                        });
                        continue;
                    }
                    let diagnostic = if page_size == 1 {
                        format!(
                            "Linear rejected a one-node {} request as too complex at cursor {:?}; \
                             split the operation or reduce the requested field set ({})",
                            operation.name(),
                            cursor,
                            operation.field_set()
                        )
                    } else {
                        format!(
                            "Linear rejected the minimum configured page size ({page_size}) for {} \
                             at cursor {:?}; lower RECTILINEAR_LINEAR_MIN_PAGE_SIZE or split the \
                             requested field set ({})",
                            operation.name(),
                            cursor,
                            operation.field_set()
                        )
                    };
                    observe(SyncEvent {
                        operation: operation.name(),
                        parent: parent.clone(),
                        page_number: request.page_number,
                        nodes_received: 0,
                        page_size,
                        adaptive_reduction: stats.adaptive_reductions > 0,
                        completed: false,
                        failure: Some(diagnostic.clone()),
                    });
                    anyhow::bail!(diagnostic);
                }

                let retry_delay = classified.and_then(|value| match value.kind {
                    LinearErrorKind::RateLimit | LinearErrorKind::Transport
                        if retry_attempts < config.max_retry_attempts =>
                    {
                        Some(value.retry_after.unwrap_or_else(|| {
                            config
                                .retry_base_delay
                                .saturating_mul(1_u32 << retry_attempts.min(10))
                        }))
                    }
                    _ => None,
                });
                if let Some(delay) = retry_delay {
                    retry_attempts += 1;
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    continue;
                }

                let failure = format!("{error:#}");
                observe(SyncEvent {
                    operation: operation.name(),
                    parent: parent.clone(),
                    page_number: request.page_number,
                    nodes_received: 0,
                    page_size,
                    adaptive_reduction: stats.adaptive_reductions > 0,
                    completed: false,
                    failure: Some(failure),
                });
                return Err(error).with_context(|| {
                    format!(
                        "failed to paginate {} at cursor {:?}",
                        operation.name(),
                        cursor
                    )
                });
            }
        };

        if page.page_info.has_next_page && page.page_info.end_cursor.is_none() {
            anyhow::bail!(
                "Malformed {} pagination response on page {}: hasNextPage was true but endCursor was missing",
                operation.name(),
                request.page_number
            );
        }
        if page.page_info.has_next_page && page.page_info.end_cursor == cursor {
            anyhow::bail!(
                "Malformed {} pagination response on page {}: endCursor did not advance",
                operation.name(),
                request.page_number
            );
        }

        let mut nodes = page.nodes;
        nodes.retain(|node| seen.insert(key_of(node)));
        let received = nodes.len();
        persist(
            nodes,
            PageContext {
                page_size,
                cursor: cursor.clone(),
            },
        )
        .await
        .with_context(|| {
            format!(
                "failed to persist {} page {} at cursor {:?}",
                operation.name(),
                request.page_number,
                cursor
            )
        })?;
        stats.pages += 1;
        stats.nodes += received;
        let completed = !page.page_info.has_next_page;
        observe(SyncEvent {
            operation: operation.name(),
            parent: parent.clone(),
            page_number: request.page_number,
            nodes_received: received,
            page_size,
            adaptive_reduction: stats.adaptive_reductions > 0,
            completed,
            failure: None,
        });
        if completed {
            return Ok(stats);
        }
        cursor = page.page_info.end_cursor;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::ready;

    use super::*;

    fn page(
        nodes: &[usize],
        has_next_page: bool,
        end_cursor: Option<&str>,
    ) -> ConnectionPage<usize> {
        ConnectionPage {
            nodes: nodes.to_vec(),
            page_info: PageInfo {
                has_next_page,
                end_cursor: end_cursor.map(ToString::to_string),
            },
        }
    }

    fn run<F>(future: F) -> F::Output
    where
        F: Future,
    {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn single_page_connection_yields_each_node() {
        let mut persisted = Vec::new();
        let stats = run(paginate(
            &SyncQueryConfig::default(),
            LinearOperation::Issues,
            None,
            |_| ready(Ok(page(&[1, 2], false, None))),
            |nodes, _| {
                persisted.extend(nodes);
                ready(Ok(()))
            },
            |node| *node,
            |_| {},
        ))
        .unwrap();
        assert_eq!(persisted, [1, 2]);
        assert_eq!(stats.pages, 1);
    }

    #[test]
    fn multiple_pages_traverse_cursors_and_remove_boundary_duplicates() {
        let mut responses = VecDeque::from([
            page(&[1, 2], true, Some("cursor-1")),
            page(&[2, 3], false, None),
        ]);
        let mut requested = Vec::new();
        let mut persisted = Vec::new();
        let stats = run(paginate(
            &SyncQueryConfig::default(),
            LinearOperation::Issues,
            None,
            |request| {
                requested.push(request.cursor);
                ready(Ok(responses.pop_front().unwrap()))
            },
            |nodes, _| {
                persisted.extend(nodes);
                ready(Ok(()))
            },
            |node| *node,
            |_| {},
        ))
        .unwrap();
        assert_eq!(requested, [None, Some("cursor-1".into())]);
        assert_eq!(persisted, [1, 2, 3]);
        assert_eq!(stats.nodes, 3);
    }

    #[test]
    fn empty_connection_persists_an_empty_page() {
        let mut pages = 0;
        let stats = run(paginate(
            &SyncQueryConfig::default(),
            LinearOperation::Comments,
            None,
            |_| ready(Ok(page(&[], false, None))),
            |nodes, _| {
                assert!(nodes.is_empty());
                pages += 1;
                ready(Ok(()))
            },
            |node| *node,
            |_| {},
        ))
        .unwrap();
        assert_eq!(pages, 1);
        assert_eq!(stats.nodes, 0);
    }

    #[test]
    fn malformed_pagination_metadata_is_rejected() {
        let error = run(paginate(
            &SyncQueryConfig::default(),
            LinearOperation::Comments,
            None,
            |_| ready(Ok(page(&[1], true, None))),
            |_, _| ready(Ok(())),
            |node| *node,
            |_| {},
        ))
        .unwrap_err();
        assert!(error.to_string().contains("endCursor was missing"));
    }

    #[test]
    fn transient_transport_failure_retries_same_cursor() {
        let mut attempts = 0;
        let config = SyncQueryConfig {
            retry_base_delay: Duration::ZERO,
            ..Default::default()
        };
        let stats = run(paginate(
            &config,
            LinearOperation::Issues,
            None,
            |request| {
                attempts += 1;
                if attempts == 1 {
                    ready(Err(LinearOperationError::new(
                        LinearErrorKind::Transport,
                        "issues",
                        request.cursor.as_deref(),
                        "connection reset",
                    )
                    .into()))
                } else {
                    ready(Ok(page(&[1], false, None)))
                }
            },
            |_, _| ready(Ok(())),
            |node| *node,
            |_| {},
        ))
        .unwrap();
        assert_eq!(attempts, 2);
        assert_eq!(stats.nodes, 1);
    }

    #[test]
    fn rate_limit_retries_without_advancing_cursor() {
        let mut cursors = Vec::new();
        let config = SyncQueryConfig {
            retry_base_delay: Duration::ZERO,
            ..Default::default()
        };
        run(paginate(
            &config,
            LinearOperation::Comments,
            None,
            |request| {
                cursors.push(request.cursor.clone());
                if cursors.len() == 1 {
                    ready(Err(LinearOperationError::new(
                        LinearErrorKind::RateLimit,
                        "comments",
                        request.cursor.as_deref(),
                        "too many requests",
                    )
                    .into()))
                } else {
                    ready(Ok(page(&[], false, None)))
                }
            },
            |_, _| ready(Ok(())),
            |node| *node,
            |_| {},
        ))
        .unwrap();
        assert_eq!(cursors, [None, None]);
    }

    #[test]
    fn complexity_rejection_reduces_page_size_at_same_cursor() {
        let config = SyncQueryConfig::default().with_page_size(LinearOperation::Issues, 40);
        let mut sizes = Vec::new();
        let stats = run(paginate(
            &config,
            LinearOperation::Issues,
            None,
            |request| {
                sizes.push(request.page_size);
                if request.page_size > 10 {
                    ready(Err(LinearOperationError::new(
                        LinearErrorKind::Complexity,
                        "issues",
                        request.cursor.as_deref(),
                        "Query complexity exceeds maximum allowed complexity",
                    )
                    .into()))
                } else {
                    ready(Ok(page(&[1], false, None)))
                }
            },
            |_, _| ready(Ok(())),
            |node| *node,
            |_| {},
        ))
        .unwrap();
        assert_eq!(sizes, [40, 20, 10]);
        assert_eq!(stats.adaptive_reductions, 2);
    }

    #[test]
    fn repeated_complexity_rejection_at_minimum_is_actionable() {
        let config = SyncQueryConfig::default().with_page_size(LinearOperation::Projects, 4);
        let error = run(paginate::<usize, usize, _, _, _, _, _, _>(
            &config,
            LinearOperation::Projects,
            None,
            |request| {
                ready(Err(LinearOperationError::new(
                    LinearErrorKind::Complexity,
                    "projects",
                    request.cursor.as_deref(),
                    "too complex",
                )
                .into()))
            },
            |_, _| ready(Ok(())),
            |node| *node,
            |_| {},
        ))
        .unwrap_err();
        assert!(error.to_string().contains("one-node projects request"));
        assert!(error.to_string().contains("field set"));
    }

    #[test]
    fn repeated_complexity_rejection_stops_at_configured_minimum() {
        let mut sizes = Vec::new();
        let config = SyncQueryConfig {
            minimum_page_size: 5,
            ..SyncQueryConfig::default().with_page_size(LinearOperation::Issues, 20)
        };
        let error = run(paginate::<usize, usize, _, _, _, _, _, _>(
            &config,
            LinearOperation::Issues,
            None,
            |request| {
                sizes.push(request.page_size);
                ready(Err(LinearOperationError::new(
                    LinearErrorKind::Complexity,
                    "issues",
                    request.cursor.as_deref(),
                    "too complex",
                )
                .into()))
            },
            |_, _| ready(Ok(())),
            |node| *node,
            |_| {},
        ))
        .unwrap_err();
        assert_eq!(sizes, [20, 10, 5]);
        assert!(error
            .to_string()
            .contains("minimum configured page size (5)"));
    }

    #[test]
    fn simulated_large_workspace_stays_under_target() {
        let config = SyncQueryConfig::default();
        let old_workspace_query_complexity = 72_400;
        assert!(old_workspace_query_complexity > 10_000);
        let issue_count: usize = 1_200;
        let issue_page_size = config.page_size(LinearOperation::Issues);
        let issue_request_count = issue_count.div_ceil(issue_page_size);
        assert_eq!(issue_page_size, 50);
        assert_eq!(issue_request_count, 24);
        for operation in [
            LinearOperation::Teams,
            LinearOperation::Labels,
            LinearOperation::Projects,
            LinearOperation::ProjectTeams,
            LinearOperation::ProjectMembers,
            LinearOperation::ProjectLabels,
            LinearOperation::ProjectMilestones,
            LinearOperation::Cycles,
            LinearOperation::Issues,
            LinearOperation::Comments,
            LinearOperation::Relations,
        ] {
            let size = config.page_size(operation);
            assert!(
                config.estimated_request_complexity(operation, size) <= config.complexity_target,
                "{} planned above target",
                operation.name()
            );
        }
    }
}
