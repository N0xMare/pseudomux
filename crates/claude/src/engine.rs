use std::collections::{HashMap, HashSet};

use pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER;

use crate::{
    ContentBlock, EngineWarning, FinalTurn, LogicalAssistantMessage, LogicalMessageKey, ParseMode,
    ParsedRow, PromptAcknowledgement, RowKind, RowScope, StopReason, TerminalOutcome, TokenUsage,
    ToolRecord, TranscriptAnalysis, TranscriptError, TurnStatus, UsageTotals,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IngestOutcome {
    Added { ordinal: u64 },
    PromptAcknowledged(PromptAcknowledgement),
    DuplicateIgnored { uuid: String },
}

/// Deterministic collection-element work performed by one transcript analysis.
///
/// This diagnostic counts visits made by the production analysis itself. It is
/// not protocol data or a latency estimate; it exists so algorithmic scaling
/// can be gated without workstation timing thresholds or a second evaluator.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TranscriptAnalysisWork {
    element_visits: u64,
}

impl TranscriptAnalysisWork {
    #[must_use]
    pub const fn element_visits(self) -> u64 {
        self.element_visits
    }

    fn record(&mut self, amount: usize) {
        self.element_visits = self
            .element_visits
            .saturating_add(u64::try_from(amount).unwrap_or(u64::MAX));
    }
}

#[inline(always)]
fn record_work<const RECORD_WORK: bool>(work: &mut TranscriptAnalysisWork, amount: usize) {
    if RECORD_WORK {
        work.record(amount);
    }
}

#[derive(Clone, Debug)]
struct StoredRow {
    ordinal: u64,
    row: ParsedRow,
}

#[derive(Clone, Debug)]
struct ActiveTurn {
    expected_prompt: String,
    armed_at_ordinal: u64,
    acknowledgement: Option<PromptAcknowledgement>,
}

/// Pure parent-graph and logical-message accumulator for one Claude transcript.
#[derive(Clone, Debug)]
pub struct TranscriptEngine {
    mode: ParseMode,
    rows: Vec<StoredRow>,
    by_uuid: HashMap<String, usize>,
    next_ordinal: u64,
    active_turn: Option<ActiveTurn>,
}

impl TranscriptEngine {
    #[must_use]
    pub fn new(mode: ParseMode) -> Self {
        Self {
            mode,
            rows: Vec::new(),
            by_uuid: HashMap::new(),
            next_ordinal: 0,
            active_turn: None,
        }
    }

    /// Arms correlation after all rows already ingested by the caller.
    pub fn arm_turn(&mut self, expected_prompt: impl Into<String>) -> Result<(), TranscriptError> {
        if self.active_turn.is_some() {
            return Err(TranscriptError::TurnAlreadyArmed);
        }
        self.active_turn = Some(ActiveTurn {
            expected_prompt: normalize_prompt(&expected_prompt.into()),
            armed_at_ordinal: self.next_ordinal,
            acknowledgement: None,
        });
        Ok(())
    }

    /// Ends correlation while retaining transcript history for the next turn.
    pub fn disarm_turn(&mut self) -> Result<(), TranscriptError> {
        if self.active_turn.take().is_none() {
            return Err(TranscriptError::NoTurnArmed);
        }
        Ok(())
    }

    pub fn ingest(&mut self, row: ParsedRow) -> Result<IngestOutcome, TranscriptError> {
        if let Some(uuid) = row.common.uuid.as_ref()
            && let Some(existing_index) = self.by_uuid.get(uuid).copied()
        {
            let existing = &self.rows[existing_index].row;
            if existing.raw == row.raw {
                return Ok(IngestOutcome::DuplicateIgnored { uuid: uuid.clone() });
            }
            return Err(TranscriptError::ConflictingDuplicateRow { uuid: uuid.clone() });
        }

        let ordinal = self.next_ordinal;
        let mut acknowledged = None;
        if let (Some(turn), RowKind::TypedUser { prompt, prompt_id }) =
            (self.active_turn.as_mut(), &row.kind)
            && ordinal >= turn.armed_at_ordinal
            && row.common.scope == RowScope::Main
        {
            let actual = normalize_prompt(prompt);
            if turn.acknowledgement.is_some() {
                return Err(TranscriptError::MultiplePromptAcknowledgements);
            }
            if actual != turn.expected_prompt {
                return Err(TranscriptError::UnexpectedTypedPrompt {
                    expected: turn.expected_prompt.clone(),
                    actual,
                });
            }
            let row_uuid = row
                .common
                .uuid
                .clone()
                .ok_or_else(|| TranscriptError::SchemaDrift {
                    row_uuid: None,
                    path: "$.uuid".to_owned(),
                    message: "acknowledged typed prompt requires a UUID".to_owned(),
                })?;
            let acknowledgement = PromptAcknowledgement {
                row_uuid,
                prompt_id: prompt_id.clone(),
                ordinal,
            };
            turn.acknowledgement = Some(acknowledgement.clone());
            acknowledged = Some(acknowledgement);
        }

        let row_index = self.rows.len();
        if let Some(uuid) = row.common.uuid.as_ref() {
            self.by_uuid.insert(uuid.clone(), row_index);
        }
        self.rows.push(StoredRow { ordinal, row });
        self.next_ordinal = self.next_ordinal.saturating_add(1);

        Ok(acknowledged.map_or(
            IngestOutcome::Added { ordinal },
            IngestOutcome::PromptAcknowledged,
        ))
    }

