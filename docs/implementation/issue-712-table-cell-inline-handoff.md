# Issue #712 実装ハンドオフ要約

対象: 非アクティブな表セル内のインライン装飾（太字・イタリック・インラインコード・打ち消し・インラインリンク）が、通常段落と同じくマーカー隠蔽＋スタイル付きで表示されるようにする。

設計の正本は `docs/implementation/issue-712-table-cell-inline-design.md`。要点:

- `present_table_line` のセル追加を、セルごとに仮想二列表で既存 parser に解析させ `present_markdown_from_parse` で第1セルだけを投影するアダプター経由に置き換える
- `TableCellDisplay.source_range` はマーカー込みで維持、`visual_range` は隠蔽後の表示範囲。列幅は表示テキスト＋実スタイル基準
- 参照リンクは正式な `TableProjectionRow.links` を正本に維持し、ラベルへの Link スタイル補完のみ拡張する。参照構文の新規隠蔽はしない
- セルをまたぐマーカーは結合しない、未閉鎖は隠さない、コードスパン内は装飾しない、検証不成立のセルは生ソースへフォールバック
- 変更しない: 開示時の通常経路、TableProjection の構造認識、区切り行、編集操作（セル移動・行列編集・undo/redo）、リンク先の解決規則

状態: 本書と設計書のみの準備 commit。この時点では未実装・未検証。
