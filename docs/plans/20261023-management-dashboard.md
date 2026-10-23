# Static account dashboard

- Date: 2026-10-23
- Status: Implemented and tested; publication and deployment follow the code commit.

The dashboard consumes the account management API directly from a separate HTTPS origin. A buildless Korean UI covers accounts, API tokens and metrics, provider OAuth, and atomic model routing. Nginx serves only static files; Helm manages separate API/dashboard Deployments and runtime public configuration. Management-only CORS preserves host-only HttpOnly session cookies on sibling domains.

The user authorized push and installation in the connected cluster at `api-llmproxy.dlun.ch` and `llmproxy.dlun.ch`, with values stored in the homelab Helm directory. Existing PostgreSQL and Traefik provide database and public routing.

- [Requirements](../forge/20261023-management-dashboard/prd.md)
- [Architecture](../forge/20261023-management-dashboard/architecture.md)
- [Review](../forge/20261023-management-dashboard/implementation-review.md)
- [QA](../forge/20261023-management-dashboard/qa-report.md)
- [Delivery and limits](../forge/20261023-management-dashboard/final-report.md)

Live subscription OAuth/inference remains account-dependent and unverified. Local verification includes real browser/API/database flows, OAuth UI fixtures, race regressions, responsive layout, static container behavior, Helm, and affected Rust tests.
