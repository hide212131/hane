# ADR-0017: Phase 4 Polish 実装計画

## Status

Accepted

## Context

Phase 3はMarkdown sourceを唯一の正に保ちながら、記号の段階表示と非一対一のSource ↔ Visual mappingを完成させ、Phase 4への判断をGoとした。

Phase 4ではRFPにある画像、表、保存、自動保存、Recent Files、設定、themeを追加する。ただし、100 MB文書の入力pathで全文変換や同期I/Oを行うと、これまで守ってきたlatencyとmemoryの性質を失う。

## Decision

Phase 4はこれまでと同じく、設計判断、実装、単体テスト、実UI検証、性能測定、reportの順で進める。

1. 画像はMarkdown imageのalt textとdestinationをsource range付きで抽出する。非active行では可視範囲にある画像だけGPUIへ渡し、active行ではsourceを段階表示して編集可能性を優先する。
2. 表はpipe tableを行単位でpresentationする。sourceのpipe/alignment markerをhidden、画面上のcell separatorをsynthesized segmentとし、canonical mapping testを追加する。
3. `Save` / `Save As`はMarkdown source bytesを変換せず、同一directoryの一時fileへのwrite、flush、renameでatomicに置換する。
4. 保存処理は共有Rope snapshotをbackground executorで書き出す。入力pathでは全文`String`化もfile I/Oも待たない。
5. 自動保存は最後の入力から750 ms debounceし、対象revisionとpathが現在値に一致する場合だけ開始する。新しい入力があれば古い要求を破棄する。
6. Recent Filesは最大10件を重複なしで永続化し、OSのrecent documentsにも通知する。存在しないpathは表示時に除外する。
7. 設定はautosaveのon/offとthemeのsystem/light/darkを対象とする。設定変更は永続化し、themeはOS appearance変化を追従する。
8. 画像decode、table parse、保存、設定読込の失敗はeditorを失わせずstatusへ表示する。
9. Phase 3と同じformat、clippy、test、実UI、latency、scroll、startup、RSS gateを回帰確認する。

## Phase 4での非目標

- network画像のdownload管理、画像編集、drag and drop upload。
- GFMのcell spanning、HTML table、複数行cell。
- 複数window/tab、cloud同期、外部変更の自動merge。
- 設定項目のplugin化や高度なtheme editor。
- packaging、署名、配布。

## Completion criteria

- local imageとpipe tableが通常表示され、active時には元Markdownを編集できる。
- synthesized segmentを含むSource ↔ Visual mappingがUnicodeと境界affinityでcanonicalになる。
- Open、Save、Save Asで元Markdownがbyte単位で保存される。
- 自動保存がdebounceされ、stale revisionを保存済みとして扱わない。
- Recent Files、autosave設定、theme設定が再起動後も復元される。
- format、clippy、workspace testがpassする。
- 実UI capture、性能・memory測定、Phase 4 reportがある。

## Consequences

Phase 4のpolishを追加しても、入力pathはlocal editと可視行cache invalidationのまま保たれる。画像・表がsourceと一対一でない場合もPhase 3のSourceMap契約を再利用でき、保存内容はpresentationから独立する。
