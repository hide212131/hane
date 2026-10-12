use super::*;
use crate::display::{block_display_kind, block_is_joinable, block_line_context};

pub fn block_line_span(document: &RopeBuffer, block: &IndexedBlock) -> Option<Range<usize>> {
    let first = document.line_for_offset(block.source_range.start).ok()?;
    let last =
        if block.source_range.is_empty() || block.source_range.end.0 >= document.len_bytes().0 {
            document.line_for_offset(block.source_range.end).ok()?
        } else {
            document
                .line_for_offset(SourceOffset(block.source_range.end.0 - 1))
                .ok()?
        };
    Some(first.0..last.0 + 1)
}

/// Blank lines closing a block. Tiling folds the blank run between two blocks
/// into the block above, and those lines are not part of the construct. Walks
/// back from the block's end, so it reads the blank run and one line more.
pub fn trailing_blank_lines(document: &RopeBuffer, span: &Range<usize>) -> usize {
    span.clone()
        .rev()
        .take_while(|line| {
            document
                .line_range(hane_document::LineId(*line))
                .ok()
                .and_then(|range| document.text(range).ok())
                .is_none_or(|text| text.trim().is_empty())
        })
        .count()
}

/// Initial height of every block in the index, from the line height alone.
///
/// Seeds the [`crate::HeightIndex`] at block granularity, and is re-run whenever the
/// block count changes. It uses the same block-local calculation as incremental
/// updates; only the final block needs a bounded line-span lookup to account
/// for a trailing empty caret line. Measured heights replace these as blocks
/// are drawn.
pub fn block_heights(document: &RopeBuffer, index: &BlockIndex, line_height: f32) -> Vec<f32> {
    block_heights_with_disclosure(document, index, line_height, None)
}

/// Initial block heights while respecting the editor's active disclosure.
///
/// The height projection is block-relative and cheap to query, so startup and
/// height-index rebuilds can keep the caret/selection/IME-owned fence row at
/// normal height without presenting the block first. This prevents a run of
/// otherwise zero-height fence blocks from disappearing from the initial
/// virtualization window before the editable row has a chance to render.
pub fn block_heights_with_disclosure(
    document: &RopeBuffer,
    index: &BlockIndex,
    line_height: f32,
    disclosure: Option<SourceRange>,
) -> Vec<f32> {
    index
        .blocks()
        .map(|block| block_height_with_disclosure(document, index, &block, line_height, disclosure))
        .collect()
}

/// Initial height for one block, shared by full snapshots and incremental
/// height-index splices. The calculation reads only this block's projections;
/// the final block's line-span lookup accounts for an empty line after a
/// trailing newline without scanning the document.
pub fn block_height_with_disclosure(
    document: &RopeBuffer,
    index: &BlockIndex,
    block: &IndexedBlock,
    line_height: f32,
    disclosure: Option<SourceRange>,
) -> f32 {
    let block_is_final = block.ordinal + 1 == index.len();
    let collapsed_fence_rows = index
        .fence_height_projection(block)
        .map_or(0, |projection| {
            projection.inactive_rows_in(
                block.source_range,
                block.source_range,
                disclosure,
                block_is_final,
            )
        });
    let collapsed_table_delimiter = index
        .table_projection(block)
        .filter(|projection| table_delimiter_is_collapsed(projection, disclosure))
        .map_or(0, |_| 1);
    let collapsed = collapsed_fence_rows + collapsed_table_delimiter;
    let trailing_empty_lines = if block_is_final {
        block_line_span(document, block)
            .map_or(0, |span| span.len().saturating_sub(block.line_count))
    } else {
        0
    };
    line_height * block.line_count.saturating_sub(collapsed) as f32
        + line_height * trailing_empty_lines as f32
}

/// One physical source line handed to [`present_block`].
#[derive(Clone, Copy, Debug)]
pub struct BlockLine<'a> {
    /// Document line number. Becomes the presented line's [`VisualLine::line_id`].
    pub line: usize,
    pub range: SourceRange,
    pub text: &'a str,
    /// Source range whose Markdown markers this line currently discloses,
    /// resolved by the caller from caret, selection and IME state.
    pub disclosure: Option<SourceRange>,
}

