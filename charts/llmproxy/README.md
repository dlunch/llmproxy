# llmproxy Helm chart

Requires an existing PostgreSQL database and a Secret in the release namespace containing its connection URL. The chart creates a ConfigMap, Deployment, and ClusterIP Service on port 8080. Database migrations run at startup.

```sh
helm upgrade --install llmproxy ./charts/llmproxy \
  --set database.existingSecret=llmproxy-database
```

The Secret key defaults to `DATABASE_URL`. Supply credentials through your normal Secret management process. The chart does not create database credentials or PostgreSQL. Provision client accounts, provider ownership, and subscription credentials as described in the [repository README](../../README.md).

| Value | Default | Purpose |
| --- | --- | --- |
| `image.repository` | `ghcr.io/dlunch/llmproxy` | Multiarchitecture proxy image |
| `image.tag` | `latest` | Use a published `sha-<commit>` tag for a pinned release |
| `image.pullPolicy` | `Always` | Refresh the mutable `latest` tag |
| `imagePullSecrets` | `[]` | Existing registry Secrets, e.g. `[{name: ghcr}]` |
| `database.existingSecret` | Required | Secret containing the PostgreSQL URL |
| `database.key` | `DATABASE_URL` | URL key in the Secret |
| `config` | Codex example | Complete TOML configuration, without credentials |
| `claude.existingClaim` | Empty | Optional existing PVC containing `bin/claude` |
| `terminationGracePeriodSeconds` | `180` | Graceful request and OAuth refresh shutdown |
| `nodeSelector` | `{}` | Node labels, e.g. `{kubernetes.io/arch: amd64}` for a Claude binary |
| `resources` | `{}` | Container resource requests and limits |

Override `config` using `--set-file config=your-config.toml` or a values file. Keep `listen = "0.0.0.0:8080"` for the Service and health probes. Model IDs are examples; choose models supported by your subscription. Increase the termination grace period when increasing `request_timeout_seconds`: allow at least 30 additional seconds for an in-flight OAuth refresh plus finalization margin.

The Deployment runs exactly one replica with `Recreate` because credential refresh locks are process-local. Do not deploy another release using the same provider credentials. Configuration changes trigger a rollout. The process runs as UID/GID 1000, with writable temporary storage at `/tmp`.

For Claude, build an architecture-compatible executable following the repository README and place it at `bin/claude` on an existing PVC, executable by UID/GID 1000. Set `claude.existingClaim` and use `executable = "/var/lib/llmproxy/claude/bin/claude"` in that provider's TOML configuration. Set `nodeSelector.kubernetes.io/arch` to the binary architecture (`amd64`, `arm64`, or `arm`). The volume is mounted read-only. Authentication is stored in PostgreSQL, not on this volume. The image does not bundle or download Claude.

Expose the ClusterIP Service through your normal TLS/network boundary. `/health` and `/metrics` share the application listener. Use bearer tokens behind shared proxies; CIDR authentication observes the TCP peer.

Validate rendering with:

```sh
helm lint charts/llmproxy --set database.existingSecret=llmproxy-database
python3 tests/helm_chart.py
```

The render checks require Helm, Python 3.11+, and PyYAML.
