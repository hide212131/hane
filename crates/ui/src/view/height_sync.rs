use super::*;

impl EditorView {
    /// Initial height of every entry, from the line height alone. Measured
    /// heights replace these as blocks are drawn; R4C keeps them across rebuilds.
    pub(super) fn item_heights(&self) -> Vec<f32> {
        let line_height = self.line_height();
        let (granularity, len) = self.desired_layout();
        match granularity {
            Granularity::Lines => vec![line_height; len],
            Granularity::Blocks => {
                let document = self.editor().document();
                let index = self
                    .current_index()
                    .expect("block granularity has an index");
                let disclosure = self.active_height_disclosure();
                block_heights_with_disclosure(document, index, line_height, disclosure)
            }
        }
    }

    /// Keeps the active endpoint's structural rows in the height index before
    /// the caret-scroll calculation runs. A caret move does not change the
    /// block count, so `resync_heights` quite deliberately keeps its measured
    /// index; that fast path must still expand a zero-height row that was just
    /// made editable or it can disappear from the next virtualization window.
    ///
    /// A non-empty selection is different: its complete disclosure is rebuilt
    /// by the coalesced background parse. Updating every block it spans here
    /// would make shift-selection and mouse dragging proportional to document
    /// size. Only the two selection endpoints and the IME range boundaries are
    /// needed synchronously to keep the caret addressable while that snapshot
    /// is pending.
    pub(super) fn ensure_active_disclosure_height(&mut self) {
        if self.granularity != Granularity::Blocks {
            // A formal parse can publish its index while the view is still on
            // the initial line-granularity height tree. In that transition
            // the bounded endpoint bridge must first install the block tree;
            // otherwise a caret move that races the first parse leaves the
            // newly published index without any block heights at all.
            self.resync_heights();
            if self.granularity != Granularity::Blocks {
                return;
            }
        }
        let Some(disclosure) = self.active_height_disclosure() else {
            return;
        };
        let Some(index) = self.current_index() else {
            return;
        };
        let line_height = self.line_height();
        let revision = self.editor().document().revision();
        let previous = self
            .last_endpoint_height_disclosure
            .filter(|(previous_revision, _)| *previous_revision == revision)
            .map(|(_, disclosure)| disclosure);
        let mut ordinals = Vec::with_capacity(4);
        let mut add_ordinal = |offset: SourceOffset| {
            if let Some(ordinal) = index.ordinal_at(offset)
                && !ordinals.contains(&ordinal)
            {
                ordinals.push(ordinal);
            }
        };
        let selection = self.editor().selection();
        add_ordinal(selection.anchor);
        add_ordinal(selection.active);
        if let Some(previous) = previous {
            add_ordinal(previous.start);
            if !previous.is_empty() {
                add_ordinal(SourceOffset(previous.end.0.saturating_sub(1)));
            }
        }
        if let Some(ime) = self.editor().ime() {
            add_ordinal(ime.current_range.start);
            if !ime.current_range.is_empty() {
                add_ordinal(SourceOffset(ime.current_range.end.0.saturating_sub(1)));
            }
        }
        let updates = ordinals
            .into_iter()
            .filter_map(|ordinal| {
                let block = index.block(ordinal)?;
                let block_is_final = ordinal + 1 == index.len();
                let fence_projection = index.fence_height_projection(&block);
                let table_projection = index.table_projection(&block);
                if fence_projection.is_none() && table_projection.is_none() {
                    return None;
                }
                let collapsed_fence = fence_projection.map_or(0, |projection| {
                    projection.inactive_rows_in(
                        block.source_range,
                        block.source_range,
                        Some(disclosure),
                        block_is_final,
                    )
                });
                let collapsed_table = table_projection
                    .filter(|projection| table_delimiter_is_collapsed(projection, Some(disclosure)))
                    .map_or(0, |_| 1);
                let collapsed = collapsed_fence + collapsed_table;
                let minimum = line_height * block.line_count.saturating_sub(collapsed) as f32;
                let current = self.heights.height(ordinal)?;
                let target = previous.map_or_else(
                    || current.max(minimum),
                    |previous| {
                        let previous_collapsed_fence = fence_projection.map_or(0, |projection| {
                            projection.inactive_rows_in(
                                block.source_range,
                                block.source_range,
                                Some(previous),
                                block_is_final,
                            )
                        });
                        let previous_collapsed_table = table_projection
                            .filter(|projection| {
                                table_delimiter_is_collapsed(projection, Some(previous))
                            })
                            .map_or(0, |_| 1);
                        // `current` may be a measured layout height: a code
                        // row is taller than the plain line-height seed. Move
                        // only the structural-row delta between disclosures so
                        // measured body rows stay measured instead of being
                        // replaced by an arithmetic lower bound.
                        let fence_height_delta = fence_projection.map_or(0.0, |projection| {
                            self.fence_height_delta(
                                &block,
                                projection,
                                Some(previous),
                                Some(disclosure),
                                collapsed_fence as f32 - previous_collapsed_fence as f32,
                                line_height,
                            )
                        });
                        let table_height_delta = table_projection.map_or(0.0, |projection| {
                            self.table_delimiter_height_delta(
                                &block,
                                projection,
                                Some(previous),
                                Some(disclosure),
                                collapsed_table as f32 - previous_collapsed_table as f32,
                                line_height,
                            )
                        });
                        preserve_measured_height_after_fence_delta(
                            current,
                            minimum,
                            fence_height_delta + table_height_delta,
                        )
                    },
                );
                (target != current).then_some((ordinal, target))
            })
            .collect::<Vec<_>>();
        for (ordinal, minimum) in updates {
            self.heights.update(ordinal, minimum);
        }
        // This is the disclosure used by the bounded endpoint bridge, even
        // while a document-sized snapshot is running. Keep it separate from
        // `last_applied_height_disclosure`: the latter remains the complete
        // snapshot baseline used to preserve measured middle blocks when the
        // background result arrives.
        self.last_endpoint_height_disclosure = Some((revision, disclosure));
    }