    pub fn analyze(&self) -> Result<TranscriptAnalysis, TranscriptError> {
        let mut work = TranscriptAnalysisWork::default();
        self.analyze_recording::<false>(&mut work)
    }

    /// Runs the production analysis while recording deterministic work.
    ///
    /// The result is returned alongside work even on failure so malformed
    /// graph complexity can be bounded by tracked tests. Normal `analyze()` is
    /// monomorphized with recording disabled and has no counter increments.
    #[doc(hidden)]
    pub fn analyze_with_work(
        &self,
    ) -> (
        Result<TranscriptAnalysis, TranscriptError>,
        TranscriptAnalysisWork,
    ) {
        let mut work = TranscriptAnalysisWork::default();
        let analysis = self.analyze_recording::<true>(&mut work);
        (analysis, work)
    }

    fn analyze_recording<const RECORD_WORK: bool>(
        &self,
        work: &mut TranscriptAnalysisWork,
    ) -> Result<TranscriptAnalysis, TranscriptError> {
        let turn = self
            .active_turn
            .as_ref()
            .ok_or(TranscriptError::NoTurnArmed)?;
        let Some(acknowledgement) = turn.acknowledgement.clone() else {
            return Ok(TranscriptAnalysis {
                status: TurnStatus::AwaitingPromptAcknowledgement,
                acknowledgement: None,
                active_chain: Vec::new(),
                messages: Vec::new(),
                tools: Vec::new(),
                usage: UsageTotals::default(),
                sidechain_usage: UsageTotals::default(),
                combined_usage: UsageTotals::default(),
                turn_duration_seen: false,
                stop_hook_summary_seen: false,
                api_error_retries_seen: 0,
                // No acknowledged prompt yet, so no row belongs to this turn --
                // including a sidechain one. Zero here is a fact about an empty
                // window, not an unmeasured default.
                sidechain_rows: 0,
                warnings: self.unknown_warnings_after::<RECORD_WORK>(turn.armed_at_ordinal, work),
            });
        };

        let active = self.active_indices::<RECORD_WORK>(&acknowledgement, work)?;
        let mut warnings = self.unknown_warnings_after::<RECORD_WORK>(turn.armed_at_ordinal, work);
        self.validate_active_unknowns::<RECORD_WORK>(&active.path, work)?;

        let messages =
            self.logical_messages::<RECORD_WORK>(&active.included, &mut warnings, true, work)?;
        let usage = aggregate_usage::<RECORD_WORK>(&messages, work)?;
        let sidechain_rows = self.sidechain_row_count::<RECORD_WORK>(&acknowledgement, work);
        let sidechain_indices = self.sidechain_indices::<RECORD_WORK>(&acknowledgement, work)?;
        debug_assert!(
            sidechain_indices.len() as u64 <= sidechain_rows,
            "the sidechain row count must be a superset of the sidechain messages it groups"
        );
        // Sidechains can be concurrent branches, so their append order may
        // legitimately interleave fragments from distinct logical messages.
        // The main chain is linear and retains the stronger A/B/A rejection;
        // sidechain grouping instead relies on stable message/request identity
        // plus the existing per-message conflict checks.
        let sidechain_messages =
            self.logical_messages::<RECORD_WORK>(&sidechain_indices, &mut warnings, false, work)?;
        let sidechain_usage = aggregate_usage::<RECORD_WORK>(&sidechain_messages, work)?;
        let combined_usage = combine_usage(&usage, &sidechain_usage)?;
        let tools = self.tools::<RECORD_WORK>(&active.included, &mut warnings, work)?;
        let mut turn_duration_seen = false;
        let mut stop_hook_summary_seen = false;
        // Retries are counted, not flagged, so the whole path is walked: one
        // network incident emits a ladder of `api_error` rows and the ladder's
        // length is the reportable fact.
        let mut api_error_retries_seen: u64 = 0;
        for index in &active.path {
            record_work::<RECORD_WORK>(work, 1);
            if let RowKind::System(system) = &self.rows[*index].row.kind {
                match system.subtype.as_deref() {
                    Some("turn_duration") => turn_duration_seen = true,
                    Some("stop_hook_summary") => stop_hook_summary_seen = true,
                    Some("api_error") => {
                        api_error_retries_seen = api_error_retries_seen.saturating_add(1);
                    }
                    _ => {}
                }
            }
        }
        let status = self.turn_status::<RECORD_WORK>(&messages, active.leaf_index, work)?;
        record_work::<RECORD_WORK>(work, active.path.len());
        let active_chain = active
            .path
            .iter()
            .filter_map(|index| self.rows[*index].row.common.uuid.clone())
            .collect();

        Ok(TranscriptAnalysis {
            status,
            acknowledgement: Some(acknowledgement),
            active_chain,
            messages,
            tools,
            usage,
            sidechain_usage,
            combined_usage,
            turn_duration_seen,
            stop_hook_summary_seen,
            api_error_retries_seen,
            sidechain_rows,
            warnings,
        })
    }

