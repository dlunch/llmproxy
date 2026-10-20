# Deployment QA

Verdict: CONDITIONAL PASS. Application, chart, native amd64 and armv7 cross-build checks pass; hosted deployment checks remain separate below. No unresolved implementation defects were found.

| Check | Observed result |
| --- | --- |
| `cargo fmt --check` | Passed |
| `cargo clippy --all-targets -- -D warnings` | Passed |
| `cargo test --locked -- --include-ignored` with disposable PostgreSQL and actual Claude executable against a fake local API | 17 passed, 0 failed, 0 ignored |
| Final strengthened Claude lifecycle fixtures (actual CLI excluded because already exercised above) | 5 passed, 0 failed; never-polled drop and persistence failure suppressing success covered |
| Helm lint with an existing database Secret name | Passed; informational icon recommendation only |
| `python3 tests/helm_chart.py` | Passed default Codex and optional Claude executable PVC render checks |
| actionlint | Passed |
| Native amd64 image build and runtime smoke | Passed: UID1000, startup/migrations, token rejection/authentication, live model insert/delete routing, statistics/metrics, JSON logging without token leakage, SIGTERM exit 0 |
| External Claude executable in native image | Mounted executable returned version 2.1.302 as UID1000; confirms loader/runtime dependencies, not subscription authentication |
| armv7 cross build and emulated runtime smoke | Passed after declaring required cross-libc development headers. QEMU runtime checks passed UID1000 startup/migrations, token authentication, live model insert/delete routing, statistics/metrics, JSON logging without token leakage and SIGTERM exit 0; no upstream calls |
| Hosted native ARM64, GHCR manifest publication and live Kubernetes deployment | Not performed during local QA |

The integration suite covers account/provider-scoped database model selection and immediate model-row removal; both public API protocols and authentication modes; Codex refresh persistence; Claude credential hydration, concurrent rotation, success/error persistence, function handoff, cancellation and dropped streams. A real Claude executable successfully round-tripped parallel client functions against a fake local Messages API. No live subscription account was contacted.

The chart checks verify required Secret wiring, single-replica Recreate, configuration, probes, writable temporary storage, runtime UID, optional architecture selection and executable PVC. Static workflow review confirms PR builds do not log in or publish, and manifest assembly depends on successful builds for all three platforms.

Follow-up environment checks are actual GitHub Actions completion, a three-platform published manifest, and rollout using the operator's PostgreSQL Secret and any architecture-compatible Claude executable. Local render tests do not establish Kubernetes scheduling or PVC permissions in a real cluster.
