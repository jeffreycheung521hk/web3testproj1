# 設計：唯讀付款查核與可復原對帳

## 三項核心技術與責任分工

1. **Rust async／Future**：Tokio async/await 用於 HTTP、逾時、退避、RPS 等待與取消。
   `buffer_unordered(N)` 只保持 N 個交易詳情工作；沒有為整份 CSV spawn 任務。
2. **應用層批次**：最多 256 個 signature 合併查 `getSignatureStatuses`，開啟歷史搜尋。
   詳情個別查詢，不假設供應商支援任意 JSON-RPC batch。
3. **串流**：snapshot、SHA-256、CSV 列、批次輸入及報告全部循序處理。
   SQLite 保存去重索引、待查查詢及稽核歷史；沒有無界 HashSet 或佇列。

`lib.rs` 對外提供可測試邏輯，`main.rs` 負責 CLI、runtime、Ctrl-C 與結束碼。
`engine` 編排批次與復原、`rpc` 管理網路政策、`model` 描述 typed RPC 資料、
`domain` 執行證據閘門、`reason` 提供固定原因代碼、`store` 負責串流輸入與 SQLite／匯出。
檔案、雜湊、SQLite、fsync 在一個 `spawn_blocking` worker 中執行；worker 等待有界 async 批次。
儲存完成才讀取下一批，讓下游速度限制上游。

## 鏈上狀態、付款結論與基礎設施故障

`ChainStatus` 表達 unknown／processed／confirmed／finalized／failed，
`ReconciliationStatus` 表達 matched／mismatch／needs_review／unsupported／invalid_input。
`Reason` 是穩定 enum 代碼，變動的差異欄位與列參照放 `reason_details`。
`Option` 與明確的 execution-error 欄位型別區分「缺欄位」、「err:null 成功」及「真實執行錯誤」。

需要 finalized、成功且完整證據才可 matched。核對實際 Token Program、交易 signature／slot、
token account 索引、前後 owner／mint／program、owner signer authority、指令金額及整數餘額差。
使用 `u64` 與 checked subtraction，沒有浮點數對帳。
`block_time` 保存 RPC blockTime，`checked_at_unix` 保存查核時間；兩者都使用 Unix UTC 秒。
網路費以獨立的 `fee_lamports` 呈現。

HTTP／transport／逾時／格式／異常回應形狀／大小限制屬於基礎設施錯誤，向上傳回 `Err`，
使目前整批中止，不寫入付款結果、不前進 checkpoint。這避免暫時 RPC 故障被永久保存成
付款待查並在 resume 時略過。即使同批其他網路請求已完成，也不部分提交。
查無簽章、交易 result:null、未 finalized 或缺 owner 等付款證據則是可保存且可覆查的待查結果。
明確不支援的交易版本是 unsupported，不等於 RPC 斷線或鏈上失敗。

SNAFU enum 保留 I/O、JSON、SQLite、transport 等來源鏈，操作邊界加入 context。
RPC 原始外部訊息不寫入報告，端點從 HTTP 錯誤中移除，診斷採固定安全代碼。

## 擴大可辨識範圍但保留證據閘門

legacy／v0 交易可包含一個標準 SPL Token `transfer`／`transferChecked`，
其餘外層指令只允許已驗證的 ComputeBudget、新版 Memo 或舊版 Memo。
實際 Program ID 必須出現在交易帳戶且不是 signer；ComputeBudget 檢查已知 opcode、
資料長度、空 accounts 與重複 opcode，Memo 檢查 parsed 字串或原始 UTF-8 及要求的 signer。
未知、偽裝或無法驗證的額外指令仍拒絕。

innerInstructions 必須明確為空；路由、CPI、多筆轉帳、Token-2022、delegate／multisig、
建立或關閉 token account 仍在範圍外。簽章／payment_id 重用只產生輸入待查，
不宣稱已證實重複付款，也不從相同金額推測重複付款。
工具信任 RPC 所報的鏈與 finalized 狀態，不獨立驗證共識，也不探索未知 signature。

## 重試、RPS 與 circuit breaker

適合重試的暫時 HTTP／transport 與 RPC 錯誤才採有限重試、指數退避及抖動；
永久錯誤、解析失敗及超限不盲目重試。Retry-After 支援秒數和 HTTP date，
超過 60 秒等待預算就停止，不提早重試。TLS 校驗持續開啟。

同一 `Rpc` 的所有 clone 共用 RPS 節奏與 circuit 狀態，限制初始、重試與 genesis 請求的啟動。
RPS limiter 在有界工作內等待，不預先保留無界時槽。breaker 累積連續「耗盡重試的暫時失敗呼叫」，
成功呼叫在尚未開啟時歸零；開啟後維持到本次程序結束，晚到的成功回應不會悄悄關閉它。
開啟時停止新的嘗試，但無法撤回遠端已經執行的請求。新程序／resume 建立新的工作階段。

## 有界資源與匯出成本