    /// Every row this turn appended on a sidechain, of any kind.
    ///
    /// DELIBERATELY WIDER than [`Self::sidechain_indices`], which keeps only
    /// `Assistant` rows that carry a uuid and are provably descended from the
    /// acknowledged prompt, because that set exists to be aggregated into
    /// tokens and this one exists to answer "did a sidechain happen at all".
    /// The three conditions it drops are the three ways a sidechain row can
    /// carry no usage: the `user` row that opens a `Task`, a row whose uuid the
    /// writer had not flushed, and a row whose parent chain is not yet
    /// reconstructible. A caller that refuses on tokens alone misses all three,
    /// which is precisely the residue
    /// [`crate::TranscriptAnalysis::sidechain_rows`] exists to close.
    ///
    /// The ordinal bound is the same one `sidechain_indices` applies, so a
    /// sidechain belonging to an earlier turn is not charged to this one.
    fn sidechain_row_count<const RECORD_WORK: bool>(
        &self,
        acknowledgement: &PromptAcknowledgement,
        work: &mut TranscriptAnalysisWork,
    ) -> u64 {
        let mut count = 0;
        for stored in &self.rows {
            record_work::<RECORD_WORK>(work, 1);
            if stored.ordinal >= acknowledgement.ordinal
                && stored.row.common.scope == RowScope::Sidechain
            {
                count += 1;
            }
        }
        count
    }

    pub fn rows(&self) -> impl Iterator<Item = &ParsedRow> {
        self.rows.iter().map(|stored| &stored.row)
    }

