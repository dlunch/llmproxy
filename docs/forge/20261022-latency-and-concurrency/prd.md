# Request latency and provider concurrency

## Purpose and scope

Implement all five improvements accepted by the user: bounded-cost usage reads, fewer credential/quota database round trips, asynchronous log output, concurrent Claude inference without a lifetime credential lock, and request-stage latency measurements. The existing Rust API/gateway/provider boundaries, PostgreSQL authority, both API surfaces, complete body logging, quota semantics, and cancellation/accounting guarantees remain requirements.

## Required behavior

- Routing usage reads must not aggregate the growing request history. Persist account/provider token totals atomically with request finalization, preserving known usage on failure/cancellation and avoiding double counting. Preserve existing data; do not reset production state or introduce a compatibility path.
- Valid Codex credentials use a read path without an exclusive transaction. Refresh must lock, re-read and deduplicate across replicas; cancellation must not lose a rotated token. Store all observed quota windows atomically with one SQL statement and retain observation ordering.
- Write logs on a dedicated worker using an established library. Preserve JSON fields and request/chunk/response logging. Use bounded buffering without intentional drops and drain logs on normal shutdown. State saturation limits honestly.
- Independent Claude requests using one provider can reach the CLI/upstream concurrently. Keep credential refresh coordinated and persisted across replicas and cancellation. Preserve CLI integration, isolated conversations, tools, quotas, shutdown, and supported OAuth behavior. Do not simply remove synchronization while retaining competing refresh writers.
- Measure authentication, routing, credential waiting/preparation, provider preparation, first actual text, and total request duration with useful millisecond-scale buckets and bounded labels. First-frame timings must not be presented as first-token timings. Include authentication/routing in end-to-end timing.

## Validation and delivery

- Extend existing PostgreSQL and provider integration fixtures for concurrent inference, refresh races, cancellation, accounting, quota ordering, logging drain/backpressure, and metrics timing as appropriate. Reuse coverage and avoid production APIs solely for tests.
- Run fmt, clippy, functional tests including official Claude CLI against a fake local API, and repeatable release measurements against commit `2d13928`. Include realistic accumulated request history and report workload/concurrency/logging limits. No real subscription calls are needed.
- Keep changes cohesive, avoid speculative caches/configuration/process pools, and document query/write/lock tradeoffs. Internal documents and comments are English.

## Assumptions

The user approved the concrete list with “다 고쳐보자”; no further product clarification is necessary. No fixed speedup target is promised before measurement. Existing data must survive schema changes; adding an ordinary schema evolution with data initialization is not an obsolete-runtime compatibility layer. Deployments are not modified by the local verification work.
