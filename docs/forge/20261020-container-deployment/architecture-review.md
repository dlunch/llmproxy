# Architecture review

Status: accepted with integration checks required before publication.

The native amd64/native arm64/cross-compiled armv7 design meets the no-emulated-compilation requirement. The build stage uses BUILDPLATFORM. QEMU is confined to runtime package installation for arm/v7; Rust and C compilation execute natively on the build runner. Per-run digest artifacts and a dependent merge job prevent mixing platforms from different commits. Pull requests must neither authenticate nor publish.

The single-replica Recreate chart preserves process-local credential ownership. Existing PostgreSQL Secrets, a non-root process, writable temporary storage, and an optional read-only Claude executable volume fit the runtime. Authentication belongs in PostgreSQL and private invocation directories, not the executable volume.

Provider model rows should be scoped by both authenticated owner and currently registered providers before routing. Removing TOML routes avoids conflicting sources of truth. Claude invocations must serialize hydration, refresh and persistence per provider and persist refreshed credentials even on timeout, cancellation and process failure. Persist only authentication/configuration files, not conversations or temporary request content.

Final integration checks: align the chart's image tag with published tags and its UID with the image; remove obsolete TOML model routes and persistent Claude configuration paths; verify DB-backed credentials and models through integration tests. Actual hosted ARM64 compilation and registry publication remain separate from local validation.