/// Which lines of a block to present, and where they sit inside it.
#[derive(Clone, Debug)]
pub struct BlockWindow<'a> {
    /// Document lines the whole block covers.
    pub span: Range<usize>,
    /// Blank lines closing the block. Tiling folds the blank run between two
    /// blocks into the block above, and those lines are not part of the
    /// construct — a blank line after a closing fence is not code.
    pub trailing_blank_lines: usize,
    /// Lines available to build the [`VisualBlock`] from. For a joinable
    /// paragraph (see [`present_block`]) this may reach beyond `render` on
    /// either side, because CommonMark resolves `**`/`_`/`` ` `` across the
    /// whole paragraph, not just the lines that happen to be drawn; a marker
    /// whose other half sits outside `lines` still cannot be recognized, so
    /// the caller should give enough of the paragraph to close every marker
    /// that opens inside `render` — unless `joined` already supplies that
    /// context, in which case `lines` only needs to cover `render`.
    pub lines: &'a [BlockLine<'a>],
    /// Fence delimiter lines outside `lines` that still affect the block's
    /// inactive vertical footprint. They are height-accounting context only:
    /// never joined into paragraph/list parsing and never rendered directly.
    pub clipped_fence_lines: &'a [BlockLine<'a>],
    /// Inactive nested fence rows clipped above/below the render window.
    /// These are pre-counted from the formal projection so presentation does
    /// not materialize off-screen source rows merely to account for height.
    pub zero_height_fence_rows_before: usize,
    pub zero_height_fence_rows_after: usize,
    /// Absolute document line number of a formal table delimiter. This is
    /// supplied even when the delimiter is outside `lines`, so clipped table
    /// rows do not regain a virtual height.
    pub table_delimiter_line: Option<usize>,
    /// The subset of `lines` (by document line number) to actually turn into
    /// presented [`VisualLine`]s. Lines in `lines` outside this range are
    /// parsing context only and are not drawn.
    pub render: Range<usize>,
    /// A joinable block's whole-span parse, already computed. When present,
    /// a joined run is presented against this instead of rejoining and
    /// reparsing `lines`, so a caller that keeps this cached across
    /// presentation calls (see [`parse_joined_span`]) can hand `render`'s own
    /// lines alone as `lines` — a viewport miss on an already-parsed block
    /// then costs proportionally to what is drawn, not to the whole block,
    /// while still resolving a marker pair arbitrarily far apart the way a
    /// single whole-paragraph parse would, regardless of where the viewport
    /// happens to sit.
    pub joined: Option<&'a JoinedParse>,
    /// Caret, selection or IME range touching this block's whole span,
    /// computed by the caller directly from editor state rather than
    /// reconstructed from `lines`. A joined run's own [`merged_disclosure`]
    /// only sees the physical lines `lines` actually carries, which for a
    /// block presented from `render` alone (see [`JoinedParse`]) excludes any
    /// off-screen line of the same shared construct; this field is what lets
    /// a caret sitting on one of those off-screen lines still disclose the
    /// construct's markers on the lines that are drawn.
    pub block_disclosure: Option<SourceRange>,
}

/// Presents `window.render`'s lines of one indexed Markdown block.
///
/// Every line is presented in the context its block kind implies, except the
/// blank run closing the block. The trailing newline each line's visual text
/// keeps for source-map fidelity is trimmed here, because the renderer draws one
/// element per line.
pub fn present_block(
    block: &IndexedBlock,
    revision: Revision,
    window: &BlockWindow<'_>,
    line_height: f32,
) -> VisualBlock {
    present_block_with_list_projection(block, revision, window, line_height, None)
}