| 資源 | 限制 |
|---|---|
| CSV 批次 | 1–256 列 |
| 交易詳情並行工作 | 1–64 |
| 單一回應本文 | 2 MiB |
| 單一實體 CSV 列 | 64 KiB |
| 首次以外的重試 | 0–10 次 |
| 每次網路嘗試 | 最多 120 秒，不含獨立限流／退避等待 |
| 啟動頻率 | 1–1000 RPS，預設 20 |
| SQLite page cache | 約 4 MiB |

記憶體隨一批輸入／報告及 `concurrency × response_limit` 增長，另有 JSON 解析物件開銷；
serde_json 保留預設遞迴深度限制。不全量載入輸入、交易或待查索引。
磁碟包含輸入 snapshot、結果、原始付款欄位與覆查歷史，隨資料及覆查次數成長。

開始 RPC 前需要串流 snapshot 與 SHA-256 預掃描，會讀完整檔案，但不把它存入全量記憶體。
逐批提交 SQLite；CSV 只在程序可處理的終止時串流匯出一次，因此單次輸出資料掃描為 O(N)，
不再每批重新掃描所有歷史結果。這不是整個查核所有操作的嚴格 O(N) 宣稱；
SQLite 索引、覆查、輸入預掃描與網路成本另計。沒有量測或宣稱 RSS 峰值。

## 原子保存、覆查與稽核

SQLite 使用 WAL 與 synchronous=FULL。同一 append 交易保存整批結果、原始付款資料及 progress；
索引由資料庫維護，整批成功或整批回滾。輸出目錄 advisory lock 防止並行寫入。
CSV 是可重建檢視，不作 checkpoint。終止匯出採暫存檔、檔案 fsync、原子 rename 與目錄 fsync。
成功寫出的 `run-summary.json` 呈現執行狀態、已提交列數、`processing_complete` 與
`report_available`；處理不完整時，CSV 仍只代表已提交部分。即使處理成功，若最終匯出失敗，
狀態為 `export_error` 且 `report_available` 為 false；舊 CSV 不代表最新資料庫。
若寫入 manifest 本身失敗，錯誤仍會保留原本處理失敗的來源，不把清理失敗掩蓋成成功。

Ctrl-C 透過 watch 通知取消 async 批次，丟棄未提交工作後匯出既有結果，以 130 結束。
硬終止或停電可能留下舊 CSV；`--resume --export-only` 從 SQLite 離線重建，仍核對原始輸入與語義。
`exported` 表示報告重建，`processing_complete` 在這次只匯出的呼叫為 false；
不能據此判定先前查核完整或不完整。檔案系統錯誤不能保證 CSV 或 manifest 成功寫出。

`--resume --retry-review` 只選擇本次啟動前已提交的 needs_review，以磁碟索引及有界頁面讀取。
覆查更新與將前一版本寫入 `revisions` 在同一交易中；當批失敗時既不改舊結果也不追加虛假修訂。
重複輸入檢查只查目前列之前的資料，避免覆查自己時誤判 signature 重用。
同一付款的不同歷史結論可以由稽核表追溯。

## Fingerprint 與版本相容性

semantics 3 的固定條件包含輸入 SHA-256、判定語義版本、finalized 門檻與鏈的 genesis hash。
第一次網路成功後綁定 `getGenesisHash`；其後更換 RPC 供應商／憑證必須維持同一條鏈。
RPC URL、batch size、concurrency、timeout、retries、RPS 與 breaker 門檻都是執行設定，
不再放入不可改的 fingerprint。這允許修正存取與資源問題後原地續跑，也避免跨鏈混用結果。
genesis 回應本身仍位於 RPC 信任邊界內。

舊 semantics 1 的結果原因及 schema 與新版不同，明確拒絕直接續跑；
使用相同輸入在新輸出目錄重跑，保留原目錄作稽核，不靜默解讀舊資料或重設 checkpoint。
即使舊版曾保存 RPC 錯誤為 needs_review，也不直接沿用為新版付款結論。

## 驗證邊界與來源

Rust 單元測試直接涵蓋證據閘門、typed parsing、原因代碼、整數邊界、白名單、RPS、breaker
與 RPC envelope；本機 HTTP mock 驗收涵蓋完整 CLI 的錯誤／取消／續跑／稽核與匯出。
跨多批合成輸入測試檢查請求拆批與實際在途數量，延遲壞列測試確認先前批次仍已提交。

真實 fixtures 取自固定上游提交，保留原始 bytes、SHA-256、簽章、slot、blockTime 與 Apache-2.0
來源授權，詳見 [來源說明](tests/fixtures/real/README.md)。它們驗證真實回應形狀的 unsupported／failed
與時間欄位；matched 仍使用合成資料。沒有重新取得 live mainnet 回應，也沒有把上游擷取宣稱
當成本工具已獨立完成簽章、finalized 或端到端付款驗證。

協定參考：

- https://solana.com/docs/rpc/http/getsignaturestatuses
- https://solana.com/docs/rpc/http/gettransaction
- https://solana.com/docs/rpc/http/getgenesishash
- https://doc.rust-lang.org/std/future/trait.Future.html
