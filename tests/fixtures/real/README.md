# 公開 mainnet RPC 擷取資料

這三份檔案來自 `gagliardetto/solana-go` 公開提交、上游標示為 mainnet 擷取的 RPC fixture，
與 `examples/fixtures.json` 的合成付款資料分開保存。上游
[`introspection/testdata/README.md`](https://github.com/gagliardetto/solana-go/blob/098741868128ef93ad973cfc0d5b2e14fc5c675b/introspection/testdata/README.md)
明確記錄為 **2026-11-09 從 mainnet 擷取的 `getTransaction` result**。

固定來源 commit：`098741868128ef93ad973cfc0d5b2e14fc5c675b`。
原始 JSON 逐位元組保留；沒有改寫交易、補造狀態、變更地址或移除指令。
檔名僅移除上游 `.jsonParsed` 中綴。詳盡來源、簽章、slot、blockTime、版本與
原始／留存檔 SHA-256 見 `provenance.json`，可用 `sha256sum -c SHA256SUMS` 驗證。
上游原始擷取說明及 Apache-2.0 授權分別保存在 `UPSTREAM-README.md` 與 `UPSTREAM-LICENSE`。

| fixture | 涵蓋情況 | 本工具預期 |
|---|---|---|
| `sync_native.json` | legacy、System transfer + SPL Token SyncNative，實際 SOL 包裝交易 | unsupported；不當成標準 SPL 付款 |
| `swap_v0.json` | v0、ComputeBudget、lookup-table 帳戶、多層 CPI 的 Jupiter swap | unsupported；有 token transfer 仍不從複雜路由推測付款 |
| `failed_v0.json` | v0、Jupiter swap 執行失敗、InstructionError 與部分 innerInstructions | chain failed、needs_review；不能 matched |

這些資料驗證真實 RPC 回應的 typed parsing、blockTime／slot 保留及保守拒絕行為。
直接轉帳的 matched 與錯誤金額等案例仍使用明確標示的合成 fixture；
**沒有宣稱已用真實直接付款 fixture 驗證 matched**。

本次沒有重新連線 mainnet、獨立驗證上游擷取宣稱、查驗簽章或證明 finalized。
這些檔案只有交易詳情，沒有對應 signature-status 擷取；測試直接進入純對帳函式，
所以不代表端到端 finalized 付款已經驗證。預設測試完全離線，不使用任何資產或 API key。
