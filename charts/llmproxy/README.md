# llmproxy Helm chart

Requires an existing PostgreSQL database, a Secret in the release namespace containing its connection URL, and two public HTTPS origins under the same site. The chart creates separate API and dashboard ConfigMaps, Deployments, and ClusterIP Services. Each Service listens on port 8080. API database migrations run at startup.

Choose your public domains before installing; the addresses below are illustrative:

```sh
helm upgrade --install llmproxy ./charts/llmproxy \
  --set database.existingSecret=llmproxy-database \
  --set dashboard.apiUrl=https://api.example.com \
  --set dashboard.origin=https://dashboard.example.com
```

The Secret key defaults to `DATABASE_URL`. Supply credentials through your normal Secret management process. The chart does not create database credentials or PostgreSQL. After startup initializes the database, issue a signup code with `scripts/issue-signup-code.sql`, then open the dashboard to create an account and configure providers as described in the [repository README](../../README.md).

| Value | Default | Purpose |
| --- | --- | --- |
| `image.repository` | `ghcr.io/dlunch/llmproxy` | Multiarchitecture proxy image |
| `image.tag` | `latest` | Use a published `sha-<commit>` tag for a pinned release |
| `image.pullPolicy` | `Always` | Refresh the mutable `latest` tag |
| `replicaCount` | `2` | API replicas; credential refresh uses PostgreSQL row locks |
| `imagePullSecrets` | `[]` | Existing registry Secrets used by both deployments, e.g. `[{name: ghcr}]` |
| `database.existingSecret` | Required | Secret containing the PostgreSQL URL |
| `database.key` | `DATABASE_URL` | URL key in the Secret |
| `config` | Host settings with secure cookies | API TOML host settings; `dashboard_origin` is appended from `dashboard.origin` |
| `terminationGracePeriodSeconds` | `180` | Graceful request and OAuth refresh shutdown |
| `resources` | `{}` | API container resource requests and limits |
| `dashboard.image.repository` | `ghcr.io/dlunch/llmproxy-dashboard` | Independent static Nginx image |
| `dashboard.image.tag` | `latest` | Dashboard image tag; pin alongside the API image |
| `dashboard.image.pullPolicy` | `Always` | Dashboard pull policy |
| `dashboard.replicaCount` | `2` | Dashboard replicas |
| `dashboard.resources` | `{}` | Dashboard container resource requests and limits |
| `dashboard.apiUrl` | Required | API origin used directly by browser requests |
| `dashboard.origin` | Required | Exact public dashboard origin allowed by management CORS |

Override `config` using `--set-file config=your-config.toml` or a values file. Keep `listen = "0.0.0.0:8080"` for the Service and health probes. Do not repeat `dashboard_origin` in this TOML; set `dashboard.origin` instead. Both origins include the scheme and optional port, without a path or trailing slash. Increase the termination grace period when increasing `request_timeout_seconds`: allow at least 30 additional seconds for an in-flight OAuth refresh plus finalization margin.

Both Deployments use rolling updates and separate pod selectors. PostgreSQL stores management sessions, provider logins, credentials, and routes; row locks serialize credential updates across API replicas. No sticky sessions or persistent application volume are required. API configuration and `dashboard.origin` changes roll the API Deployment. `dashboard.apiUrl` changes roll only the dashboard, whose public JSON configuration is mounted at `/usr/share/nginx/html/config.json`. Both images run as UID/GID 1000 with writable temporary storage at `/tmp`; the dashboard root filesystem is read-only.

The image includes the official Claude executable at `/usr/local/bin/claude` for amd64 and arm64, selected by `claude_executable`. Register Codex or Claude provider instances, complete their login, and set model routes through the management API. These changes apply without a rollout. Subscription credentials remain in PostgreSQL; request files use temporary storage.

Use your existing external routing/TLS setup to publish the two Services; this chart does not install an Ingress controller or manage certificates:

| Public origin | Service for release `llmproxy` | Traffic |
| --- | --- | --- |
| `dashboard.apiUrl` | `llmproxy:8080` | `/management`, `/v1/chat/completions`, `/v1/responses`, including streams |
| `dashboard.origin` | `llmproxy-dashboard:8080` | HTML, CSS, JavaScript, and public `config.json` |

The browser calls the API origin directly. Dashboard Nginx serves static files only. Its configuration response is not cached, and other assets revalidate. The chart uses `cookie_secure = true`; publish both domains over HTTPS and under the same site, such as sibling subdomains, for SameSite=Strict management cookies. The API allows credentialed management CORS only for the configured dashboard origin. Mutations require `X-LLMProxy-Management: 1`. `/health` and `/metrics` share the API listener. Use bearer tokens for inference behind shared proxies; CIDR authentication observes the TCP peer.

Validate rendering with:

```sh
helm lint charts/llmproxy --set database.existingSecret=llmproxy-database \
  --set dashboard.apiUrl=https://api.example.com \
  --set dashboard.origin=https://dashboard.example.com
python3 tests/helm_chart.py
```

The render checks require Helm, Python 3.11+, and PyYAML.