    /// Returns the amount of measured fence height that changes between two
    /// disclosures. The arithmetic projection supplies the bounded baseline
    /// for every transitioned fence row. When the old disclosure's layout is
    /// cached, replace the baseline for the visible transitioned rows with
    /// their actual fragment height so a long delimiter that wrapped into
    /// several layout rows does not leave residual height behind.
    fn fence_height_delta(
        &self,
        block: &IndexedBlock,
        projection: &FenceHeightProjection,
        previous: Option<SourceRange>,
        current: Option<SourceRange>,
        collapsed_delta: f32,
        line_height: f32,
    ) -> f32 {
        let code_height = code_line_height(line_height);
        let baseline = code_height * collapsed_delta;
        let Some(visual) = self.block_cache.get(&block.id) else {
            return baseline;
        };
        let Some(layout) = self.layout_cache.get(&block.id) else {
            return baseline;
        };
        if !layout.is_valid(
            self.content_width,
            self.layout_font_revision,
            self.editor().document().revision(),
        ) {
            return baseline;
        }
        let Some(block_lines) = block_line_span(self.editor().document(), block) else {
            return baseline;
        };
        let first_presented_line = visual.span.start + visual.lines_before;
        let block_is_final = self
            .current_index()
            .is_some_and(|index| block.ordinal + 1 == index.len());
        let mut seen = HashSet::new();
        let mut correction = 0.0;
        for row in &layout.layout.lines {
            if !seen.insert(row.line) {
                continue;
            }
            let physical_line = first_presented_line + row.line;
            let relative_line = physical_line.saturating_sub(block_lines.start);
            if !projection.is_fence_row_line(relative_line) {
                continue;
            }
            let was_collapsed = fence_row_is_inactive(
                projection,
                block.source_range,
                relative_line,
                previous,
                block_is_final,
            );
            let is_collapsed = fence_row_is_inactive(
                projection,
                block.source_range,
                relative_line,
                current,
                block_is_final,
            );
            if was_collapsed == is_collapsed {
                continue;
            }
            let measured = layout.layout.line_height_of(row.line);
            // A cached layout from the collapsed state has zero height for the
            // row. It cannot tell us the not-yet-laid-out expanded height, so
            // keep the arithmetic fallback for that direction. The reverse
            // transition has the old expanded layout and can use all wrapped
            // fragments it measured.
            if measured <= 0.0 {
                continue;
            }
            let measured_delta = measured - code_height;
            correction += if is_collapsed {
                measured_delta
            } else {
                -measured_delta
            };
        }
        baseline + correction
    }

