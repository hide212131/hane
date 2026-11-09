# Issue #704 参考: gpui-pre 0.3.6 / accesskit 0.24.1 の a11y API 抜粋

この文書は、実装 worker が registry 上の依存ソースへアクセスできない環境向けに、実際の依存（crates.io の `gpui-pre 0.3.6` と `accesskit 0.24.1`）から a11y 関連の公開 API だけを抜粋したものである。設計の正本は `issue-704-a11y-synthetic-nodes-design.md`、範囲の正本は `issue-704-a11y-phase0-handoff.md` とし、この文書は API の事実確認専用とする。

**注意: リポジトリ内の `vendor/gpui` は gpui 0.2.2 であり、実際に使う gpui-pre 0.3.6 ではない。API の参照先に使わないこと。**

## Element トレイトの a11y フック（gpui-pre 0.3.6 `src/element.rs`）

```rust
/// Returns the accessible role for this element, if any.
/// Elements that return `None` are not included in the accessibility tree.
/// Note: inclusion in accessibility tree requires non-`None` `Element::id`.
fn a11y_role(&self) -> Option<accesskit::Role> {
    None
}

/// Write accessibility properties to the given node.
/// Called only when `a11y_role()` returns `Some`.
fn write_a11y_info(&self, _node: &mut accesskit::Node) {}

/// Add synthetic child nodes to an `Element` that has an `.id()` and a `.role()`.
/// Some elements may want to inject accessibility nodes that do not
/// correspond to any GPUI element. For example, a custom text field element
/// may want to inject synthetic child nodes for the text content.
fn a11y_synthetic_children(
    &mut self,
    _prepaint: &mut Self::PrepaintState,
    _builder: &mut A11ySubtreeBuilder,
) {
}
```

要素が a11y ツリーに載る条件は、`Element::id` が non-`None` で、かつ `a11y_role()` が `Some` を返すこと。フレーム描画時に GPUI がツリーを走査し、role のある要素について `accesskit::Node` を作り、要素の layout bounds に `window.scale_factor()` を掛けた bounds を自動設定したうえで `write_a11y_info` を呼ぶ。合成子ノードは prepaint の後に追加されるため、prepaint state（可視範囲など）を使える。

## A11ySubtreeBuilder（gpui-pre 0.3.6 `src/window/a11y.rs`）

```rust
pub struct A11ySubtreeBuilder<'a> { /* parent_id, nodes */ }

impl<'a> A11ySubtreeBuilder<'a> {
    /// Derive a `NodeId` for a synthetic child.
    /// The generated ID is based on the hash of `key`, as well as the parent's
    /// ID. `key`s must be unique within the same `a11y_synthetic_children`
    /// call, but may be duplicated across different calls.
    pub fn synthetic_node_id(&self, key: impl Hash) -> NodeId;

    /// Append a synthetic leaf node as a child of this element's node.
    /// Returns `false` if a node with this id is already present in the tree,
    /// in which case the node is discarded.
    pub fn push_child(&mut self, id: NodeId, node: accesskit::Node) -> bool;

    /// A mutable reference to the parent node.
    pub fn parent_node(&mut self) -> &mut accesskit::Node;
}
```

`push_child` は leaf 専用である。合成ノード同士の階層は組めない。

## Window の a11y API（gpui-pre 0.3.6 `src/window.rs`）

```rust
/// Whether accessibility is currently active.
/// Use this to skip computing data during rendering that is only
/// observable through the accessibility tree. When accessibility is
/// activated, a redraw is forced, so gated work is recomputed before the
/// next tree update is sent to the platform.
pub fn is_a11y_active(&self) -> bool;

/// Register a listener for an accessibility action on a specific node.
/// The listener will be called when a screen reader requests the given
/// action on the node identified by `node_id`.
pub fn on_a11y_action(
    &mut self,
    node_id: accesskit::NodeId,
    action: accesskit::Action,
    listener: impl FnMut(Option<&accesskit::ActionData>, &mut Window, &mut App) + 'static,
);
```

リスナーはフレームごとに再登録が必要である（GPUI はフレーム開始時にリスナーをクリアする）。登録のないノードへ Click が来た場合の GPUI 既定 fallback は、node bounds の中央へのマウス入力生成であり、本機能では使わない（設計書 §6.1）。

## accesskit 0.24.1 の Node（`src/lib.rs`）

```rust
let mut node = accesskit::Node::new(accesskit::Role::Link);
node.set_label("表示ラベル".to_string());        // (Label, label, set_label, clear_label)
node.set_bounds(accesskit::Rect { x0, y0, x1, y1 }); // (Bounds, ..., set_bounds, ..., Rect, ...)
node.add_action(accesskit::Action::Click);
```

- `accesskit::Role` には `Document` と `Link` がある。
- `accesskit::Rect` は `f64` の `x0, y0, x1, y1`（左上・右下）。GPUI の通常ノードは layout bounds（logical）に `window.scale_factor()` を掛けた値を渡しているため、合成ノードの bounds も同じ単位にそろえる。
- プロパティ setter はマクロ生成されており、`set_label`、`set_bounds`、`set_description` 等が `Node` に生える。`add_action` で対応アクションを宣言する。

## 公式ガイドの合成子ノード例（gpui-pre 0.3.6 `src/_accessibility.rs` より要旨）

カスタムテキスト要素が `Role::TextInput` を名乗り、子として `Role::TextRun` の合成ノードを `synthetic_node_id(0)` と `push_child` で注入する例が示されている。本機能では同じ型で、親を文書ルート（`Role::Document`）、子をリンク（`Role::Link`）として適用する。ガイドは「合成子ノードは prepaint の後に追加される」ことを明記している。