/// Presents a block with document-wide list context from a formal block index.
///
/// A block larger than the synchronous join budget is intentionally parsed only
/// for the visible lines. The compact projection lets those lines retain the
/// formal list's numbering, depth, and marker-column width until a whole-block
/// [`JoinedParse`] becomes available.
pub fn present_block_with_list_projection(
    block: &IndexedBlock,
    revision: Revision,
    window: &BlockWindow<'_>,
    line_height: f32,
    list_projection: Option<&ListProjection>,
) -> VisualBlock {
    present_block_with_table_projection(block, revision, window, line_height, list_projection, None)
}

/// Presents a block with document-wide list and table metadata. The compact
/// table projection is optional so provisional/local callers can safely use
/// the bounded delimiter fallback.
pub fn present_block_with_table_projection(
    block: &IndexedBlock,
    revision: Revision,
    window: &BlockWindow<'_>,
    line_height: f32,
    list_projection: Option<&ListProjection>,
    table_projection: Option<&TableProjection>,
) -> VisualBlock {
    let context = block_line_context(block.kind);
    let content_end = window.span.end.saturating_sub(window.trailing_blank_lines);
    let local_table_delimiter_line = (context == LineContext::Table)
        .then(|| {
            window
                .lines
                .iter()
                .find(|line| is_table_delimiter(line.text))
                .map(|line| line.line)
        })
        .flatten();
    let table_delimiter_range = table_projection.and_then(|projection| projection.delimiter_range);
    let table_delimiter_line = window
        .table_delimiter_line
        .or_else(|| {
            table_delimiter_range.and_then(|delimiter| {
                window
                    .lines
                    .iter()
                    .find(|line| line.range == delimiter)
                    .map(|line| line.line)
            })
        })
        .or(local_table_delimiter_line);
    let table_alignments = table_projection.map_or_else(
        || {
            table_delimiter_line
                .and_then(|delimiter| window.lines.iter().find(|line| line.line == delimiter))
                .map_or_else(Vec::new, |line| table_alignments(line.text))
        },
        |projection| projection.alignments.to_vec(),
    );
    // The opening fence's own marker shape, read from whichever of `window.lines`
    // carries the block's true first physical line — present regardless of
    // `window.render` (see `BlockWindow::lines`'s own "parsing context only"
    // contract) so a closing-fence candidate deep in a large block can be
    // validated without the caller ever materializing lines between the two.
    // The lowest-numbered line in `window.lines` is not always that line: block
    // ordinal 0's tiled span absorbs any blank run before the document's first
    // block, so a blank line can sit in `window.lines` ahead of the real
    // opening fence. Skipping blank lines here protects the opening metadata
    // from that prefix without re-reading the document. `None` for an
    // indented code block (no fence to derive at all) or when the caller could
    // not supply that line.
    let fence_opening = (context == LineContext::FencedCode)
        .then(|| {
            window
                .lines
                .iter()
                .filter(|line| !line.text.trim().is_empty())
                .min_by_key(|line| line.line)
        })
        .flatten()
        .and_then(|line| fence_delimiter(line.text).map(|delimiter| (line.line, delimiter)));
    let mut lines = Vec::with_capacity(window.render.len().min(window.lines.len()));
    for run in disclosure_runs(block.kind, window) {
        if run_uses_shared_parse(run.len(), window.joined) {
            present_joined_run_with_list_projection(
                &window.lines[run.clone()],
                revision,
                line_height,
                &window.render,
                window.joined,
                window.block_disclosure,
                list_projection,
                &mut lines,
            );
            continue;
        }
        let line = window.lines[run.start];
        if !window.render.contains(&line.line) {
            continue;
        }
        let line_context = if line.line < content_end {
            context
        } else {
            LineContext::Normal
        };
        let fence_role = (line_context == LineContext::FencedCode)
            .then(|| fence_line_role(line.line, content_end, line.text, fence_opening))
            .flatten();
        let table_header = table_projection
            .and_then(|projection| projection.delimiter_range)
            .is_some_and(|delimiter| line.range.end <= delimiter.start)
            || table_delimiter_line.is_some_and(|delimiter| line.line < delimiter);
        let mut presented = present_polished_line_with_fence(
            line.line as u64,
            revision,
            line.range,
            line.text,
            line_height,
            line.disclosure,
            line_context,
            list_projection,
            fence_role,
            table_header,
            &table_alignments,
        );
        while presented.visual_text.ends_with(['\r', '\n']) {
            presented.visual_text.pop();
        }
        lines.push(presented);
    }
    let lines_before = lines.first().map_or(0, |line| {
        (line.line_id as usize).saturating_sub(window.span.start)
    });
    let lines_after = window.span.len().saturating_sub(lines_before + lines.len());
    let first_presented_line = window.span.start.saturating_add(lines_before);
    let presented_end = first_presented_line.saturating_add(lines.len());
    let mut zero_height_lines_before = window.zero_height_fence_rows_before;
    let mut zero_height_lines_after = window.zero_height_fence_rows_after;
    let mut record_collapsed = |line: usize, collapsed: bool| {
        if !collapsed {
            return;
        }
        if line < first_presented_line {
            zero_height_lines_before += 1;
        } else if line >= presented_end {
            zero_height_lines_after += 1;
        }
    };
    if context == LineContext::FencedCode {
        for line in window.lines {
            let Some(fence_role) =
                fence_line_role(line.line, content_end, line.text, fence_opening)
            else {
                continue;
            };
            let collapsed = present_fenced_code_line(
                line.line as u64,
                revision,
                line.range,
                line.text,
                line_height,
                line.disclosure,
                Some(fence_role),
            )
            .height()
                == 0.0;
            record_collapsed(line.line, collapsed);
        }
    }
    for line in window.clipped_fence_lines {
        let collapsed = if context == LineContext::FencedCode {
            fence_line_role(line.line, content_end, line.text, fence_opening).is_some_and(
                |fence_role| {
                    present_fenced_code_line(
                        line.line as u64,
                        revision,
                        line.range,
                        line.text,
                        line_height,
                        line.disclosure,
                        Some(fence_role),
                    )
                    .height()
                        == 0.0
                },
            )
        } else {
            false
        };
        record_collapsed(line.line, collapsed);
    }
    let table_delimiter_collapsed = table_delimiter_line.is_some_and(|delimiter| {
        table_delimiter_range.map_or_else(
            || {
                lines
                    .iter()
                    .find(|line| line.line_id as usize == delimiter)
                    .is_some_and(|line| line.kind == BlockKind::TableDelimiter)
            },
            |range| {
                table_delimiter_is_collapsed_range(
                    range,
                    table_projection.is_some_and(|projection| projection.delimiter_has_line_ending),
                    window.block_disclosure,
                )
            },
        )
    });
    if table_delimiter_collapsed {
        record_collapsed(table_delimiter_line.unwrap_or_default(), true);
    }
    VisualBlock {
        id: block.id,
        kind: block_display_kind(block.kind),
        source_range: block.source_range,
        revision,
        confidence: block.confidence,
        span: window.span.clone(),
        lines,
        table_projection: table_projection.cloned(),
        lines_before,
        lines_after,
        zero_height_lines_before,
        zero_height_lines_after,
        line_height,
    }
}