    /// Returns the amount of height that changes when the table delimiter's
    /// disclosure changes. The arithmetic projection supplies one ordinary
    /// line as the bounded fallback, while a cached layout from the expanded
    /// state supplies the delimiter's actual wrapped height when available.
    fn table_delimiter_height_delta(
        &self,
        block: &IndexedBlock,
        projection: &TableProjection,
        previous: Option<SourceRange>,
        current: Option<SourceRange>,
        collapsed_delta: f32,
        line_height: f32,
    ) -> f32 {
        let baseline = line_height * collapsed_delta;
        let Some(delimiter_range) = projection.delimiter_range else {
            return baseline;
        };
        let Some(visual) = self.block_cache.get(&block.id) else {
            return baseline;
        };
        let Some(layout) = self.layout_cache.get(&block.id) else {
            return baseline;
        };
        if !layout.is_valid(
            self.content_width,
            self.layout_font_revision,
            self.editor().document().revision(),
        ) {
            return baseline;
        }
        let Some(block_lines) = block_line_span(self.editor().document(), block) else {
            return baseline;
        };
        let Some(delimiter_line) = self
            .editor()
            .document()
            .line_for_offset(delimiter_range.start)
            .ok()
            .map(|line| line.0)
        else {
            return baseline;
        };
        let first_presented_line = visual.span.start + visual.lines_before;
        let Some(measured) = layout
            .layout
            .lines
            .iter()
            .find(|row| {
                first_presented_line + row.line == delimiter_line
                    && delimiter_line >= block_lines.start
                    && delimiter_line < block_lines.end
            })
            .map(|row| layout.layout.line_height_of(row.line))
            .filter(|height| *height > 0.0)
        else {
            return baseline;
        };
        let was_collapsed =
            previous.is_none_or(|active| table_delimiter_is_collapsed(projection, Some(active)));
        let is_collapsed =
            current.is_none_or(|active| table_delimiter_is_collapsed(projection, Some(active)));
        if was_collapsed == is_collapsed {
            return baseline;
        }
        let measured_delta = measured - line_height;
        baseline
            + if is_collapsed {
                measured_delta
            } else {
                -measured_delta
            }
    }