    #[must_use]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn active_indices<const RECORD_WORK: bool>(
        &self,
        acknowledgement: &PromptAcknowledgement,
        work: &mut TranscriptAnalysisWork,
    ) -> Result<ActiveIndices, TranscriptError> {
        match self.mode {
            ParseMode::Strict => self.strict_active_indices::<RECORD_WORK>(acknowledgement, work),
        }
    }

    fn strict_active_indices<const RECORD_WORK: bool>(
        &self,
        acknowledgement: &PromptAcknowledgement,
        work: &mut TranscriptAnalysisWork,
    ) -> Result<ActiveIndices, TranscriptError> {
        let ack_index = self
            .by_uuid
            .get(&acknowledgement.row_uuid)
            .copied()
            .ok_or_else(|| TranscriptError::SchemaDrift {
                row_uuid: Some(acknowledgement.row_uuid.clone()),
                path: "$.uuid".to_owned(),
                message: "acknowledged row disappeared from the UUID index".to_owned(),
            })?;

        let mut graph_rows = Vec::new();
        for (index, stored) in self.rows.iter().enumerate() {
            record_work::<RECORD_WORK>(work, 1);
            if stored.ordinal >= acknowledgement.ordinal
                && stored.row.common.scope == RowScope::Main
                && !matches!(stored.row.kind, RowKind::Metadata { .. })
            {
                graph_rows.push(index);
            }
        }

        let mut uuid_to_index = HashMap::new();
        let mut graph_children: HashMap<&str, Vec<usize>> = HashMap::new();
        for index in &graph_rows {
            record_work::<RECORD_WORK>(work, 1);
            let stored = &self.rows[*index];
            if let Some(uuid) = stored.row.common.uuid.as_deref() {
                uuid_to_index.insert(uuid, *index);
            }
            if let Some(parent_uuid) = stored.row.common.parent_uuid.as_deref() {
                graph_children.entry(parent_uuid).or_default().push(*index);
            }
        }

        let mut reachable_indices = HashSet::from([ack_index]);
        let mut pending = vec![acknowledgement.row_uuid.as_str()];
        while let Some(parent_uuid) = pending.pop() {
            let Some(children) = graph_children.get(parent_uuid) else {
                continue;
            };
            for index in children {
                record_work::<RECORD_WORK>(work, 1);
                if reachable_indices.insert(*index)
                    && let Some(uuid) = self.rows[*index].row.common.uuid.as_deref()
                {
                    pending.push(uuid);
                }
            }
        }

        let mut reachable = Vec::with_capacity(reachable_indices.len());
        for index in &graph_rows {
            record_work::<RECORD_WORK>(work, 1);
            if reachable_indices.contains(index) {
                reachable.push(*index);
            }
        }
        for index in &reachable {
            record_work::<RECORD_WORK>(work, 1);
            if *index == ack_index {
                continue;
            }
            let stored = &self.rows[*index];
            let uuid =
                stored
                    .row
                    .common
                    .uuid
                    .as_ref()
                    .ok_or(TranscriptError::ActiveRowMissingUuid {
                        ordinal: stored.ordinal,
                    })?;
            let parent_uuid = stored.row.common.parent_uuid.as_ref().ok_or_else(|| {
                TranscriptError::SchemaDrift {
                    row_uuid: Some(uuid.clone()),
                    path: "$.parentUuid".to_owned(),
                    message: "active main-chain row requires a parent UUID".to_owned(),
                }
            })?;
            let parent_index = uuid_to_index
                .get(parent_uuid.as_str())
                .copied()
                .ok_or_else(|| TranscriptError::SchemaDrift {
                    row_uuid: Some(uuid.clone()),
                    path: "$.parentUuid".to_owned(),
                    message: "active main-chain row has no resolvable parent".to_owned(),
                })?;
            if self.rows[parent_index].ordinal >= stored.ordinal {
                return Err(TranscriptError::ParentAppendOrder {
                    row_uuid: uuid.clone(),
                    parent_uuid: parent_uuid.clone(),
                });
            }
        }

        for index in &graph_rows {
            record_work::<RECORD_WORK>(work, 1);
            if *index == ack_index || reachable_indices.contains(index) {
                continue;
            }
            let row = &self.rows[*index].row;
            if is_live_semantic_row(&row.kind) {
                return Err(TranscriptError::DisconnectedActiveRow {
                    row_uuid: row
                        .common
                        .uuid
                        .clone()
                        .unwrap_or_else(|| format!("ordinal:{}", self.rows[*index].ordinal)),
                });
            }
        }

        let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
        for index in reachable
            .iter()
            .copied()
            .filter(|index| *index != ack_index)
        {
            record_work::<RECORD_WORK>(work, 1);
            let parent_uuid = self.rows[index]
                .row
                .common
                .parent_uuid
                .as_deref()
                .expect("reachable active rows were validated to have parents");
            children.entry(parent_uuid).or_default().push(index);
        }
        let leaf_count = reachable
            .iter()
            .filter(|index| {
                record_work::<RECORD_WORK>(work, 1);
                self.rows[**index]
                    .row
                    .common
                    .uuid
                    .as_deref()
                    .is_some_and(|uuid| !children.contains_key(uuid))
            })
            .count();
        if leaf_count > 1 {
            return Err(TranscriptError::AmbiguousActiveBranches { leaf_count });
        }

        let mut path = vec![ack_index];
        let mut current_uuid = acknowledgement.row_uuid.as_str();
        let mut visited = HashSet::from([current_uuid]);
        while let Some(next) = children.get(current_uuid) {
            record_work::<RECORD_WORK>(work, 1);
            let [next] = next.as_slice() else {
                return Err(TranscriptError::AmbiguousActiveBranches {
                    leaf_count: next.len(),
                });
            };
            path.push(*next);
            current_uuid = self.rows[*next]
                .row
                .common
                .uuid
                .as_deref()
                .expect("reachable active rows were validated to have UUIDs");
            if !visited.insert(current_uuid) {
                return Err(TranscriptError::ParentCycle {
                    uuid: current_uuid.to_owned(),
                });
            }
        }
        if path.len() != reachable.len() {
            return Err(TranscriptError::AmbiguousActiveBranches {
                leaf_count: leaf_count.max(2),
            });
        }
        self.validate_strict_active_path::<RECORD_WORK>(&path, work)?;
        Ok(ActiveIndices {
            leaf_index: *path
                .last()
                .expect("the path always contains the acknowledgement"),
            included: path.clone(),
            path,
        })
    }

    fn is_descendant_cached<'a, const RECORD_WORK: bool>(
        &'a self,
        uuid: &'a str,
        root: &str,
        allow_sidechain: bool,
        cache: &mut HashMap<&'a str, bool>,
        work: &mut TranscriptAnalysisWork,
    ) -> Result<bool, TranscriptError> {
        let mut current = Some(uuid);
        let mut path = Vec::new();
        let mut visited = HashSet::new();
        let reachable = loop {
            let Some(candidate) = current else {
                break false;
            };
            record_work::<RECORD_WORK>(work, 1);
            if let Some(reachable) = cache.get(candidate).copied() {
                break reachable;
            }
            if !visited.insert(candidate) {
                return Err(TranscriptError::ParentCycle {
                    uuid: candidate.to_owned(),
                });
            }
            let Some(index) = self.by_uuid.get(candidate).copied() else {
                break false;
            };
            let row = &self.rows[index].row;
            if row.common.scope != RowScope::Main
                && !(allow_sidechain && row.common.scope == RowScope::Sidechain)
            {
                break false;
            }
            if candidate == root {
                break true;
            }
            path.push(candidate);
            current = row.common.parent_uuid.as_deref();
        };
        for candidate in path {
            cache.insert(candidate, reachable);
        }
        Ok(reachable)
    }

    fn sidechain_indices<const RECORD_WORK: bool>(
        &self,
        acknowledgement: &PromptAcknowledgement,
        work: &mut TranscriptAnalysisWork,
    ) -> Result<Vec<usize>, TranscriptError> {
        let mut indices = Vec::new();
        let mut descendant_cache = HashMap::new();
        for (index, stored) in self.rows.iter().enumerate() {
            record_work::<RECORD_WORK>(work, 1);
            if stored.ordinal < acknowledgement.ordinal
                || stored.row.common.scope != RowScope::Sidechain
                || !matches!(stored.row.kind, RowKind::Assistant(_))
            {
                continue;
            }
            let Some(uuid) = stored.row.common.uuid.as_deref() else {
                continue;
            };
            if self.is_descendant_cached::<RECORD_WORK>(
                uuid,
                &acknowledgement.row_uuid,
                true,
                &mut descendant_cache,
                work,
            )? {
                indices.push(index);
            }
        }
        Ok(indices)
    }

    fn message_key(
        &self,
        stored: &StoredRow,
    ) -> Result<Option<LogicalMessageKey>, TranscriptError> {
        let RowKind::Assistant(fragment) = &stored.row.kind else {
            return Ok(None);
        };
        if let Some(message_id) = fragment.message_id.as_ref() {
            return Ok(Some(LogicalMessageKey::MessageId(message_id.clone())));
        }
        if let Some(request_id) = fragment.request_id.as_ref() {
            return Ok(Some(LogicalMessageKey::RequestId(request_id.clone())));
        }
        stored
            .row
            .common
            .uuid
            .as_ref()
            .map(|uuid| Some(LogicalMessageKey::RowUuid(uuid.clone())))
            .ok_or_else(|| TranscriptError::SchemaDrift {
                row_uuid: None,
                path: "$.uuid".to_owned(),
                message: "assistant fragment has no grouping identity".to_owned(),
            })
    }

    fn logical_messages<const RECORD_WORK: bool>(
        &self,
        included: &[usize],
        warnings: &mut Vec<EngineWarning>,
        require_contiguity: bool,
        work: &mut TranscriptAnalysisWork,
    ) -> Result<Vec<LogicalAssistantMessage>, TranscriptError> {
        if self.mode == ParseMode::Strict && require_contiguity {
            self.validate_logical_message_contiguity::<RECORD_WORK>(included, work)?;
        }

        let mut messages: Vec<LogicalAssistantMessage> = Vec::new();
        let mut positions: HashMap<LogicalMessageKey, usize> = HashMap::new();
        for index in included {
            record_work::<RECORD_WORK>(work, 1);
            let stored = &self.rows[*index];
            let RowKind::Assistant(fragment) = &stored.row.kind else {
                continue;
            };
            let key = self
                .message_key(stored)?
                .expect("assistant fragment always has a message key");
            let position = *positions.entry(key.clone()).or_insert_with(|| {
                debug_assert!(
                    messages
                        .last()
                        .is_none_or(|message| message.first_ordinal <= stored.ordinal),
                    "included transcript rows must be in append order"
                );
                let position = messages.len();
                messages.push(LogicalAssistantMessage {
                    key: key.clone(),
                    row_uuids: Vec::new(),
                    model: None,
                    blocks: Vec::new(),
                    stop_reason: None,
                    usage: None,
                    is_api_error: false,
                    first_ordinal: stored.ordinal,
                    last_ordinal: stored.ordinal,
                });
                position
            });
            let message = &mut messages[position];
            if let Some(uuid) = stored.row.common.uuid.as_ref() {
                message.row_uuids.push(uuid.clone());
            }
            merge_optional(
                &mut message.model,
                fragment.model.as_ref(),
                &key,
                "model",
                self.mode,
            )?;
            if let Some(stop) = fragment.stop_reason.as_deref().map(StopReason::parse) {
                merge_optional_owned(
                    &mut message.stop_reason,
                    Some(stop),
                    &key,
                    "stop_reason",
                    self.mode,
                )?;
            }
            if let Some(usage) = fragment.usage.as_ref() {
                match message.usage.as_ref() {
                    None => message.usage = Some(usage.clone()),
                    Some(existing) if existing == usage => {}
                    Some(_) if self.mode == ParseMode::Strict => {
                        return Err(TranscriptError::LogicalMessageConflict {
                            key: key.clone(),
                            field: "usage",
                        });
                    }
                    Some(_) => warnings.push(EngineWarning::ConflictingUsage {
                        message: key.clone(),
                    }),
                }
            }
            for block in &fragment.blocks {
                record_work::<RECORD_WORK>(work, 1);
                if let ContentBlock::Unknown { declared_type, .. } = block {
                    if self.mode == ParseMode::Strict {
                        return Err(TranscriptError::SchemaDrift {
                            row_uuid: stored.row.common.uuid.clone(),
                            path: "$.message.content".to_owned(),
                            message: format!(
                                "unknown or malformed content block {declared_type:?}"
                            ),
                        });
                    }
                    warnings.push(EngineWarning::UnknownContentBlock {
                        message: key.clone(),
                        declared_type: declared_type.clone(),
                    });
                }
                message.blocks.push(block.clone());
            }
            message.is_api_error |= fragment.is_api_error;
            message.last_ordinal = stored.ordinal;
        }
        Ok(messages)
    }

    fn validate_logical_message_contiguity<const RECORD_WORK: bool>(
        &self,
        included: &[usize],
        work: &mut TranscriptAnalysisWork,
    ) -> Result<(), TranscriptError> {
        let mut current = None;
        let mut closed = HashSet::new();
        for index in included {
            record_work::<RECORD_WORK>(work, 1);
            // Claude Code can persist one streamed assistant API response as
            // several fragments while appending an immediately executed tool
            // result between those fragments. The fragments retain the same
            // message/request identity and the parent graph remains linear.
            // Tool-result and typed attachment rows therefore do not close the
            // current logical assistant identity; a different assistant
            // identity (or any other semantic/control row) still does, so
            // A/B/A remains an unambiguous strict-mode error.
            if matches!(
                self.rows[*index].row.kind,
                RowKind::UserToolResults { .. } | RowKind::Attachment { .. }
            ) {
                continue;
            }
            let next = self.message_key(&self.rows[*index])?;
            if next == current {
                continue;
            }
            if let Some(previous) = current.take() {
                closed.insert(previous);
            }
            if let Some(key) = next {
                if closed.contains(&key) {
                    return Err(TranscriptError::InterleavedLogicalMessage { key });
                }
                current = Some(key);
            }
        }
        Ok(())
    }

    fn tools<const RECORD_WORK: bool>(
        &self,
        included: &[usize],
        warnings: &mut Vec<EngineWarning>,
        work: &mut TranscriptAnalysisWork,
    ) -> Result<Vec<ToolRecord>, TranscriptError> {
        let mut tools: Vec<ToolRecord> = Vec::new();
        let mut positions: HashMap<String, usize> = HashMap::new();
        let mut order = 0;
        for index in included {
            record_work::<RECORD_WORK>(work, 1);
            match &self.rows[*index].row.kind {
                RowKind::Assistant(fragment) => {
                    for block in &fragment.blocks {
                        record_work::<RECORD_WORK>(work, 1);
                        let ContentBlock::ToolUse { id, name, input } = block else {
                            continue;
                        };
                        if let Some(position) = positions.get(id).copied() {
                            let existing = &tools[position];
                            if existing.name == *name && existing.input == *input {
                                continue;
                            }
                            return Err(TranscriptError::DuplicateToolCall {
                                tool_use_id: id.clone(),
                            });
                        }
                        positions.insert(id.clone(), tools.len());
                        tools.push(ToolRecord {
                            tool_use_id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                            result: None,
                            order,
                        });
                        order += 1;
                    }
                }
                RowKind::UserToolResults { results } => {
                    for result in results {
                        record_work::<RECORD_WORK>(work, 1);
                        let Some(position) = positions.get(&result.tool_use_id).copied() else {
                            if self.mode == ParseMode::Strict {
                                return Err(TranscriptError::OrphanToolResult {
                                    tool_use_id: result.tool_use_id.clone(),
                                });
                            }
                            warnings.push(EngineWarning::OrphanToolResult {
                                tool_use_id: result.tool_use_id.clone(),
                            });
                            continue;
                        };
                        if let Some(existing) = tools[position].result.as_ref() {
                            if existing == result {
                                continue;
                            }
                            return Err(TranscriptError::DuplicateToolResult {
                                tool_use_id: result.tool_use_id.clone(),
                            });
                        }
                        tools[position].result = Some(result.clone());
                    }
                }
                _ => {}
            }
        }
        Ok(tools)
    }

    fn turn_status<const RECORD_WORK: bool>(
        &self,
        messages: &[LogicalAssistantMessage],
        leaf_index: usize,
        work: &mut TranscriptAnalysisWork,
    ) -> Result<TurnStatus, TranscriptError> {
        record_work::<RECORD_WORK>(work, messages.len());
        let latest = messages.iter().max_by_key(|message| message.last_ordinal);
        let Some(message) = latest else {
            return Ok(TurnStatus::Running {
                latest_stop_reason: None,
            });
        };
        // Not every admitted system row means the turn is over. The proven-inert
        // markers do. `api_error` proves the opposite -- a retry is in flight --
        // so it must never inherit terminal compatibility from its `type`: with
        // an `api_error` leaf, the latest logical message still carries the
        // pre-retry stop reason, so treating the leaf as terminal-compatible
        // would let pmux commit the pre-retry text as the answer and complete a
        // turn mid-retry during a dropped connection. Exhausted retry ladders
        // take this same non-terminal path (exhaustion is unobserved), which
        // refuses to return rather than returning early.
        let leaf_allows_terminal = match &self.rows[leaf_index].row.kind {
            RowKind::Assistant(_) => true,
            RowKind::System(system) => !system.is_retry_in_flight_marker(),
            _ => false,
        };
        if !leaf_allows_terminal {
            return Ok(TurnStatus::Running {
                latest_stop_reason: message.stop_reason.clone(),
            });
        }

        let outcome = if message.is_api_error {
            Some(TerminalOutcome::ApiError)
        } else {
            match message.stop_reason.as_ref() {
                Some(StopReason::EndTurn) => Some(TerminalOutcome::Completed),
                Some(StopReason::MaxTokens) => Some(TerminalOutcome::MaxTokens),
                Some(StopReason::Refusal) => Some(TerminalOutcome::Refused),
                Some(StopReason::StopSequence) => Some(TerminalOutcome::Completed),
                Some(StopReason::Unknown(value)) if self.mode == ParseMode::Strict => {
                    return Err(TranscriptError::SchemaDrift {
                        row_uuid: message.row_uuids.last().cloned(),
                        path: "$.message.stop_reason".to_owned(),
                        message: format!("unknown stop reason {value:?}"),
                    });
                }
                _ => None,
            }
        };

        let Some(outcome) = outcome else {
            return Ok(TurnStatus::Running {
                latest_stop_reason: message.stop_reason.clone(),
            });
        };
        let mut final_text_blocks = Vec::new();
        for block in &message.blocks {
            record_work::<RECORD_WORK>(work, 1);
            if let ContentBlock::Text { text } = block {
                final_text_blocks.push(text.clone());
            }
        }
        if self.mode == ParseMode::Strict
            && final_text_blocks.is_empty()
            && matches!(
                outcome,
                TerminalOutcome::Completed | TerminalOutcome::MaxTokens
            )
        {
            return Err(TranscriptError::TerminalMessageMissingText {
                key: message.key.clone(),
            });
        }
        let final_text = final_text_blocks.concat();

        Ok(TurnStatus::Terminal(FinalTurn {
            outcome,
            message_key: message.key.clone(),
            stop_reason: message.stop_reason.clone(),
            final_text,
            final_text_blocks,
            model: message.model.clone(),
        }))
    }

    fn validate_active_unknowns<const RECORD_WORK: bool>(
        &self,
        path: &[usize],
        work: &mut TranscriptAnalysisWork,
    ) -> Result<(), TranscriptError> {
        for index in path {
            record_work::<RECORD_WORK>(work, 1);
            if let RowKind::Unknown { declared_type } = &self.rows[*index].row.kind
                && self.mode == ParseMode::Strict
            {
                return Err(TranscriptError::SchemaDrift {
                    row_uuid: self.rows[*index].row.common.uuid.clone(),
                    path: "$.type".to_owned(),
                    message: format!(
                        "unknown row type {declared_type:?} is on the active parent chain"
                    ),
                });
            }
        }
        Ok(())
    }

    fn validate_strict_active_path<const RECORD_WORK: bool>(
        &self,
        path: &[usize],
        work: &mut TranscriptAnalysisWork,
    ) -> Result<(), TranscriptError> {
        // The subtype of the first allowlisted marker seen on the chain, which
        // is also the name of the trailing zone it opened. `turn_duration` and
        // a proven-inert `stop_hook_summary` share one zone rather than owning
        // parallel flags: what matters is that *some* marker has been passed,
        // after which any semantic row is drift.
        let mut inert_marker: Option<&str> = None;
        for index in path.iter().skip(1) {
            record_work::<RECORD_WORK>(work, 1);
            let stored = &self.rows[*index];
            match &stored.row.kind {
                RowKind::Unknown { declared_type } => {
                    return Err(TranscriptError::SchemaDrift {
                        row_uuid: stored.row.common.uuid.clone(),
                        path: "$.type".to_owned(),
                        message: format!(
                            "unknown row type {declared_type:?} is on the active parent chain"
                        ),
                    });
                }
                RowKind::UserOther => {
                    return Err(TranscriptError::SchemaDrift {
                        row_uuid: stored.row.common.uuid.clone(),
                        path: "$.message".to_owned(),
                        message: "unrecognized user row is on the active parent chain".to_owned(),
                    });
                }
                RowKind::System(system) => {
                    if !system.is_admitted_on_active_chain() {
                        return Err(TranscriptError::SchemaDrift {
                            row_uuid: stored.row.common.uuid.clone(),
                            path: "$.subtype".to_owned(),
                            message: format!(
                                "unsupported active system subtype {:?}",
                                system.subtype
                            ),
                        });
                    }
                    // Markers may chain through one another: the observed
                    // `turn_duration` row parents onto the `stop_hook_summary`
                    // row, so both live inside the zone the first one opened.
                    //
                    // Only inert markers open the zone. An `api_error` row is
                    // admitted without opening one, because the semantic row
                    // that follows a retry is the retry *succeeding* -- the
                    // ordinary case, and the whole point of admitting the row.
                    if system.is_proven_inert_marker() && inert_marker.is_none() {
                        inert_marker = system.subtype.as_deref();
                    }
                }
                RowKind::Assistant(_)
                | RowKind::TypedUser { .. }
                | RowKind::UserToolResults { .. }
                | RowKind::Attachment { .. } => {
                    if let Some(marker) = inert_marker {
                        return Err(TranscriptError::SchemaDrift {
                            row_uuid: stored.row.common.uuid.clone(),
                            path: "$.type".to_owned(),
                            message: format!(
                                "{marker} must be trailing on the active parent chain"
                            ),
                        });
                    }
                }
                RowKind::Metadata { .. } => {
                    unreachable!("metadata rows are excluded from the active graph")
                }
            }
        }
        Ok(())
    }

    fn unknown_warnings_after<const RECORD_WORK: bool>(
        &self,
        ordinal: u64,
        work: &mut TranscriptAnalysisWork,
    ) -> Vec<EngineWarning> {
        let mut warnings = Vec::new();
        for stored in &self.rows {
            record_work::<RECORD_WORK>(work, 1);
            if stored.ordinal < ordinal {
                continue;
            }
            if let RowKind::Unknown { declared_type } = &stored.row.kind {
                warnings.push(EngineWarning::UnknownRow {
                    ordinal: stored.ordinal,
                    declared_type: declared_type.clone(),
                });
            }
        }
        warnings
    }
}

