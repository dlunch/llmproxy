# Implementation review

Status: accepted. Final minimality review and local execution checks passed. No unresolved blocking findings.

The final Dockerfile builds the Rust proxy natively and resolves the latest release descriptor through remote ADD and downloads the matching official Claude executable, checking its release-manifest SHA-256 before copying it unchanged into the runtime. The workflow contains only native amd64 and arm64 jobs. Helm uses the bundled executable, configurable rolling replicas and temporary storage. Obsolete executable transformation, startup update/build paths, persistent binary storage, architecture emulation and their supporting tests were removed completely.

Claude owns its PostgreSQL credential transaction through invocation, process reaping and authentication persistence. Codex acquires the row lock, rereads current credentials and persists refresh before committing. Both save through the held connection. Waiting callers remain cancellable, and tracked active workers finish persistence during shutdown. Independent provider instances with separate single-connection pools validate cross-replica behavior.

Model mappings remain filtered by account ownership and registered providers, with database changes taking effect on subsequent requests.

## Persistence impact

Model routing adds one account-scoped query per inference. Claude holds one connection and row lock during each invocation and updates authentication in that transaction. Calls sharing a Claude credential serialize across replicas; different credentials remain independent. Codex holds its row lock only for credential checking or refresh. No request bodies or CLI transcripts are added to PostgreSQL.
