# Static dashboard architecture

## Components

- `dashboard/index.html`, `app.js`, `style.css`: one buildless browser application with semantic forms and navigation. All user data enters text nodes/DOM properties, not interpolated HTML. Fetch `config.json` once, then call the configured API origin with credentials included and the management mutation header. Keep API-token plaintext only in the one-time display until dismissed/logout. Do not persist passwords, cookies, OAuth codes or tokens in local/session storage.
- `dashboard/config.json`: local development API origin; Helm overrides this single file with a ConfigMap containing `apiUrl`. Validate an HTTP(S) origin before sending credentials. Domain values remain deploy-time settings.
- `dashboard/Dockerfile` and Nginx configuration: maintained unprivileged Nginx base, port 8080, static files only, `/tmp` writable, no API proxy. Static HTML/config revalidate; no inline scripts or third-party runtime assets. Security headers forbid framing and sniffing. Browser JS owns API interaction.
- API: optional `dashboard_origin` in host configuration, validated as an exact HTTP(S) origin. Add the existing ecosystem's `tower-http` CORS middleware, enabled only on management routes. Allow credentials, exact origin, GET/POST/PATCH/PUT/DELETE, Content-Type and X-LLMProxy-Management. Preflight must run outside session authentication. Keep existing CSRF mutation header and cookie flags. Sibling HTTPS domains preserve SameSite=Strict semantics; unrelated sites are outside the chosen deployment contract.
- Helm: existing API Deployment/Service plus a separate dashboard Deployment/Service with disjoint selectors, image, replicas/resources. Required dashboard API URL and origin populate frontend JSON and API configuration respectively. No domain defaults imply the illustrative addresses are final. Existing external routing/TLS publishes the two Services under their respective domains. No Ingress controller or TLS automation is introduced.
- Image workflow: extend the existing native architecture matrix to build API and dashboard independently, preserving their separate digest artifacts and manifests/tags.

## UI behavior

Desktop sidebar, compact mobile navigation, calm neutral background and restrained teal accent. Auth screen contains login and signup forms. Signed-in views cover overview, tokens, providers, routing, and account. Show actual statistics only; no synthetic charts. Token metrics describe admitted request history. Provider status means saved credentials, not proof of current subscription validity. Codex displays a browser link and device code, polls no faster than server timestamps, and stops on terminal state/closed dialog/logout. Claude accepts the full code from its browser flow. Pending asynchronous results must not repopulate a logged-out or different account's UI.

Routing edits stay local until one atomic PUT. Preserve server values and foreign/missing provider errors; label each model alias, provider, upstream model and priority. Revoke tokens with an explicit UI action; show revoked history and clear one-time secrets on account transition. Errors remain visible and do not erase unsaved form values.

## Delivery blocks

1. CORS/config foundation with integration tests (root).
2. Static browser UI against that contract (frontend agent).
3. Nginx image, Helm and image workflow (deployment agent).
4. Independent review, browser/container QA, documentation and separate commit.

Blocks 2 and 3 share only the `dashboard/config.json` contract; the frontend agent owns the three UI files and the deployment agent owns Docker/config/chart/workflow files. Root serializes Cargo, runs browser QA, and fixes integration issues. An independent reviewer checks the architecture before these blocks begin and final code afterward.

## Sources

Use maintained platform facilities rather than custom CORS or an application web server: [tower-http CORS](https://docs.rs/tower-http/latest/tower_http/cors/struct.CorsLayer.html), [Fetch credentials](https://developer.mozilla.org/en-US/docs/Web/API/Request/credentials), [same-site subdomains](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Sec-Fetch-Site), and [unprivileged Nginx](https://github.com/nginx/docker-nginx-unprivileged).