fn run_uses_shared_parse(run_len: usize, joined: Option<&JoinedParse>) -> bool {
    run_len > 1 || joined.is_some()
}

/// Groups `window.lines` into the runs [`present_block`] presents from: a run
/// of two or more contiguous lines in a joinable block's normal flow, sharing
/// one whole-paragraph parse and one run-wide merged disclosure (see
/// [`merged_disclosure`]), or a single line presented and disclosed on its
/// own. Returns index ranges into `window.lines`, in order. Needs no parse,
/// so [`expected_disclosures`] can reuse the exact same grouping
/// [`present_block`] itself derives without doing the whole-paragraph parse
/// that grouping feeds into.
///
/// A standalone image line stays inside its run rather than splitting it: a
/// delimiter pair CommonMark resolves across the whole paragraph (see
/// [`present_joined_run`]) must still close correctly when an image line sits
/// between the two halves, even though that image line itself renders through
/// [`present_image`] rather than the shared parse (see
/// [`inactive_standalone_image`]).
///
/// Whether a returned run is actually presented with shared-parse semantics
/// is [`run_uses_shared_parse`]'s call, not this function's: a run can come
/// back one line long here and still need the shared parse, when
/// `window.joined` is already available.
fn disclosure_runs(block_kind: NodeKind, window: &BlockWindow<'_>) -> Vec<Range<usize>> {
    let context = block_line_context(block_kind);
    let content_end = window.span.end.saturating_sub(window.trailing_blank_lines);
    let joinable = block_is_joinable(block_kind);
    let mut runs = Vec::new();
    let mut index = 0;
    while index < window.lines.len() {
        let line = window.lines[index];
        let line_context = if line.line < content_end {
            context
        } else {
            LineContext::Normal
        };
        let mut run_end = index + 1;
        if joinable && line_context == LineContext::Normal {
            while run_end < window.lines.len() {
                let next = window.lines[run_end];
                let next_context = if next.line < content_end {
                    context
                } else {
                    LineContext::Normal
                };
                if next_context != LineContext::Normal {
                    break;
                }
                run_end += 1;
            }
        }
        runs.push(index..run_end);
        index = run_end;
    }
    runs
}

