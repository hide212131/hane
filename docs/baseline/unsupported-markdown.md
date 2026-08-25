# R0.5 Markdown pending / unsupported contract

R0.5時点で解析できても表示モデルが正式対応していない構文を明示する。これらはsource bytesを
保持し、編集・保存を継続できるraw-source fallbackを期待値とする。R3.25からR4Bで対応するまで、
場当たり的な行文字列判定を追加しない。

| 構文 | R0.5時点の契約 | 対応予定 |
|---|---|---|
| 複数行quote / list item | CommonMark block rangeは保持するが、表示は物理行単位 | R4A–R4B |
| fenced / indented code block | 背景contextとsource rangeは保持するが、単一の複数行block layoutは未対応 | R4A–R4B |
| pipe table | 背景contextで行を関連付け、sourceを保持する。table node/cell階層は未対応 | R3.25–R4B |
| Setext heading | parserはheadingとして保持するが、複数行表示としてのmarker disclosureは未対応 | R3–R4B |
| `1)` ordered list | parserはlist itemとして保持するが、presentationのmarker非表示は未対応 | R3 |
| reference link | parserはlinkとして保持するが、reference markerの段階表示は未対応 | R3 |
| soft wrap | source上の改行とvisual layout lineを分離するモデルは未対応 | R4B |

escape済みmarkerはMarkdown構文に昇格させず、raw sourceとして表示・保存する。未対応構文でも
source↔visualの正規化、カーソル移動、IME、保存が壊れないことを優先する。