    /// Applies a disclosure-only height snapshot without replacing measured
    /// block geometry. The formal index is already current when this runs, so
    /// the old and new structural projections describe the same block ids; only the
    /// structural fence or table-delimiter delta needs to move each measured
    /// height. The
    /// snapshot itself supplies the arithmetic minimum for blocks that have
    /// not been laid out yet.
    pub(super) fn install_disclosure_heights_preserving_measurements(
        &mut self,
        snapshot: HeightIndex,
        previous_disclosure: Option<SourceRange>,
    ) {
        let Some(disclosure) = self.active_height_disclosure() else {
            return;
        };
        let Some(index) = self.current_index() else {
            return;
        };
        if self.granularity != Granularity::Blocks
            || self.heights.len() != snapshot.len()
            || index.len() != snapshot.len()
        {
            return;
        }

        let anchor = self.top_source_offset();
        let intra = (!self.heights.is_empty()).then(|| {
            let item = self.heights.block_at_y(self.scroll_y);
            self.scroll_y - self.heights.prefix_sum(item)
        });
        let line_height = self.line_height();
        let updates = (0..snapshot.len())
            .filter_map(|ordinal| {
                let block = index.block(ordinal)?;
                let minimum = snapshot.height(ordinal)?;
                let current = self.heights.height(ordinal)?;
                let target = previous_disclosure.map_or_else(
                    || current.max(minimum),
                    |previous| {
                        let block_is_final = ordinal + 1 == index.len();
                        let fence_projection = index.fence_height_projection(&block);
                        let table_projection = index.table_projection(&block);
                        let collapsed_fence = fence_projection.map_or(0, |projection| {
                            projection.inactive_rows_in(
                                block.source_range,
                                block.source_range,
                                Some(disclosure),
                                block_is_final,
                            )
                        });
                        let previous_collapsed_fence = fence_projection.map_or(0, |projection| {
                            projection.inactive_rows_in(
                                block.source_range,
                                block.source_range,
                                Some(previous),
                                block_is_final,
                            )
                        });
                        let collapsed_table = table_projection
                            .filter(|projection| {
                                table_delimiter_is_collapsed(projection, Some(disclosure))
                            })
                            .map_or(0, |_| 1);
                        let previous_collapsed_table = table_projection
                            .filter(|projection| {
                                table_delimiter_is_collapsed(projection, Some(previous))
                            })
                            .map_or(0, |_| 1);
                        let fence_height_delta = fence_projection.map_or(0.0, |projection| {
                            self.fence_height_delta(
                                &block,
                                projection,
                                Some(previous),
                                Some(disclosure),
                                collapsed_fence as f32 - previous_collapsed_fence as f32,
                                line_height,
                            )
                        });
                        let table_height_delta = table_projection.map_or(0.0, |projection| {
                            self.table_delimiter_height_delta(
                                &block,
                                projection,
                                Some(previous),
                                Some(disclosure),
                                collapsed_table as f32 - previous_collapsed_table as f32,
                                line_height,
                            )
                        });
                        preserve_measured_height_after_fence_delta(
                            current,
                            minimum,
                            fence_height_delta + table_height_delta,
                        )
                    },
                );
                (target != current).then_some((ordinal, target))
            })
            .collect::<Vec<_>>();
        for (ordinal, height) in updates {
            self.heights.update(ordinal, height);
        }
        self.scroll_y = anchor.map_or(self.scroll_y, |offset| {
            let top = self.scroll_for_offset(offset);
            let item = self.heights.block_at_y(top);
            let inside = intra
                .zip(self.heights.height(item))
                .map_or(0.0, |(intra, height)| intra.clamp(0.0, height));
            top + inside
        });
        self.last_applied_height_disclosure =
            Some((self.editor().document().revision(), disclosure));
        self.last_endpoint_height_disclosure =
            Some((self.editor().document().revision(), disclosure));
    }

    pub(super) fn active_height_disclosure(&self) -> Option<SourceRange> {
        if !self.body_editable() {
            // Reading opens no Markdown markers, so no structural row stays
            // expanded: `None` makes every fence and table delimiter row count
            // as collapsed in height arithmetic and virtualization.
            return None;
        }
        self.editor()
            .ime()
            .map(|ime| ime.current_range)
            .or_else(|| Some(self.editor().selection().range()))
    }

    /// Keeps `heights` keyed to the same thing the renderer enumerates. This is
    /// the full synchronization path used for startup and index publication;
    /// input uses `apply_height_index_update` to touch only its parse window.
    pub(super) fn resync_heights(&mut self) {
        let (granularity, len) = self.desired_layout();
        if granularity == self.granularity && len == self.heights.len() {
            return;
        }
        let heights = HeightIndex::new(self.item_heights());
        self.install_heights(granularity, heights);
    }

    /// Rebuilds the height snapshot even when its shape did not change. A
    /// formal parse can finish after the caret or IME moved without changing
    /// the block count; retaining the old same-sized tree would retain the
    /// previous fence disclosure and make presentation and virtualization
    /// disagree.
    pub(super) fn resync_heights_for_current_disclosure(&mut self) {
        let (granularity, len) = self.desired_layout();
        if granularity == self.granularity && len == self.heights.len() {
            self.install_heights(granularity, HeightIndex::new(self.item_heights()));
        } else {
            self.resync_heights();
        }
    }

