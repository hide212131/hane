# Issue #704 Phase 0 実装準備

この文書は、Issue #704 の Phase 0（契約確認）の実装用 PR の準備記録である。この PR は Claude 実装待ちの準備 PR であり、この準備 commit 自体は製品コードを含まない。未実装・未検証の状態で開始する。

## 対象

- Issue: #704「feat(editor): エディタ本文の文書ルートと可視リンクをアクセシビリティツリーへ公開する」
- フェーズ: Phase 0（契約確認）。Phase 1（可視リンクの製品実装）は、この Phase 0 の契約が確定した後に別 PR で開始する。
- 基準 SHA: `5f8e27c1eaf3b33cc10b8d87144667f89010a1ee`（main、v0.43.0）
- 採用済み設計: `docs/implementation/issue-704-a11y-synthetic-nodes-design.md`（本 branch に同梱。Codex GPT-6.1 Sol / medium による設計、2026-11-09）

## PR の範囲を Phase 0 に分離した判断

意味判断（Jev、2026-11-09）により、最初の実装用 PR は Phase 0 のみに分離する。理由は、Phase 0 の未決事項（特に keyboard focus を持たないリンクを両 OS の支援技術が探索・起動できるか）が Phase 1 の契約を左右し、最小 spike で早期に検証できるためである。Phase 1 は別 PR とする。

## 今回の範囲（Phase 0）

設計書 §3〜§7 と §11 の Phase 0 に従い、契約確認のための最小実装を行う。

- 本文の実要素を文書ルート（`Role::Document`）として扱い、a11y 活性時だけ `a11y_synthetic_children` で表示範囲内のリンクを合成ノード（`Role::Link`、表示ラベル、bounds、`Action::Click`）として公開する。
- ノード ID は設計書 §4.2 の初期契約（文書ルートの ID ＋ ノード種別 ＋ 文書の a11y 内容世代 ＋ リンクのテキスト範囲）で生成する。bounds は既存のリンク矩形計算を再利用し、合成 leaf へ明示設定する。
- paint 時に `Window::on_a11y_action` を登録し、Click を受けたら設計書 §6.2 の検証（対象文書・文書 generation・公開内容世代・対象リンク・destination の一致）を行ったうえで、既存のリンク起動処理へ接続する。マウス入力への変換や GPUI の既定 Click fallback は使わない。
- a11y 非活性時は、設計書 §7 のとおり専用処理（列挙、ラベル生成、bounds 集約、NodeId の hash、closure の生成・登録）を一切行わない。
- Phase 1 の土台として使える品質で書くが、Phase 0 の prototype を完成機能として出荷した扱いにはしない。PR の処分（Phase 1 の土台として取り込むか、閉じて作り直すか）は、実機 evidence を見て別途判断する。

## 今回の受入条件（Phase 0）

- 合成 ID への action 配送が成立する: 宣言した NodeId へ Click が届き、正しい destination が1回だけ既存のリンク起動処理へ渡ることを、GPUI 統合テストで確認する。stale な要求（文書・世代・destination の不一致）は何も開かない。
- 座標単位が設計どおりである: ノードの bounds が既存のリンク矩形と一致し、GPUI の通常ノードと同じ単位（logical bounds × scale_factor）であることをテストで確認する。
- 非活性ゲートが成立する: a11y 非活性時に専用 collector・label builder・handler registration が呼ばれないことをテストで確認する。
- 編集・閲覧の両モードで同じリンクが公開され、リンク起動で本文 bytes、revision、selection、caret、Undo/Redo、IME 状態が変わらないことをテストで確認する。
- 実機 evidence（Commander / GUI Validator が別途取得し、この PR の受入判断に使う）: macOS では agent-desktop の bounds 付き snapshot で文書ルートとリンクを確認し、AXPress で起動する。keyboard focus を新設しないリンクを VoiceOver が探索・起動できるかも確認する。Windows は UIA snapshot と Invoke、Narrator で同様に確認する（Windows 側のチャットから直接操作）。実機で不成立の項目があれば、Phase 1 で Focus 契約を追加設計する。

## 対象外

- Phase 1 の完成条件（全文書的な受入、性能予算の固定）への拡張。
- Phase 2a（編集をまたぐリンク ID の同一性維持）、Phase 2b（見出しの公開）。
- 本文全体のテキストアクセシビリティ（テキスト範囲、選択、キャレット、IME、点字）。
- 画面外リンクの列挙、画像、表構造、Mermaid・数式の公開。
- #661 の画面操作 API、#570 の vendored accesskit_windows パッチの変更。
- リンク認識範囲・対応 scheme の拡張、通常クリックの意味論の変更。

## 状態

- 準備 commit 時点: 未実装・未検証。CI、レビュー、GUI evidence はこの PR の current head に対して今後取得する。
