# Cloud development setup

此任務使用既有隔離 checkout `/workspace/web3testproj1`；除非使用者明確要求，不建立 Git worktree。

已實際執行的安裝腳本為 `bash scripts/setup-cloud.sh`。它使用官方 rustup 與 TLS 驗證，
在 `/workspace/.cargo`、`/workspace/.rustup` 安裝工具，並執行 frozen lockfile build。
需要既有 `curl`、C/C++ 編譯器、CMake、Python 3 與允許的 Rust／crates.io 套件來源。

每個新 shell 啟用：

```sh
export CARGO_HOME=/workspace/.cargo
export RUSTUP_HOME=/workspace/.rustup
export PATH="$CARGO_HOME/bin:$PATH"
cd /workspace/web3testproj1
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

CLI 不需常駐服務；套件／編譯產物可以保留，程序不會隨環境快照恢復。
可選離線 mock 啟動及結果驗證見 README。實際展示驗證為 19 列，matched 2、mismatch 4、
unsupported 3、needs_review 10；續跑不重複輸出。不提供 localhost 預覽連結。

本次平台的設定草稿讀取與更新工具均回傳 INVALID_ARGUMENT，無法確認 `install_script` 與
`start_skill` 已保存，這份文件與腳本保留完整可審閱的本機替代內容；不宣稱已發布或在新任務驗證。
需要在環境設定恢復保存能力後，保存腳本及上述啟動說明，再由使用者發布環境。

GitHub Git 代理與 GitHub API 是不同能力；建立 PR 需要 `api.github.com` 網路存取與平台提供的
有效 API 授權。本次 API CONNECT 曾被代理 403 拒絕。若使用真實 RPC，另允許指定供應商網域，
並透過安全環境變數提供 RPC_URL；不需為離線展示提供任何秘密。
