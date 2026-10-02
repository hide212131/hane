use super::*;

impl EditorView {
    /// Coalesced background job producing the formal, document-wide `BlockIndex`.
    /// One job at a time; a result that no longer matches the document revision
    /// is rebased or re-scheduled rather than published stale.
    pub(super) fn schedule_document_parse(&mut self, cx: &mut Context<Self>) {
        let document = self.sessions.active().editor().document();
        let revision = document.revision();
        let disclosure = self.active_height_disclosure();
        let force_height_disclosure_snapshot = self.force_height_disclosure_snapshot;
        let disclosure_refresh = disclosure
            .filter(|disclosure| !disclosure.is_empty())
            .is_some_and(|disclosure| {
                self.last_background_height_disclosure != Some((revision, disclosure))
            });
        let disclosure_collapse = disclosure
            .filter(|disclosure| disclosure.is_empty())
            .is_some_and(|_| {
                self.last_background_height_disclosure.is_some_and(
                    |(background_revision, background)| {
                        background_revision == revision && !background.is_empty()
                    },
                )
            });
        if !self.block_index.needs_formal_parse(document)
            && !disclosure_refresh
            && !disclosure_collapse
            && !force_height_disclosure_snapshot
        {
            return;
        }
        if self.document_parse_job_running {
            return;
        }
        self.force_height_disclosure_snapshot = false;
        self.document_parse_job_running = true;
        let key = self.document_key();
        let line_height = self.line_height();
        let line_height_bits = line_height.to_bits();
        let previous_height_disclosure = self
            .last_applied_height_disclosure
            .filter(|(previous_revision, _)| *previous_revision == revision)
            .map(|(_, disclosure)| disclosure);
        let snapshot_is_collapsing_disclosure = previous_height_disclosure
            .is_some_and(|previous| !previous.is_empty())
            && disclosure.is_some_and(|disclosure| disclosure.is_empty());
        if let Some(disclosure) = disclosure {
            self.last_background_height_disclosure = Some((revision, disclosure));
        }
        let snapshot = self.editor().document().clone();
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(40))
                .await;
            let current = view
                .update(cx, |view, _| {
                    view.document_key() == key
                        && block_context_revision_is_current(
                            view.editor().document().revision(),
                            revision,
                        )
                        && height_snapshot_matches_line_height(
                            view.line_height(),
                            f32::from_bits(line_height_bits),
                        )
                })
                .unwrap_or(false);
            if !current {
                let _ = view.update(cx, |view, cx| {
                    view.document_parse_job_running = false;
                    view.schedule_document_parse(cx);
                });
                return;
            }
            // Sizing the height index is proportional to the block count, so it
            // is done here rather than on the main thread: for a 100 MB document
            // that is tens of milliseconds that would otherwise land in one
            // frame.
            let (index, heights) = cx
                .background_executor()
                .spawn(async move {
                    let index = BlockIndex::from_buffer(&snapshot);
                    let heights = HeightIndex::new(block_heights_with_disclosure(
                        &snapshot,
                        &index,
                        line_height,
                        disclosure,
                    ));
                    (index, heights)
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.document_parse_job_running = false;
                if view.document_key() != key
                    || !height_snapshot_matches_line_height(
                        view.line_height(),
                        f32::from_bits(line_height_bits),
                    )
                {
                    // The index itself may still be current, but these
                    // heights were measured for an older zoom/theme. Keep the
                    // stale snapshot out of the visible height tree and rerun
                    // the job with the current line height.
                    view.schedule_document_parse(cx);
                    return;
                }
                let document = view.sessions.active().editor().document();
                let publish_outcome =
                    view.block_index
                        .publish(index, IndexSource::Formal, document);
                let index_was_updated = matches!(
                    publish_outcome,
                    PublishOutcome::Published | PublishOutcome::Rebased(_)
                );
                if index_was_updated {
                    view.background_presentation_generation = revision.0 + 1;
                    // Formal boundaries can disagree with what the bounded
                    // local parse showed, so every cached presentation is
                    // re-derived once when the index actually changed.
                    view.block_cache.clear();
                    view.joined_parse_cache.clear();
                }
                let (granularity, len) = view.desired_layout();
                let snapshot_disclosure_is_current = view.editor().document().revision()
                    == revision
                    && view.active_height_disclosure() == disclosure;
                if snapshot_disclosure_is_current
                    && granularity == Granularity::Blocks
                    && len == heights.len()
                {
                    if publish_outcome == PublishOutcome::NotMoreAuthoritative {
                        // The formal index is already current in this case;
                        // this job only refreshed disclosure-dependent fence
                        // heights. Preserve measured wrapping/image heights and
                        // invalidate presentations lazily through their
                        // disclosure check instead of throwing their caches
                        // away for a selection change.
                        view.install_disclosure_heights_preserving_measurements(
                            heights,
                            previous_height_disclosure,
                        );
                    } else if index_was_updated {
                        view.install_heights(granularity, heights);
                    }
                    if let Some(disclosure) = disclosure {
                        view.last_background_height_disclosure = Some((revision, disclosure));
                    }
                } else {
                    // The parse was rebased onto edits, or the caret/IME moved
                    // while it ran, so the prepared heights no longer describe
                    // the current disclosure. A changed selection, including
                    // a non-empty selection collapsing to a caret, is retried
                    // in another background snapshot; rebuilding all selected
                    // blocks here would put the same document-sized walk back
                    // on the input thread at the completion boundary. The
                    // bounded active-end update keeps the caret addressable
                    // until that snapshot lands.
                    let current_disclosure = view.active_height_disclosure();
                    let selection_snapshot_requires_retry = current_disclosure != disclosure
                        && (current_disclosure.is_some_and(|disclosure| !disclosure.is_empty())
                            || disclosure.is_some_and(|disclosure| !disclosure.is_empty())
                            || snapshot_is_collapsing_disclosure);
                    if selection_snapshot_requires_retry {
                        if snapshot_is_collapsing_disclosure
                            && current_disclosure.is_some_and(|disclosure| disclosure.is_empty())
                        {
                            // Both snapshots are caret disclosures, so the
                            // usual non-empty comparison cannot make the
                            // queued job distinguish the latest caret from
                            // the one that was captured before it started.
                            view.force_height_disclosure_snapshot = true;
                        }
                        view.schedule_document_parse(cx);
                        view.ensure_active_disclosure_height();
                    } else if current_disclosure != disclosure {
                        // Moving between two caret disclosures only needs the
                        // bounded endpoint update; a whole-document snapshot
                        // would make ordinary cursor motion unnecessarily
                        // expensive.
                        view.ensure_active_disclosure_height();
                    } else {
                        view.resync_heights_for_current_disclosure();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Coalesced per-block background job producing the whole-span parse of a
    /// joinable block that exceeds either synchronous line or byte budget — the
    /// case `presented_block` itself cannot read and reparse synchronously on
    /// every viewport miss without making a single huge paragraph's render
    /// cost scale with its length. One job per block, bounded across documents; mirrors
    /// [`Self::schedule_document_parse`]'s snapshot-and-spawn shape but at
    /// block granularity, and is what resolves a marker pair arbitrarily far
    /// apart in such a block without a fixed context window whose result
    /// would depend on where the viewport happens to sit.
    pub(super) fn schedule_joined_parse(
        &mut self,
        blocks: &[IndexedBlock],
        cx: &mut Context<Self>,
    ) {
        let revision = self.editor().document().revision();
        for block in blocks {
            if self.joined_parse_jobs_running >= MAX_JOINED_PARSE_JOBS {
                // No backlog of obsolete viewport requests. Completion notifies
                // the view so its current visible blocks can request a free slot.
                break;
            }
            if !block_is_joinable(block.kind) {
                continue;
            }
            // Re-fetched every iteration (cheap: a reference, not a clone) so
            // its borrow never has to outlive the mutable `self` access below.
            let document = self.editor().document();
            let Some(span) = block_line_span(document, block) else {
                continue;
            };
            if block_fits_sync_join_budget(block, &span) {
                continue;
            }
            if self.joined_parse_jobs.contains_key(&block.id)
                || self
                    .joined_parse_cache
                    .get(&block.id)
                    .is_some_and(|cached| {
                        cached.revision == revision && cached.source_range == block.source_range
                    })
            {
                continue;
            }
            let content = span.start
                ..span
                    .end
                    .saturating_sub(trailing_blank_lines(document, &span));
            let snapshot = document.clone();
            let id = block.id;
            let source_range = block.source_range;
            let job = JoinedParseJob {
                document: self.document_key(),
                revision,
                source_range,
            };
            self.joined_parse_jobs.insert(id, job);
            self.joined_parse_jobs_running += 1;
            cx.spawn(async move |view, cx| {
                let parse =
                    cx.background_executor()
                        .spawn(async move {
                            parse_joined_span(&snapshot, content, source_range, revision)
                        })
                        .await;
                let _ = view.update(cx, |view, cx| {
                    // Release capacity even for an old document. Dropping a
                    // Task cannot interrupt synchronous parse already polling;
                    // capacity remains charged until it really finishes.
                    view.joined_parse_jobs_running -= 1;
                    cx.notify();
                    if view.document_key() != job.document
                        || view.joined_parse_jobs.get(&id) != Some(&job)
                    {
                        return;
                    }
                    view.joined_parse_jobs.remove(&id);
                    // Resolve against the already-published current index;
                    // never parse source synchronously to validate a result.
                    // A provisional request can retry once the formal index
                    // arrives and provides an exact block identity and range.
                    let current = view
                        .current_index()
                        .and_then(|index| index.block_at(source_range.start));
                    if view.editor().document().revision() != revision
                        || current.is_none_or(|block| {
                            block.id != id || block.source_range != source_range
                        })
                    {
                        return;
                    }
                    if let Some(parse) = parse {
                        view.joined_parse_cache.insert(
                            id,
                            JoinedBlockCache {
                                revision,
                                source_range,
                                parse,
                            },
                        );
                        // The next viewport miss on this block should read the
                        // cache instead of the presentation this view already
                        // built from a bounded, render-window-only parse.
                        view.block_cache.remove(&id);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }
}
