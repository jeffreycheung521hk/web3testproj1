# 設計：Rust 唯讀付款對帳

## 三項核心技術

1. **async / Future**：Tokio async/await 在等待 HTTP 與退避時釋出工作執行緒。
   futures `buffer_unordered(N)` 只輪詢 N 個交易查詢，不為全量輸入 spawn 任務。
   CSV、SQLite、hash、報告 fsync 在單一 `spawn_blocking` worker 中執行；worker 利用 runtime handle
   等待一批有界網路 Future。下游落盤完成前不讀下一批，自然施加背壓。
2. **應用層批次**：每批最多 256 個 signature 使用一次 getSignatureStatuses，歷史搜尋開啟。
   finalized 且成功才取得詳情；詳情逐筆受控併發，不發送任意 JSON-RPC batch。
3. **串流**：輸入 snapshot、SHA-256、CSV 列、分批處理與報告輸出均串流執行。
   不全量載入輸入或交易；SQLite 索引去重，避免無界 HashSet。

## 確認與對帳

鏈上失敗、確認程度、資料不可得及付款差異是不同維度。只有 finalized 成功才可能 matched。
實際 Token Program ID、帳戶索引、前後 owner／mint／program、指令金額及整數餘額差共同構成證據。
只支援單一直接轉帳；禁止從複雜指令或缺資料推論付款成功。signature 重用與 payment_id 重用是輸入待查，
不等於已證實重複付款。RPC 是信任邊界，不執行獨立共識驗證。fee_lamports 不混入 token 金額。

## 有界資源

批次 1–256、詳情併發 1–64、每個回應 2 MiB、每實體 CSV 列 64 KiB、重試 0–10、每次嘗試最多 120 秒。
最大在途回應空間約隨 `concurrency × 2 MiB` 增長，加上 JSON DOM 的解析額外空間與一批付款／報告。
serde_json 採預設遞迴深度限制；不用手寫底層 JSON／HTTP parser。
SQLite page cache 約 4 MiB，去重索引與結果在磁碟上；磁碟需求隨輸入及結果線性成長，沒有宣稱無限容量。
輸入需完整串流 hash／snapshot 預掃描，所以第一個 RPC 前會讀完整檔案，但不會把檔案全量載入記憶體。
為簡化稽核，每批重新產生 CSV，總輸出 I/O 在小批次大資料量時可能呈平方成長；資料庫查詢本身串流。

19 項本機驗收包含 1,001 列／8 批、實際 mock HTTP 在途數量檢查，以及第 9 列超長時前兩批仍已提交，
證明解析／網路沒有等到全量資料成功解析才執行。沒有量測或宣稱 RSS 峰值。

## 復原與一致性

SQLite 交易同時寫 results、去重索引及 progress；FULL 同步、WAL journal。整批成功或整批回滾。
CSV 不充當 checkpoint，避免兩個檔案無法原子提交的漏洞。CSV 經暫存檔 fsync、rename 與目錄 fsync，
任何時候都可由資料庫重新建立。取消會終止整個程序；SQLite 負責回復未完成交易。

resume 比對輸入 SHA-256、immutable snapshot 及判定／執行設定。端點只保存 SHA-256 指紋，沒有原始憑證。
同一輸出目錄 advisory lock 保護整個執行流程。來源於第一次 snapshot 後變動不影響當次判定；下次 resume 會拒絕變動。
未提交列重查；已提交待查項目透過新輸出目錄覆查，不靜默修改原始稽核結果。
測試刻意回滾未提交結果並續跑，另測取消、部分 CSV 與完整 CSV 的重建。

## 錯誤與協定依據

I/O、CSV、SQLite、JSON 與 transport 錯誤由 SNAFU enum 保存 source。
RPC 的外部文字不進入報告，僅輸出固定錯誤分類；HTTP client 錯誤移除 URL。
429、指定暫時 5xx、讀取 transport／逾時有有限指數退避＋時間抖動；Retry-After 秒數與 HTTP date 均支援。
永久 HTTP／JSON-RPC 錯誤、格式與大小限制直接保留待查。

核對官方來源：

- https://solana.com/docs/rpc/http/getsignaturestatuses （最多 256、history、confirmationStatus／err）
- https://solana.com/docs/rpc/http/gettransaction （null、commitment、jsonParsed、版本）
- https://doc.rust-lang.org/std/future/trait.Future.html （惰性 Future、poll／wake、await）

網站存取受雲端網路規則限制，本次從 `solana-foundation/solana-com` 官方 Git 儲存庫
`31f66e08e6c37c8080ca05e554ff798a4beb82fc` 的英文文件核對前兩項，
並閱讀 `rust-lang/rust` 官方 core Future 原始碼。依賴版本由 crates.io 實際解析、Rust 1.99.1 編譯與 Cargo.lock 鎖定。