#[derive(Debug)]
struct ActiveIndices {
    path: Vec<usize>,
    included: Vec<usize>,
    leaf_index: usize,
}

fn is_live_semantic_row(kind: &RowKind) -> bool {
    matches!(
        kind,
        RowKind::TypedUser { .. }
            | RowKind::UserToolResults { .. }
            | RowKind::UserOther
            | RowKind::Assistant(_)
    )
}

fn merge_optional(
    target: &mut Option<String>,
    candidate: Option<&String>,
    key: &LogicalMessageKey,
    field: &'static str,
    mode: ParseMode,
) -> Result<(), TranscriptError> {
    merge_optional_owned(target, candidate.cloned(), key, field, mode)
}

fn merge_optional_owned<T: Clone + PartialEq>(
    target: &mut Option<T>,
    candidate: Option<T>,
    key: &LogicalMessageKey,
    field: &'static str,
    mode: ParseMode,
) -> Result<(), TranscriptError> {
    let Some(candidate) = candidate else {
        return Ok(());
    };
    match target {
        None => *target = Some(candidate),
        Some(existing) if existing == &candidate => {}
        Some(_) if mode == ParseMode::Strict => {
            return Err(TranscriptError::LogicalMessageConflict {
                key: key.clone(),
                field,
            });
        }
        Some(_) => *target = Some(candidate),
    }
    Ok(())
}

