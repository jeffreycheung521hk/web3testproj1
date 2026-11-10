# 修改紀錄

## 0.2.0：審查後修訂（2026-11-10，尚未發布套件）

本次依審查意見調整實際行為、復原方式、支援範圍與程式結構。對帳依然唯讀，
維持 finalized、實際 Token Program、owner 與整數證據完整才可 matched 的原則。

### 1. 基礎設施錯誤、circuit breaker 與可修正的續跑設定

| 修改前 | 修改後 |
|---|---|
| RPC 失敗被寫為付款 needs_review，checkpoint 前進後 resume 會略過 | 基礎設施錯誤傳回 Err，整個目前批次不提交，先前批次保留 |
| 多個呼叫可持續向故障服務發送請求 | 共用可設定的 circuit breaker，開啟後本次程序停止新嘗試 |
| 只有併發上限，沒有啟動頻率限制 | 新增所有初始／重試／genesis 呼叫共用的 `--max-rps` |
| endpoint、batch、concurrency、timeout、retries 都被固定 fingerprint 綁住 | 以輸入、判定版本、finalized 與 genesis hash 固定語義，允許同鏈更換端點／憑證及執行參數 |
| 已保存的待查只能另建輸出目錄重跑 | `--resume --retry-review` 有界覆查，舊報告與更新在同一交易寫入稽核歷史 |

HTTP／transport／格式／異常 RPC 形狀／超限與鏈上執行失敗分開。
查無簽章或缺付款證據仍可保存為待查；它們不被解讀為未付款。
breaker 不自動在同一程序半開恢復；修正問題後 resume 建立新的工作階段。
`run-summary.json` 讓報告使用者辨識 complete、infrastructure_error、cancelled、error、export_error 或 exported，
並以 `processing_complete` 與 `report_available` 分開處理完成與匯出成功；最終檔案寫入失敗不宣稱最新報告可用。

### 2. CSV 匯出改為終止時一次

以前每批提交後重新產生所有歷史 CSV，批次很多時有平方成長的輸出 I/O。
現在持續逐批提交 SQLite，程序成功、可處理錯誤或 Ctrl-C 時只匯出一次；
CSV 資料掃描成本隨已提交列數線性成長。匯出仍採暫存檔、fsync 與原子 rename。

處理過程應以 SQLite progress 為準。硬終止後 CSV 可能是舊版本；
新增 `--resume --export-only`，不需 RPC 或 API key 即可重建已提交報告。
這個選項只匯出，不補查未提交輸入；`exported` 不能當作全份查核完成。

### 3. 放行可驗證的 ComputeBudget／Memo，加入 blockTime

單一標準 SPL Token 直接轉帳前後，現在允許經 Program ID、帳戶及內容驗證的 ComputeBudget
與新／舊版 Memo。檢查已知 ComputeBudget opcode、長度、空 accounts 與重複設定，
以及 Memo 編碼與必要 signer；未知或偽裝的指令仍拒絕。

保留單一 token 轉帳、無 CPI、完整前後餘額、owner、mint、authority signer 等證據閘門。
沒有放行交換路由、Token-2022、建立／關閉帳戶或多筆付款推測。

報表新增 `block_time`（RPC blockTime 的 Unix UTC 秒，可空白），
與本機查核時間 `checked_at_unix` 明確分開。網路費繼續獨立於 token 金額。

### 4. 抽出 library、typed structs、Reason 與單元測試

- `lib.rs` 提供可直接測試的程式邏輯；binary 只負責 CLI、runtime、取消及結束碼。
- typed RPC models 取代業務邏輯中大量 JSON 字串索引，區分缺失、null 與錯誤執行狀態。
- `Reason` enum 提供固定代碼；動態差異、重用列參照移到 `reason_details`。
- 付款驗證回傳具體原因，金額仍以 `u64` 與 checked arithmetic 處理。
- RPC 錯誤 tuple 改為具來源鏈的 SNAFU variants，操作邊界補 context。
- Rust 單元測試補上證據、白名單、整數與 RPC 政策；完整 CLI 故障注入驗收持續保留。
- 加入公開上游 mainnet 擷取 fixture、固定來源 commit、checksums 與第三方授權，測試真實回應形狀的保守拒絕及失敗分類。

真實 fixture 不是新作的 live 驗證；沒有把上游資料宣稱為已獨立確認 finalized。
目前真實資料測試只涵蓋不支援／失敗，直接付款 matched 的證據測試仍使用明確標示的合成資料。

### 5. 文件與 repo 呈現

README 重整為用途、離線展示、輸入、選項、分類、復原與驗證；DESIGN 對應實際責任與資源界限，
本檔逐項記錄本次行為變更。雲端設定移至 `docs/cloud-setup.md`，PR 描述由 GitHub 維護，
不在 repo 根目錄保留執行過程文件。專案程式新增 MIT LICENSE，第三方 fixture 保留 Apache-2.0，
並以 THIRD_PARTY_NOTICES.md 清楚說明授權分界。

### 續跑相容性

新版使用 semantics 3。原 semantics 1 的 state／原因格式與覆查資料結構不同，
因此拒絕直接續跑；請保留舊輸出目錄，使用相同原始 CSV 在新目錄重跑。
不要手動重設 progress 或移除 fingerprint 以繞過驗證。
新版相同輸入、相同鏈與相同判定規則可更換 RPC 端點／憑證及各項執行參數續跑。

### 驗證命令與可證明範圍

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo test --locked --doc --all-features
```

本次實際驗證結果：

| 檢查 | 結果 |
|---|---|
| `cargo fmt --all --check` | 通過 |
| `cargo clippy --locked --all-targets --all-features -- -D warnings` | 通過，零警告 |
| Rust library 單元測試 | 23 項通過 |
| Python mock RPC 驗收（由 Cargo 整合測試啟動） | 34 項通過 |
| 真實上游擷取 fixture 離線測試 | 4 項通過 |
| `cargo test --locked --doc --all-features` | 已執行，0 項（目前無 Rust 文件程式範例） |
| 離線展示／調整參數續跑／覆查／離線重建 | 19 列不重複；覆查保存 10 份舊結果 |

測試包含 Rust 單元測試、真實 fixture 離線測試及 Python 本機 mock 故障注入驗收，
涵蓋批次失敗不提交、同鏈換端點續跑、不同鏈拒絕、RPS／breaker、取消、硬終止、
一次匯出、覆查成功／失敗稽核與跨批次輸入。離線展示與續跑步驟見 README。
沒有宣稱 live devnet/mainnet 測試、實際資產驗收或 RSS 量測結果。
