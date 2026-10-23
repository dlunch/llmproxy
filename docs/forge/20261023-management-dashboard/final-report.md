# Static account dashboard delivery

## Result

The dashboard provides Korean signup/login, account settings, provider registration and OAuth, model routing, multiple API tokens and per-token metrics. Buildless browser assets run in a separate unprivileged Nginx image and Deployment. Runtime JSON selects the API origin; exact credentialed CORS applies only to management routes. Both images use the existing amd64/arm64 publication workflow.

## Verification

- Configuration, management API and management OAuth tests: five passed with disposable PostgreSQL where required. Formatting and Clippy across all targets passed.
- Real browser integration against the API and static container passed, including a multibyte password, cross-origin HttpOnly cookies, token creation/revocation/statistics, provider edits, atomic route errors, account updates, reload, and logout. OAuth UI used local response fixtures.
- Browser regressions cover overlapping routing reads, failed logout draft preservation, and logout during a pending refresh. No uncaught browser errors or browser-storage secrets were observed. Desktop and 390-pixel mobile screenshots were visually inspected; mobile had no horizontal overflow.
- Static image build, unprivileged/read-only container, Nginx configuration, cache/security headers, Helm render/lint checks and workflow syntax validation passed. Independent architecture and implementation reviews passed.

## Load and limitations

The dashboard reads existing management endpoints on initial load and explicit refresh, including one metrics request per token. Provider OAuth polling follows server timing and stops on dialog/account transitions. No database schema, inference queries or inference middleware changed in this dashboard commit.

Live subscription authentication and inference were not exercised. Local tests do not prove image publication or Kubernetes health. The authorized delivery continues by pushing both commits, waiting for both image manifests, and installing the confirmed sibling domains in the connected cluster using the homelab values and HTTPRoutes. Operational results are reported separately after deployment.