fn aggregate_usage<const RECORD_WORK: bool>(
    messages: &[LogicalAssistantMessage],
    work: &mut TranscriptAnalysisWork,
) -> Result<UsageTotals, TranscriptError> {
    let mut totals = UsageTotals::default();
    for message in messages {
        record_work::<RECORD_WORK>(work, 1);
        let Some(usage) = message.usage.as_ref() else {
            continue;
        };
        totals.model_calls_with_usage =
            checked_add(totals.model_calls_with_usage, 1, "model_calls_with_usage")?;
        add_tokens(&mut totals.tokens, &usage.tokens)?;
    }
    Ok(totals)
}

fn combine_usage(
    main: &UsageTotals,
    sidechain: &UsageTotals,
) -> Result<UsageTotals, TranscriptError> {
    let mut combined = main.clone();
    combined.model_calls_with_usage = checked_add(
        combined.model_calls_with_usage,
        sidechain.model_calls_with_usage,
        "combined_model_calls_with_usage",
    )?;
    add_tokens(&mut combined.tokens, &sidechain.tokens)?;
    Ok(combined)
}

fn add_tokens(target: &mut TokenUsage, source: &TokenUsage) -> Result<(), TranscriptError> {
    target.input_tokens = checked_add(target.input_tokens, source.input_tokens, "input_tokens")?;
    target.output_tokens =
        checked_add(target.output_tokens, source.output_tokens, "output_tokens")?;
    target.cache_creation_input_tokens = checked_add(
        target.cache_creation_input_tokens,
        source.cache_creation_input_tokens,
        "cache_creation_input_tokens",
    )?;
    target.cache_read_input_tokens = checked_add(
        target.cache_read_input_tokens,
        source.cache_read_input_tokens,
        "cache_read_input_tokens",
    )?;
    Ok(())
}