/// Disclosure [`present_block`] would currently assign to each of
/// `window.render`'s lines, paired with that line's document line number: the
/// run-wide merged disclosure (see [`merged_disclosure`]) for a non-empty line
/// inside a run [`run_uses_shared_parse`] says presents with shared-parse
/// semantics — even one whose own [`BlockLine::disclosure`] is `None`, because
/// the caret, selection or IME sits on a different physical line of the same
/// shared construct, or because that other line is off-screen entirely and
/// only reachable through `window.joined` — or the line's own disclosure
/// otherwise.
/// An empty line (the blank run trailing the document's last block; see
/// [`block_line_span`]) never gets the merged value even when grouped into the
/// run, matching `present_markdown_from_parse`'s own early return for empty
/// source, which presents it via [`present_plain`] with no disclosure at all.
/// Neither does an inactive standalone image line grouped into the run — see
/// [`inactive_standalone_image`] — since [`present_joined_run`] renders it
/// through [`present_image`], which always reports no disclosure of its own.
///
/// Computed without parsing, from the same grouping [`present_block`] derives
/// (see [`disclosure_runs`]), so a caller checking whether a cached
/// [`VisualBlock`]'s disclosures are still current (see
/// `EditorView::disclosures_are_current`) gets exactly the value a fresh
/// [`present_block`] call would assign instead of risking the two definitions
/// of "current" falling out of sync — which otherwise invalidates and rebuilds
/// the cache every frame the caret sits inside an active multi-line paragraph,
/// since a per-line-only recomputation never agrees with the run-wide value a
/// joined run's other lines were actually presented with.
pub fn expected_disclosures(
    block_kind: NodeKind,
    window: &BlockWindow<'_>,
) -> Vec<(usize, Option<SourceRange>)> {
    let mut out = Vec::new();
    for run in disclosure_runs(block_kind, window) {
        let lines = &window.lines[run];
        let shared_parse = run_uses_shared_parse(lines.len(), window.joined);
        let merged = shared_parse
            .then(|| joined_run_disclosure(lines, window.block_disclosure))
            .flatten();
        for line in lines {
            if window.render.contains(&line.line) {
                let disclosure = if shared_parse
                    && !line.text.is_empty()
                    && inactive_standalone_image(line.text, line.range, line.disclosure).is_none()
                {
                    merged
                } else if shared_parse {
                    None
                } else {
                    line.disclosure
                };
                out.push((line.line, disclosure));
            }
        }
    }
    out
}

