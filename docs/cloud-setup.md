# Cloud development setup

使用既有隔離 checkout `/workspace/web3testproj1`；除非使用者明確要求，不建立 Git worktree。

## 安裝

執行 `bash scripts/setup-cloud.sh`。腳本使用官方 rustup 與 TLS 驗證，
在 `/workspace/.cargo`、`/workspace/.rustup` 準備釘選工具鏈，再用 Cargo.lock 建置。
需要既有 curl、C/C++ 編譯器、CMake、Python 3，以及允許的 Rust／crates.io 套件來源。

## 每個新 shell

```sh
export CARGO_HOME=/workspace/.cargo
export RUSTUP_HOME=/workspace/.rustup
export PATH="$CARGO_HOME/bin:$PATH"
cd /workspace/web3testproj1
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo test --locked --doc --all-features
```

已安裝套件與編譯產物可以保留；程序、連線與 runtime 認證不應假設能隨環境快照恢復。
CLI 不需要常駐服務；可選離線 mock 的啟動與驗證見 [README](../README.md)。
請使用新的輸出目錄，或依 README 的 resume／export-only 規則操作。

## 可重用的環境設定

`install_script` 可使用 `scripts/setup-cloud.sh` 的完整內容。
`start_skill` 應記錄上面的工作目錄、工具啟用、檢查命令，以及：

- 用現有隔離 checkout，不自行建立 worktree。
- 需要展示時啟動 `python3 examples/mock_rpc.py --port 8899`。
- 合成展示應有 19 列：matched 2、mismatch 4、unsupported 3、needs_review 10。
- CLI 從 SQLite 原子保存批次，結束時才匯出 CSV；離線重建使用 `--resume --export-only`。
- 開始實際網路操作前，驗證目的網域及認證；Git push 權限不代表 GitHub API 權限。

GitHub API 操作需要允許 `api.github.com`。真實 RPC 另需允許指定供應商網域，
透過安全環境設定提供 `RPC_URL`；不要把含憑證端點寫入文件或日誌。
離線 demo 及預設測試不需任何 API key，也不使用真實資產。
