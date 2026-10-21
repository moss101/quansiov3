# INT-002 reconciliation: live vision run

Owner: model gateway + Context plane (`python/intelligence`, `crates/server/src/intelligence/model_source.rs`); no new owner.
Touched: tests/e2e spec and harness option, dev-seed browse rule (read.internal on target browser). No persistent state, contracts or migrations.
External effects: browser.navigate (network.egress.new_destination, approved by the person), browser.screenshot (read.external).
Real boundary: live OpenAI-compatible provider (`QUANSIO_TEST_OPENAI_COMPATIBLE_*`, secret never recorded) and a real Chrome on a real worker.
Rollback: none; test-only.