fn checked_add(left: u64, right: u64, field: &'static str) -> Result<u64, TranscriptError> {
    left.checked_add(right)
        .filter(|total| *total <= MAX_SAFE_JSON_INTEGER)
        .ok_or(TranscriptError::UsageOverflow { field })
}

/// The canonical form of a typed prompt: the exact form Claude records one in.
///
/// Applied to BOTH ends of the only equality a turn is proven by --
/// [`TranscriptEngine::arm_turn`] normalizes the prompt pmux is about to type
/// and [`TranscriptEngine::ingest`] normalizes the prompt Claude wrote down --
/// so every transformation Claude applies on the way has to be applied here or
/// the turn can never be acknowledged.
///
/// Three transformations, all MEASURED rather than assumed:
///
/// 1. **Platform line endings** fold to `\n`.
/// 2. **Unicode canonical composition (NFC).** MEASURED at Claude Code 2.1.226
///    through a real Path B turn: pmux typed `Nonce N2. e` + U+0301 and the
///    child's own transcript row came back carrying U+00E9. Every recorded row
///    read back in that session was already NFC. Before this line, a prompt
///    carrying any decomposed sequence -- an accented character copied off
///    macOS, where NFD is the filesystem's own form -- armed a turn that could
///    not be acknowledged, so it failed `PromptNotAcknowledged` and the pool
///    destroyed the instance proving it. `docs/path-b-adversarial.md` sec. 5.2.
///
///    NFC and not a refusal, because NFC(x) and x are the SAME STRING by
///    Unicode's own definition of canonical equivalence: normalizing changes
///    the bytes and not the text, which is the one rewrite this function is
///    entitled to make. The same reasoning already governs
///    [`crate::TranscriptLocator`]'s cwd identity, for the same measured reason
///    about the same program.
///
/// 3. **The composer's own trailing trim**, [`crate::composer_submitted_text`].
///    MEASURED at Claude Code 2.1.226 through real Path B turns, reading the
///    child's own rows back: a prompt ending in three U+0020, in `\n`, in U+FEFF
///    or in U+3000 was recorded WITHOUT them, and each of those turns therefore
///    failed `PromptNotAcknowledged` and cost the pooled instance. A prompt
///    ending in U+200B was recorded WITH it, which is why the rule is that
///    function's measured set and not "invisible characters".
///    `docs/path-b-adversarial.md` sec. 11.
///
///    **It removes nothing pmux would refuse.** [`crate::is_trimmed_from_the_end`]
///    subtracts [`crate::is_refused_wherever_it_stands`], so a character a
///    caller is told about when it stands INSIDE a prompt cannot be deleted
///    without a word when it stands at the end. Four characters were in both
///    sets and the delete ran first: U+0009, U+000B, U+000C and U+0085. The last
///    of them is the one that says why this matters — Claude Code 2.1.227 was
///    MEASURED recording a trailing U+0085 verbatim, so deleting it was not
///    matching the composer, it was answering a prompt the caller did not send
///    (`docs/path-b-adversarial.md` sec. 12).
///
///    This one removes characters a caller wrote, which the other two do not, so
///    it is the transformation that needs the argument. The argument is this
///    function's first line: the canonical form of a typed prompt is the exact
///    form Claude records one in, and Claude was measured recording it trimmed.
///    pmux's alternatives were to refuse every prompt ending in whitespace --
///    which would refuse `echo q | pmux ask`, since a text file ends in a
///    terminator -- or to keep arming turns that cannot be acknowledged. The
///    third rule is also what makes an all-whitespace prompt reach the
///    empty-prompt refusal every entry point already has, rather than needing a
///    rule of its own: such a buffer is one Enter NEVER submits, and before this
///    line pmux typed it and waited 600 000 ms for an acknowledgement.
///
/// What it deliberately does NOT do is expand tabs. The composer records U+0009
/// as four U+0020, but four spaces is not canonically equivalent to a tab, so
/// pmux refuses such a prompt at
/// [`crate::composer_refusal`] rather than inventing three characters the
/// caller did not write. It also does not touch a trailing `\`, which the
/// composer does not trim and does not submit either; that is
/// [`crate::ComposerRefusal::LineContinuation`], because there is no rewrite
/// that would deliver it.
#[must_use]
pub fn normalize_prompt(prompt: &str) -> String {
    use unicode_normalization::UnicodeNormalization;

    let composed: String = prompt
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect();
    crate::composer_submitted_text(&composed).to_owned()
}
