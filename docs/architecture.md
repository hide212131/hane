# Hane architecture

Hane separates source ownership, Markdown syntax, presentation, layout, and GPUI rendering. The
workspace dependency direction is `editor → document`, `markdown → document`, and
`presentation → document/markdown`; `session` owns an `editor` and file state, and `ui` adapts
all of these to GPUI. `app` is only the executable composition root.

## Data flow

```text
DocumentSession → Editor → BlockIndex → VisualBlock → BlockLayout → LayoutLine → TextRun
                     │          │            │              │
                     │          │            │              └─ UI paints visible rows
                     │          │            └─ source map, display policy, inline runs
                     │          └─ syntax blocks, revision and confidence
                     └─ selections, IME, edits and undo/redo
```

`RopeBuffer` is the authoritative Markdown source. `BlockIndex` partitions that source into
top-level blocks and associates every result with a revision. Its background formal parse may
arrive late; `BlockIndexState` accepts only a result that can be rebased safely onto the current
revision. Before that happens, the UI uses a bounded provisional index for the viewport.

Markdown's `NodeKind` is syntax only. Presentation translates syntax through one private table
into `BlockKind`/`StyleKind` and then into `BlockDisplay`/`InlineDisplay`; the UI receives display
policy, never parser vocabulary. `VisualBlock` presents only the visible physical lines and maps
source offsets to visual offsets. `BlockLayout` adds soft-wrapped `LayoutLine`s and is the single
owner of source↔point, hit-testing, and vertical-motion geometry.

## Revision and cache boundaries

The UI caches presentation and layout per stable `BlockId`. Non-intersecting edits rebase cached
source ranges; a changed block, width, font revision, or image height invalidates only the relevant
entry. `HeightIndex` is chunked by blocks, so split/join edits splice only the changed span while
preserving measurements outside it.

`DocumentSession` owns file identity, dirty/saved revisions, save serialization, autosave tickets,
and per-session view state. `FileService` is the only filesystem boundary and writes atomically.
External file events never overwrite dirty in-memory work automatically.

## Verification and records

Run `cargo test --workspace --all-features` and
`cargo clippy --workspace --all-targets --all-features -- -D warnings` for the standard gate.
Performance scripts and baseline records live in [baseline](baseline/README.md); architectural
decisions are indexed in [ADR](adr/README.md), and refactoring status is maintained in
[the execution plan](refactor-execution-plan.md).