    /// Swaps in a height index, keeping the reader where they were: the scroll
    /// position is carried across on the source offset at the top of the
    /// viewport, read before the swap and resolved after it.
    pub(super) fn install_heights(&mut self, granularity: Granularity, mut heights: HeightIndex) {
        let anchor = self.top_source_offset();
        let intra = (!self.heights.is_empty()).then(|| {
            let item = self.heights.block_at_y(self.scroll_y);
            self.scroll_y - self.heights.prefix_sum(item)
        });
        let mut next_height_blocks = if granularity == Granularity::Blocks {
            self.current_index()
                .map(|index| {
                    index
                        .blocks()
                        .enumerate()
                        .map(|(ordinal, block)| HeightBlock {
                            id: block.id,
                            revision: block.revision,
                            line_count: block.line_count,
                            source_range: block.source_range,
                            estimated_height_bits: heights
                                .height(ordinal)
                                .unwrap_or_default()
                                .to_bits(),
                        })
                        .collect::<HeightBlocks>()
                })
                .unwrap_or_default()
        } else {
            HeightBlocks::default()
        };
        if granularity == Granularity::Blocks {
            next_height_blocks.content_width_bits = Some(self.content_width.to_bits());
        }
        if granularity == Granularity::Blocks && self.granularity == Granularity::Blocks {
            preserve_measured_block_heights(
                &self.height_blocks,
                &self.heights,
                &next_height_blocks,
                &mut heights,
            );
        }
        self.granularity = granularity;
        self.heights = heights;
        self.height_blocks = next_height_blocks;
        self.scroll_y = anchor.map_or(self.scroll_y, |offset| {
            let top = self.scroll_for_offset(offset);
            let item = self.heights.block_at_y(top);
            let inside = intra
                .zip(self.heights.height(item))
                .map_or(0.0, |(intra, height)| intra.clamp(0.0, height));
            top + inside
        });
        self.last_applied_height_disclosure = self
            .active_height_disclosure()
            .map(|disclosure| (self.editor().document().revision(), disclosure));
        self.last_endpoint_height_disclosure = self.last_applied_height_disclosure;
    }

    /// Applies the small splice reported by the incremental block index. The
    /// common case (typing without changing block boundaries or line count)
    /// compares a handful of entries and leaves the height tree untouched.
    pub(super) fn apply_height_index_update(&mut self, update: &BlockIndexUpdate) -> bool {
        if self.granularity != Granularity::Blocks {
            return false;
        }
        let first = update.first_replaced_block;
        let old_end = first.saturating_add(update.replaced_blocks);
        if old_end > self.height_blocks.len() {
            return false;
        }
        let Some(index) = self.current_index() else {
            return false;
        };
        if index.len() != self.height_blocks.len() - update.replaced_blocks + update.inserted_blocks
        {
            return false;
        }
        let Some(inserted) = (first..first + update.inserted_blocks)
            .map(|ordinal| index.block(ordinal))
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        let line_height = self.line_height();
        let disclosure = self.active_height_disclosure();
        let initial_heights = inserted
            .iter()
            .map(|block| {
                block_height_with_disclosure(
                    self.editor().document(),
                    index,
                    block,
                    line_height,
                    disclosure,
                )
            })
            .collect::<Vec<_>>();
        let next = inserted
            .iter()
            .zip(&initial_heights)
            .map(|(block, estimated_height)| HeightBlock {
                id: block.id,
                revision: block.revision,
                line_count: block.line_count,
                source_range: block.source_range,
                estimated_height_bits: estimated_height.to_bits(),
            })
            .collect::<Vec<_>>();
        if self.height_blocks.range_eq(first..old_end, &next) {
            return true;
        }
        let old_top = (!self.heights.is_empty()).then(|| {
            let ordinal = self.heights.block_at_y(self.scroll_y);
            let id = self.height_blocks.get(ordinal).map(|block| block.id);
            let intra = self.scroll_y - self.heights.prefix_sum(ordinal);
            (id, ordinal, intra)
        });
        self.heights.splice(first..old_end, initial_heights);
        self.height_blocks.splice(first..old_end, &next);

        if let Some((id, old_ordinal, intra)) = old_top {
            let ordinal = rebase_ordinal_after_splice(
                old_ordinal,
                id,
                first..old_end,
                &next,
                self.heights.len(),
            );
            let inside = self
                .heights
                .height(ordinal)
                .map_or(0.0, |height| intra.clamp(0.0, height));
            self.scroll_y = self.heights.prefix_sum(ordinal) + inside;
        }
        true
    }
}
