# Static account dashboard

## User requirements

Build the dashboard after the management API commit. The user explicitly requires a distinct Nginx plus static-site image, a separate Deployment, and Helm deployment. The confirmed public origins are `https://api-llmproxy.dlun.ch` and `https://llmproxy.dlun.ch`. The API origin serves both management and completion/streaming endpoints. The dashboard calls that API origin directly.

## Scope

Provide Korean browser UI for signup with email/password/code, login/logout, account label, provider registration and label/enablement, initial Codex/Claude OAuth, model routing and strategy, multiple API tokens, and per-token statistics. Display issued token secrets once without browser storage. Use the existing API contract. Explain empty states, pending authentication, validation failures, and expired sessions. Keep navigation, forms, feedback, and layout accessible on desktop/mobile.

Use buildless HTML/CSS/JavaScript. Static Nginx serves assets only, with no reverse proxy or application backend. Runtime public configuration contains the API URL and no secrets. A distinct dashboard Deployment/Service and independently configurable image belong to the existing Helm release. The user authorized pushing the completed commits, publishing both multi-platform images, and installing the release in the currently connected Kubernetes cluster. Store deployment values in the homelab repository's `helm` directory and use the existing PostgreSQL and Traefik Gateway.

The API allows credentialed management CORS from exactly the configured dashboard origin. Retain the custom mutation header and HttpOnly session cookie. The expected HTTPS sibling domains are same-site; no broad wildcard CORS or JavaScript session tokens. Keep inference endpoint behavior unchanged.

## Verification

Exercise real browser signup/login, token creation/revocation and statistics, provider registration/configuration, routing, account changes and logout across separate origins against the real local API/database. Test OAuth UI with fake browser responses and existing backend OAuth fixtures, not live subscription accounts. Check hostile-origin preflight, cookie behavior, CORS isolation from inference, empty/error states and responsive layout. Build/run the static image, inspect Nginx configuration, and validate Helm manifests use distinct selectors/images/services and safe JSON config. Run affected Rust tests, formatting and Clippy. After image publication, verify cluster rollouts, route acceptance, public HTTPS, CORS, and inference authentication. Report live subscription OAuth and inference as unverified unless separately exercised.
