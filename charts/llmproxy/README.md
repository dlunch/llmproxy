# llmproxy Helm chart

Requires an existing PostgreSQL database and a Secret in the release namespace containing its connection URL. The chart creates a ConfigMap, Deployment, and ClusterIP Service on port 8080. Database migrations run at startup.

```sh
helm upgrade --install llmproxy ./charts/llmproxy \
  --set database.existingSecret=llmproxy-database
```

The Secret key defaults to `DATABASE_URL`. Supply credentials through your normal Secret management process. The chart does not create database credentials or PostgreSQL. After startup initializes the database, issue a signup code with `scripts/issue-signup-code.sql`, then create an account and configure providers through the management API as described in the [repository README](../../README.md).

| Value | Default | Purpose |
| --- | --- | --- |
| `image.repository` | `ghcr.io/dlunch/llmproxy` | Multiarchitecture proxy image |
| `image.tag` | `latest` | Use a published `sha-<commit>` tag for a pinned release |
| `image.pullPolicy` | `Always` | Refresh the mutable `latest` tag |
| `replicaCount` | `2` | Application replicas; credential refresh uses PostgreSQL row locks |
| `imagePullSecrets` | `[]` | Existing registry Secrets, e.g. `[{name: ghcr}]` |
| `database.existingSecret` | Required | Secret containing the PostgreSQL URL |
| `database.key` | `DATABASE_URL` | URL key in the Secret |
| `config` | Host settings with secure cookies | Complete TOML configuration, without credentials |
| `terminationGracePeriodSeconds` | `180` | Graceful request and OAuth refresh shutdown |
| `resources` | `{}` | Container resource requests and limits |

Override `config` using `--set-file config=your-config.toml` or a values file. Keep `listen = "0.0.0.0:8080"` for the Service and health probes. Increase the termination grace period when increasing `request_timeout_seconds`: allow at least 30 additional seconds for an in-flight OAuth refresh plus finalization margin.

The Deployment uses rolling updates. PostgreSQL stores management sessions, provider logins, credentials, and routes; row locks serialize credential updates across replicas. No sticky sessions or persistent application volume are required. TOML configuration changes trigger a rollout. The process runs as UID/GID 1000, with writable temporary storage at `/tmp`.

The image includes the official Claude executable at `/usr/local/bin/claude` for amd64 and arm64, selected by `claude_executable`. Register Codex or Claude provider instances, complete their login, and set model routes through the management API. These changes apply without a rollout. Subscription credentials remain in PostgreSQL; request files use temporary storage.

Expose the ClusterIP Service through your normal TLS/network boundary. The chart uses `cookie_secure = true`, so management sessions require HTTPS. Serve management clients on the same origin as `/management`; mutations require `X-LLMProxy-Management: 1`. `/health` and `/metrics` share the application listener. Use bearer tokens for inference behind shared proxies; CIDR authentication observes the TCP peer.

Validate rendering with:

```sh
helm lint charts/llmproxy --set database.existingSecret=llmproxy-database
python3 tests/helm_chart.py
```

The render checks require Helm, Python 3.11+, and PyYAML.
