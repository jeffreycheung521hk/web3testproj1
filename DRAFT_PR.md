# Draft PR: Add read-only Rust SPL Token payment reconciliation MVP

Reads expected-payment CSV records in bounded batches, searches historical Solana signature statuses, and checks finalized successful transactions using individually bounded async detail requests. Reports matched payments only when standard Token Program instructions, wallet owners, mint and integer balance changes agree. Keeps chain confirmation/execution state separate from reconciliation, and flags reused identifiers for review without claiming proven duplicate payments.

Includes crash-safe SQLite batch commits/checkpoints and disk-backed deduplication, input/settings fingerprint checks, regenerable CSV reports, HTTP response/timeout/retry limits, a fully synthetic offline RPC/demo, Rust CI, README, DESIGN.md and tested cloud setup instructions. No keys, signing, payments or refunds.

Validation on this branch:

- `cargo fmt --all --check`: passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo test --all-targets --all-features`: passed; one Rust integration harness ran 18 Python standard-library acceptance tests against a local mock RPC.
- Offline demo and resume: 19 rows, matched 2 / mismatch 4 / unsupported 3 / needs_review 10; no duplicate output on resume.
- Tests cover owner/mint/amount mismatches, non-finalized/failed/null/missing/unsupported evidence, batching and bounded concurrency, 429 Retry-After, temporary 503, timeout, disconnect, permanent errors, response cap, cancellation, atomic batch rollback, report rebuilding, fingerprint rejection, u64 maximum arithmetic, preserved local files and 1,001 rows across eight batches.
- No Rust documentation code examples; no doctests applicable. No live devnet test or RSS measurement claimed.

Scope limits: one outer standard SPL Token transfer/transferChecked instruction, no CPI, explicit complete owner and pre/post balances, owner signer authority, legacy/v0 only. Token-2022, routing, account creation/closure, extra compute-budget instructions and delegate/multisig patterns are deliberately conservative. CSV physical lines limited to 64 KiB without embedded newlines; RPC bodies limited to 2 MiB. CSV regeneration after every batch favors simple recovery over large-input output I/O efficiency.

Repository initially had no commits or remote branches. Local history starts with an empty baseline commit; main remains untouched pending the requested base-branch decision. GitHub API access and an existing remote base branch are prerequisites to creating the Draft PR. Environment draft persistence returned INVALID_ARGUMENT; scripts/setup-cloud.sh and CLOUD_SETUP.md retain the tested configuration locally without claiming publication.
