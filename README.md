# Rust payment reconciliation

唯讀的 Solana SPL Token 付款查核 CLI：讀取預期付款 CSV，以已知交易簽章取得鏈上證據，
產生吻合、差異及待查報告。金額全程使用最小單位整數；工具不持有私鑰、不簽署、
不提交付款或退款。退款如需核對，請視為另一筆預期轉帳。

本工具驗證**已知的 signature**，目前沒有依地址、金額或時間自動尋找付款的功能。
支援範圍、復原設計及本次審查後的修改，分別見下方說明、[DESIGN.md](DESIGN.md) 與
[CHANGELOG.md](CHANGELOG.md)。雲端工作環境設定見 [docs/cloud-setup.md](docs/cloud-setup.md)。

## 安裝與離線展示

需要 [rustup](https://rustup.rs/)、C/C++ 編譯工具、CMake 及 Python 3。
Rust 版本與元件由 `rust-toolchain.toml` 鎖定；SQLite 隨 Rust 套件編譯，
Python mock 與驗收只使用標準函式庫。

```sh
cargo build --locked
# 終端 1：僅監聽 loopback，不會呼叫真實鏈
python3 examples/mock_rpc.py --port 8899
```

```sh
# 終端 2：使用全新輸出目錄，不需 API key
cargo run --locked -- reconcile \
  --input examples/payments.csv --output out-demo \
  --rpc-url http://127.0.0.1:8899 --batch-size 4 --max-concurrency 2
```

`examples/payments.csv` 與 `examples/fixtures.json` **全部是合成資料**。
展示 19 列的摘要為 matched 2、mismatch 4、unsupported 3、needs_review 10。
`report.csv` 在程序正常結束或可處理的中斷／錯誤時產生；批次進度會先保存至 SQLite。

真實 RPC 端點請由安全環境設定注入 `RPC_URL`，命令列省略 `--rpc-url`。
端點須為 HTTPS，loopback mock 例外；HTTP 重新導向不會被跟隨。
端點原文、憑證及供應商錯誤本文不進入報告或日誌。雲端需允許指定 RPC 網域。
每次連線執行先查詢 `getGenesisHash`，以鏈身分核對續跑條件。

## CSV 輸入

UTF-8 CSV，標頭與順序固定：

```csv
payment_id,tx_signature,expected_sender_owner,expected_recipient_owner,expected_mint,expected_amount_atomic
```

| 欄位 | 定義 |
|---|---|
| `payment_id` | 1–256 bytes 的業務識別碼 |
| `tx_signature` | base58 解碼為 64 bytes 的交易第一簽章 |
| `expected_sender_owner` | 發送錢包擁有人地址，base58 解碼為 32 bytes |
| `expected_recipient_owner` | 收款錢包擁有人地址，base58 解碼為 32 bytes |
| `expected_mint` | token mint 地址，base58 解碼為 32 bytes |
| `expected_amount_atomic` | 1–18446744073709551615 的十進位最小單位整數 |

**owner 是錢包擁有人，不能拿 token account 地址直接替代。**
金額不接受空白、正負符號、小數、科學記號或前導零。沒有浮點數換算或容差比對。

每個實體 CSV 列最多 64 KiB，可有標準引號及 escaped quotes；不支援欄位內換行、
空白列或額外欄位。內容驗證失敗會產生具體 `invalid_input` reason；結構損壞會停止，
保留已提交批次。`input_row` 從第一筆資料算 1，不包括標頭。

## 命令列選項

```sh
cargo run --locked -- reconcile --help
```

| 選項 | 預設／限制 |
|---|---|
| `--input`, `--output` | 必填；新查核使用空輸出目錄 |
| `--rpc-url` | 或 `RPC_URL`；`--export-only` 不需提供 |
| `--batch-size` | 64；1–256，供應商上限較低時調低 |
| `--max-concurrency` | 4；1–64，限制交易詳情的並行工作 |
| `--timeout-ms` | 10000；1–120000，每次嘗試含回應本文讀取 |
| `--max-retries` | 3；0–10，首次嘗試以外的重試數 |
| `--max-rps` | 20；1–1000，所有 RPC 嘗試共用，包括重試與 genesis 查詢 |
| `--circuit-breaker-threshold` | 3；1–100，連續暫時失敗且耗盡重試的呼叫門檻 |
| `--resume` | 保留已提交結果，接續未提交批次 |
| `--retry-review` | 與 `--resume` 一起使用，覆查先前提交的 `needs_review` |
| `--export-only` | 與 `--resume` 一起使用，離線重建已提交報告；不與覆查並用 |

`getSignatureStatuses` 每批一個請求，開啟 `searchTransactionHistory: true`。
適合核對的交易透過個別 `getTransaction` 取得 `jsonParsed` 詳情，commitment 為 finalized、
`maxSupportedTransactionVersion` 為 0；不依賴供應商接受任意 JSON-RPC batch。

429、指定暫時 5xx、適合重試的 transport 錯誤與逾時採有限重試、指數退避與抖動，
並尊重 Retry-After 秒數或 HTTP 日期。Retry-After 超過 60 秒會停止，不提早重試。
永久錯誤、格式錯誤及超限不盲目重試。TLS 與套件校驗持續啟用。
RPS 限制與 circuit breaker 由同一 client 的並行呼叫共用；breaker 開啟後，
該次程序不再啟動新嘗試，已在途請求可能仍完成。使用 `--resume` 開始新的執行工作階段。

## 報告與分類

`report.csv` 欄位：

```text
input_row,payment_id,signature,chain_status,reconciliation_status,reason,reason_details,
slot,block_time,checked_at_unix,fee_lamports,evidence
```

- `reason` 為固定的機器可讀代碼；`reason_details` 放差異欄位或重用輸入的列參照。
- `block_time` 是 RPC `blockTime`，以 Unix UTC 秒記錄，可空白；`checked_at_unix` 是本機查核時間。
- `fee_lamports` 是獨立的 SOL 網路費資訊，不混入 SPL Token 轉帳金額。
- `evidence` 保存足以追蹤辨識結果的 JSON 證據，不保存 RPC 憑證。

| 分類 | 含義 |
|---|---|
| chain_status | unknown / processed / confirmed / finalized / failed，與付款吻合程度分開 |
| matched | finalized 且成功，標準直接轉帳的 owner、mint、整數金額及交叉證據均吻合 |
| mismatch | 轉帳證據完整，但發送者、收款者、mint 或金額不同；差異放 `reason_details` |
| needs_review | 查無簽章、尚未 finalized、交易詳情 null、缺付款證據、鏈上失敗或輸入重用 |
| unsupported | 非支援版本／程式／指令、路由、CPI、無法驗證的額外指令等 |
| invalid_input | 輸入內容無效；不發送該列的狀態／詳情查詢 |

**RPC 基礎設施錯誤不會變成付款結果。** HTTP／連線／逾時／格式錯誤、異常回應形狀或
回應超限會讓程序以非零狀態停止；目前整批不提交，先前批次保留。
成功寫出的 `run-summary.json` 以 `state`、`diagnostic` 與 `committed_rows` 說明執行狀態。
`processing_complete` 表示這次呼叫是否完成查核流程；`report_available` 表示是否成功匯出
目前資料庫的報告。`infrastructure_error`、`cancelled` 或 `error` 的 CSV 僅含已提交部分。
`export_error` 表示最新報告未成功寫出，即使資料庫已有完整結果，也不能把舊 CSV 當成最新結果。
`exported` 表示只重建已提交結果，這次的 `processing_complete` 為 false；不代表先前一定未完成，
也不能單憑匯出成功判定全份輸入已處理。

### 何時允許 matched

支援 legacy／v0，必須只有**一個外層標準 SPL Token `transfer` 或 `transferChecked`**。
其前後可有通過驗證的 ComputeBudget 或新／舊版 Memo；以實際 Program ID 辨識，
並檢查 ComputeBudget opcode／資料長度與重複設定，以及 Memo 編碼及必要 signer。
未知或偽裝的額外指令不放行。

仍需明確空的 innerInstructions；token account 必須能對應交易帳戶及前後 token balances；
owner、mint、Token Program、指令金額、來源減額與目的增額必須一致；authority 必須是來源
owner 且為交易簽署者。少任何必要證據都不推測 matched。

Token-2022、CPI／交換路由、建立或關閉帳戶、delegate／multisig、多筆 token 轉帳尚未支援。
工具信任所選 RPC 的 finalized 資料，沒有獨立執行共識或簽章驗證。

相同 signature 或 payment_id 後續出現時，後續列保留待查並指向更早輸入列；
這不是已證實的重複付款。不同 signature 的相同金額也不自動判定重複付款。

## 中斷、續跑與覆查

每批的結果、原始付款欄位、磁碟去重索引與 checkpoint 在一個 SQLite FULL 同步交易中提交。
一批內即使部分詳情已成功，只要另一筆遇到基礎設施錯誤，該批全部不提交。

```sh
# 修正連線／憑證或服務恢復後，接續未提交資料
# 可更換同一條鏈的 RPC 端點及執行參數
cargo run --locked -- reconcile \
  --input examples/payments.csv --output out-demo \
  --rpc-url http://127.0.0.1:8899 --resume \
  --batch-size 16 --max-concurrency 2 --timeout-ms 20000 --max-retries 4
```

resume 核對輸入內容 SHA-256、immutable snapshot、判定語義版本、finalized 門檻及
`getGenesisHash` 鏈身分。不同鏈會拒絕；RPC URL／憑證、批次、併發、逾時、重試、RPS
與 breaker 門檻可以調整，不再被原始 endpoint 指紋綁死。

```sh
# 明確覆查「本次啟動前」已提交的 needs_review；其他已提交結果不動
cargo run --locked -- reconcile \
  --input examples/payments.csv --output out-demo \
  --rpc-url http://127.0.0.1:8899 --resume --retry-review

# 程序被強制終止後，從資料庫離線重建報告，不需 RPC_URL
cargo run --locked -- reconcile \
  --input examples/payments.csv --output out-demo --resume --export-only
```

覆查以有界批次讀 SQLite，不把所有待查列載入記憶體。每次更新先前報告的同一個交易也會
寫入 `revisions` 稽核表；失敗的當批不更新、不新增虛假修訂。後續重用列仍會參照更早輸入，
不會因為覆查到自己而誤判重複。

CSV 每次程序結束只匯出一次：串流寫暫存檔、fsync、原子 rename，再同步輸出目錄。
處理中的進度以 SQLite 為準，不期待 CSV 每批更新。Ctrl-C 會要求停止並匯出已提交資料，
結束碼為 130；硬終止、停電或磁碟錯誤不保證留下最新 CSV，可再用 `--export-only` 重建。

保留完整輸出目錄，包括 `state.sqlite`、`input.snapshot`、SQLite WAL／SHM（若存在）。
不要手動修改 checkpoint。輸入來源變動、不同鏈或判定語義變更應改用新輸出目錄。
**舊版 semantics 1 的 state 不會自動混用新規則**；請用同一原始 CSV 在新目錄重跑，保留舊資料作稽核。

## 測試與資料來源

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo test --locked --doc --all-features
```

Rust 單元測試涵蓋純對帳證據閘門、typed parsing、整數範圍、Memo／ComputeBudget 白名單、
RPC envelope、RPS 與 breaker。Python 本機 mock 驗收涵蓋基礎設施錯誤、批次原子性、
Retry-After、併發、取消、硬終止、離線匯出、同鏈換端點續跑、覆查稽核及跨批次資料。
全部預設測試可離線執行，不需 API key 或資產；沒有宣稱量測 RSS 峰值。

[真實回應 fixtures](tests/fixtures/real/README.md) 使用上游公開 mainnet 擷取檔，保留來源 commit、
簽章、SHA-256 與第三方授權，驗證真實形狀的保守拒絕／失敗分類及 blockTime。
**直接轉帳 matched 仍使用明確標示的合成資料驗證**；沒有重新查詢 mainnet，
也沒有將只有交易詳情的擷取冒充已驗證 finalized 的端到端付款證據。

## 授權

專案程式採 [MIT License](LICENSE)。公開上游 mainnet 擷取 fixture 保留自己的 Apache-2.0
授權，來源與分界見 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) 及
[fixture 說明](tests/fixtures/real/README.md)。