/// Union of every line's own disclosure in a joined run, so a shared construct
/// that spans more than one physical line discloses consistently on every line
/// that carries one of its markers, not only the line the caret/selection/IME
/// happens to sit on. Reconstructs the original caret point or selection/IME
/// extent: each line's own disclosure is already that extent clipped to the
/// line, so the union of all of them is the extent itself.
fn merged_disclosure(lines: &[BlockLine<'_>]) -> Option<SourceRange> {
    lines
        .iter()
        .filter_map(|line| line.disclosure)
        .reduce(union_disclosure)
}

fn union_disclosure(a: SourceRange, b: SourceRange) -> SourceRange {
    SourceRange {
        start: a.start.min(b.start),
        end: a.end.max(b.end),
    }
}

/// The disclosure a joined run of `lines` presents against: [`merged_disclosure`]'s
/// reconstruction from `lines`' own per-line disclosures, unioned with
/// `block_disclosure`. `lines` may be only the block's visible slice (see
/// [`BlockWindow::block_disclosure`]), so a caret/selection/IME range that
/// falls on an off-screen physical line of the same shared construct would
/// otherwise never surface here.
fn joined_run_disclosure(
    lines: &[BlockLine<'_>],
    block_disclosure: Option<SourceRange>,
) -> Option<SourceRange> {
    [merged_disclosure(lines), block_disclosure]
        .into_iter()
        .flatten()
        .reduce(union_disclosure)
}

#[cfg(test)]
fn present_joined_run(
    lines: &[BlockLine<'_>],
    revision: Revision,
    line_height: f32,
    render: &Range<usize>,
    joined: Option<&JoinedParse>,
    block_disclosure: Option<SourceRange>,
    out: &mut Vec<VisualLine>,
) {
    present_joined_run_with_list_projection(
        lines,
        revision,
        line_height,
        render,
        joined,
        block_disclosure,
        None,
        out,
    );
}

#[allow(
    clippy::too_many_arguments,
    reason = "joined presentation keeps the render window and parse context together"
)]
fn present_joined_run_with_list_projection(
    lines: &[BlockLine<'_>],
    revision: Revision,
    line_height: f32,
    render: &Range<usize>,
    joined: Option<&JoinedParse>,
    block_disclosure: Option<SourceRange>,
    list_projection: Option<&ListProjection>,
    out: &mut Vec<VisualLine>,
) {
    let computed;
    let joined = match joined {
        Some(joined) => joined,
        None => {
            computed = parse_joined_block(lines, revision);
            &computed
        }
    };
    let shared = SharedParse {
        parsed: &joined.parsed,
        projection: &joined.projection,
        list_projection,
    };
    // A single active disclosure (caret, selection or IME) may touch a shared
    // construct whose markers live on different physical lines; `marker_is_disclosed`
    // only expands a marker whose enclosing span reaches the disclosure it is
    // given, so every line of the run is offered the same, run-wide disclosure
    // rather than only the one line that literally owns the caret.
    let disclosure = joined_run_disclosure(lines, block_disclosure);
    for line in lines {
        if !render.contains(&line.line) {
            continue;
        }
        // An inactive standalone image line keeps rendering through its own
        // dedicated path even while joined into the run for parse purposes —
        // only the surrounding text lines' delimiters need the shared parse;
        // the image line's own disclosure (not the run-wide merged one)
        // decides whether it is presently being edited as raw markup instead.
        let mut presented = match inactive_standalone_image(line.text, line.range, line.disclosure)
        {
            Some(image) => present_image(
                line.line as u64,
                revision,
                line.range,
                line.text,
                line_height,
                image,
                list_projection
                    .and_then(|projection| formal_quote_metadata(line.range, projection)),
            ),
            None => present_markdown_from_parse(
                line.line as u64,
                revision,
                line.range,
                line.text,
                line_height,
                disclosure,
                &shared,
            ),
        };
        presented.context = LineContext::Normal;
        while presented.visual_text.ends_with(['\r', '\n']) {
            presented.visual_text.pop();
        }
        out.push(presented);
    }
}

/// Presents one physical source line with Phase 4 block polish. The block-level
/// [`LineContext`] is supplied by the caller from the document context index;
/// standalone image recognition is local to this source slice. Presentation owns
/// every display-kind and style-run decision here so the UI renders purely by
/// [`BlockKind`]/[`StyleKind`] without re-inspecting the source.

#[cfg(test)]
mod tests;
