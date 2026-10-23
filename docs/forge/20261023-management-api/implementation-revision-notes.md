# Implementation review decisions

- **Self-service CIDR identity capture: accepted.** CIDR CRUD was inferred extra scope, not an explicit user requirement. Removed the routes, store writes, schema additions, and associated tests. Operator-provisioned inference IP authentication remains intact. Overlap checks alone would not prove network ownership.
- **Unbounded background credential refresh: accepted.** Process at most four providers concurrently using the existing futures stream. Retain started exchanges through persistence, and skip queued work once shutdown begins. Extended the existing maintenance integration test with eight blocked providers and cancellation.
- **Private management response caching: accepted.** Reuse management middleware to set `Cache-Control: no-store`; the existing session test checks it.
- **Operator cutover documentation: accepted.** README now identifies the rejected static provider configuration and explicit account/provider data prerequisites without retaining obsolete runtime paths.

The core request path retains two parallel routing reads, no history aggregation, and one ordinary credential read for fresh credentials. Additional generation reads apply only to the existing locking/refresh paths.
