# Rust payment reconciliation MVP

唯讀 Solana SPL Token 交易查核 CLI，用於展示財務對帳與數位資產營運控制。
不持有私鑰、不簽署、不提交交易、不退款。退款只能作為另一筆待查轉帳輸入。

## 安裝與離線展示

需要 Rust（版本與元件由 `rust-toolchain.toml` 鎖定）、C 編譯器與 Python 3（僅 mock／驗收使用標準函式庫）。
安裝 [rustup](https://rustup.rs/) 後，在本 repo 執行：

```sh
cargo build --locked
# 終端 1；只監聽 loopback，不會呼叫真實鏈
python3 examples/mock_rpc.py --port 8899
```

```sh
# 終端 2；不需 API key
cargo run --locked -- reconcile --input examples/payments.csv --output out-demo \
  --rpc-url http://127.0.0.1:8899 --batch-size 4 --max-concurrency 2
# 同一份輸入、同一設定與同一端點
cargo run --locked -- reconcile --input examples/payments.csv --output out-demo \
  --rpc-url http://127.0.0.1:8899 --batch-size 4 --max-concurrency 2 --resume
```

`examples/payments.csv` 和 `fixtures.json` **全部為合成資料**，不是實際付款或有效鏈上證據。
19 列展示 2 筆 matched、4 筆 mismatch、3 筆 unsupported、10 筆 needs_review。

真實唯讀 RPC 請由安全環境設定注入 `RPC_URL`，再省略 `--rpc-url`。端點需為 HTTPS
（loopback mock 例外），不跟隨重新導向。端點、HTTP 回應本文與供應商錯誤訊息不會寫入報告／日誌。
勿把含憑證端點放進命令列歷史；本機 state 只保存端點的 SHA-256 指紋，不保存原值。
雲端需允許該 RPC 的網域；離線測試無此需求。

## 輸入

UTF-8 CSV，標頭及順序固定：

```csv
payment_id,tx_signature,expected_sender_owner,expected_recipient_owner,expected_mint,expected_amount_atomic
```

- `payment_id`：1–256 bytes 的業務識別碼。
- `tx_signature`：base58 解碼為 64 bytes 的交易第一簽章。
- 三個地址欄位：base58 解碼為 32 bytes；**owner 是錢包擁有人地址，並非 token account 地址**。
- `expected_amount_atomic`：1 到 18446744073709551615 的十進位整數；不接受空白、符號、小數、科學記號或前導零。完全不用浮點數對帳。

每個實體 CSV 列最多 64 KiB，可有標準引號及 escaped quotes，**不支援欄位內換行、空白列或額外欄位**。
欄位內容無效會列為 `invalid_input`；CSV 結構損壞會停止，保留先前已提交批次。
輸入列識別 `input_row` 由第一筆資料起算 1，不包括標頭。

## 選項與限制

`reconcile --help` 可列出完整選項：

| 選項 | 預設／上限 |
|---|---|
| `--input`, `--output` | 必填；一個輸出目錄只容納一次查核 |
| `--rpc-url` | 或 `RPC_URL` 環境變數 |
| `--batch-size` | 64；1–256，供應商上限較低時自行調低 |
| `--max-concurrency` | 4；1–64，限制逐筆交易詳情請求 |
| `--timeout-ms` | 10000；1–120000，每次嘗試含讀取本文 |
| `--max-retries` | 3；0–10，首次嘗試以外的重試數 |
| `--resume` | 續跑已提交進度，不重新查核已提交列 |

`getSignatureStatuses` 每批一個請求，啟用 `searchTransactionHistory: true`。
`getTransaction` 個別請求，使用 `jsonParsed`、`finalized` 與 `maxSupportedTransactionVersion: 0`，
不要求供應商支援 JSON-RPC batch。單一 RPC 本文上限 2 MiB；超限保留待查。
429、500／502／503／504、暫時 transport 錯誤及逾時有限重試，採指數退避與抖動，尊重 Retry-After。
若 Retry-After 超過 60 秒則停止該請求並保留待查，不提早重試。
永久 HTTP／RPC 錯誤、格式錯誤與超限不重試；TLS 與套件校驗保持啟用。

## 結果與查核語義

`report.csv` 包含列識別、payment_id、signature、chain_status、reconciliation_status、reason、slot、
Unix UTC 查核時間、獨立的 `fee_lamports` 及 JSON 證據。終端摘要依對帳結果計數。
網路費以 lamports 單獨呈現，絕不加減 SPL Token 金額。

| 欄位／分類 | 含義 |
|---|---|
| chain_status | unknown / processed / confirmed / finalized / failed；與付款是否吻合分開 |
| matched | finalized 且成功；單一標準 Token Program 直接轉帳，有完整 owner、mint、整數金額及餘額證據 |
| mismatch | 直接轉帳證據完整，但 sender／recipient／mint／amount 與預期不同；reason 列出差異 |
| needs_review | 尚未 finalized、鏈上失敗、查無簽章／歷史資料、交易 null、RPC 問題、缺證據或重用輸入 |
| unsupported | 路由／多指令／CPI、Token-2022、未知指令、未知版本、multisig／delegate 等未支援結構 |
| invalid_input | 地址、簽章或金額等欄位無效，未發送鏈上查詢 |

僅接受實際 `Tokenkeg...` Program 的 parsed `transfer`／`transferChecked`，交易只能有一個外層指令、
明確空的 innerInstructions，來源與目的 token account 必須能從交易帳戶索引及前後 token balances 辨識。
兩個帳戶的前後 owner、mint、program 必須一致，指令金額須等於來源減額及目的增額；authority
必須是來源 owner 且為交易簽署者。缺 owner 或 balance 不會推測匹配。支援 legacy 及 v0；
建立帳戶、關閉帳戶、額外 compute-budget 指令等即使付款有效，也會保守保留待查／不支援。
信任所選 RPC 提供的 finalized 鏈上資料；本工具不是獨立共識或簽章驗證器。

相同 signature 或 payment_id 後續出現時，後續列標示 `needs_review`，指向先前輸入列，
不宣稱為真正重複付款；第一列保留其獨立鏈上查核結果。不同 signature 的相同金額不會自動判定重複付款。

## 中斷與復原

`state.sqlite` 是權威結果庫，SQLite FULL 同步及一個原子交易同時提交整批結果、磁碟去重索引與 checkpoint。
報告是可重建檢視：每批提交後從資料庫串流輸出到暫存檔，fsync 後原子 rename。
因此「結果已寫入、checkpoint 未更新」在同一交易中會全部回滾；「資料庫已提交、CSV 尚未完成」則續跑重建報告。
Ctrl-C 以 130 結束，未提交批次可安全重查；也能從程序終止後續跑。輸出目錄的 advisory lock 防止兩個執行個體同時寫入。

程式先以串流複製輸入為 `input.snapshot`，再串流計算 SHA-256；逐列處理 snapshot，避免來源檔案中途變動。
resume 核對來源內容、snapshot、語義版本、端點指紋、批次、併發、逾時與重試設定；不符即拒絕。
完整保留輸出目錄，包括 SQLite WAL／SHM（若存在），不要手動編輯 state。
對已提交的 unknown／needs_review 重新查核：使用**新的輸出目錄**重跑相同或人工整理的待查 CSV，保留原報告作稽核。
不要藉改動 checkpoint 覆查。

## 驗證

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

Cargo 整合測試會啟動 Python 標準函式庫本機 mock，執行 18 項驗收，包括錯誤分類、
重試、併發、取消、SQLite 未提交回滾、CSV 重建、resume 拒絕變更、1,001 列跨批次及延遲出現的壞列。
測試不需 API key 或真實資產。沒有 Rust 文件程式範例，不需 doctests。
未宣稱實測 RSS 記憶體數值；資源設計與限制見 [DESIGN.md](DESIGN.md)。
