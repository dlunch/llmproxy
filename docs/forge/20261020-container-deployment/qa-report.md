# Deployment QA

Verdict: PASS for local validation. Application, distributed credentials, simplified chart, latest-release image build and bundled official executable checks passed. Hosted native ARM64, GHCR publication and live-cluster rollout remain unverified here.

| Check | Observed result |
| --- | --- |
| `cargo fmt --check` | Passed |
| `cargo clippy --all-targets -- -D warnings` | Passed |
| Full integration suite with disposable PostgreSQL and actual Claude executable against a fake local API | 17 passed, 0 failed, 0 ignored |
| Strengthened separate-instance Claude overlap fixture | 1 passed; other tests filtered |
| Final simplified Helm lint/render and actionlint | Passed; Helm reports only the informational icon recommendation |
| Unmodified official Claude 2.1.304 protocol fixture | Passed 1 actual-CLI parallel function round-trip test against a fake local API; checksum-verified official download |
| Final latest-release native amd64 image | Passed remote latest-descriptor retrieval, official release download, SHA-256 verification and bundled Claude 2.1.304 startup as UID1000 |
| Final image proxy smoke | Passed startup/migrations, token authentication, live model insert/delete routing, provider statistics, metrics, JSON logging and SIGTERM exit 0 |
| Hosted native ARM64, GHCR publication and live Kubernetes rollout | Not established by local QA |

The integration suite covers account/provider-scoped model selection and live model-row changes; both public API protocols and authentication modes; Codex refresh persistence across independent instances; Claude credential hydration, separate-instance rotation, success/error persistence, function handoff, cancellation and dropped/unpolled streams. Credential concurrency fixtures use separate single-connection pools, proving database coordination while detecting second-connection deadlocks.

No live subscription account was contacted. Local fixtures cannot establish account entitlements or live upstream authentication.
