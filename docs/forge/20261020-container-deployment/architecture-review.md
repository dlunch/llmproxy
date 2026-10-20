# Architecture review

Status: accepted; local bundled-image validation completed.

Two native build runners produce amd64 and arm64 images. Each image includes the matching, unmodified official Claude executable resolved from the latest release at build time and verified using the official checksum manifest. A remote ADD of the release descriptor makes the download layer depend on release content, avoiding a permanently cached latest lookup inside RUN. The [BuildKit HTTP source](https://github.com/moby/buildkit/blob/master/source/http/source.go) revalidates cached HTTP metadata and derives the cache key from the resolved content digest. This eliminates runtime installation and its persistent storage, coordination and build dependencies.

Multiple replicas coordinate shared credentials through PostgreSQL. Claude holds a credential row lock through hydration, invocation, process termination and persistence. Codex acquires its row lock and rereads stored credentials before deciding whether refresh remains necessary. Reads and writes use the held transaction connection; separate single-connection-pool fixtures verify serialization without pool deadlock. Cancellation and receiver drop preserve credential rotation before releasing the lock.

Model queries remain filtered by both authenticated account and registered providers. Authentication stays in PostgreSQL and private invocation directories. The Helm chart uses configurable rolling replicas, a Secret for PostgreSQL, and temporary storage for subprocess isolation.

Final review confirmed complete removal of obsolete executable builders, startup updaters, persistent binary storage, cross-architecture compilation and their tests. Local image and official-CLI fixtures are distinguished from hosted ARM64, registry publication, live-cluster rollout and live subscription validation.
