# Static dashboard implementation review

Scope: the complete dashboard change against API commit `4b41f50`, including new/untracked static files, Nginx image, Helm resources, image workflow, Rust CORS/configuration, integration tests and updated documentation. This pass does not execute Cargo or Docker while the coordinator owns browser/server QA.

## Findings

### Medium, resolved: overlapping routing reads can discard an unsaved draft

`dashboard/app.js:276` (`loadRouting`) applies every completed GET, resets `routingDirty`, and re-enables fields without the version guard used by token/provider loaders. `enterAccount` displays the application before its initial `loadAll` completes, and the Refresh button is available. Therefore an initial and manual routing GET can overlap: the first completion enables editing while the second is pending; the later completion then replaces those edits and clears the dirty indicator.

The final loader increments `routingVersion`, checks both version and account epoch before applying a response, and lets only the current loader re-enable the editor. Successful route writes also advance the version so an older GET cannot overwrite a completed save. This resolves the inspected race without backend changes. Browser regression coverage should delay the overlapping responses and verify that an older completion neither overwrites the current editor nor clears its dirty state.

### Medium, resolved: password character minimum rejected API-valid passwords

The initial `dashboard/index.html:31` and `:38` used `minlength="8"`, while the API accepts passwords with 8–1024 UTF-8 bytes. A password containing four Korean characters is valid in the API but failed HTML's character-length constraint before login could submit.

The latest inputs remove that character minimum and leave the authoritative byte validation to the API. The signup hint and translated error now describe the byte constraint. Browser QA should include an existing account with a short multibyte password.

### Medium, resolved: cancelled operations left controls disabled across sessions

The Refresh handler disabled its button and only restored it when its captured account epoch still matched. Logout invalidated the epoch, while the previous account reset restored only form controls. Refresh followed by logout/login could therefore leave Refresh disabled permanently; a failed logout could also strand controls belonging to an aborted form submission.

The latest `invalidateRequests` resets the Refresh button and cancelled form busy/disabled state before clearing account-specific asynchronous work. It does not erase field values on failed logout. Verify delayed refresh followed by logout/login, plus a failed logout with an in-flight form operation.

## Checked implementation boundaries

- `Config::validate` accepts only canonical HTTP(S) origins, rejecting paths, credentials, queries, trailing slash, wildcard and opaque origins. The optional CORS layer uses an exact origin list, credentials, fixed methods and fixed headers. It wraps the complete management router before that router is merged with inference, preserving readable management errors and unauthenticated preflight while leaving inference CORS unchanged.
- Existing management mutation-header, cookie and ownership checks remain in place. The new integration assertions cover allowed/hostile preflight, credential headers, an unauthenticated management error, and inference isolation. Fresh inference routing/credential logic is unchanged by this commit.
- Helm keeps the existing API selector intact and gives dashboard Pods a different name label. Each Service selects only its own Deployment. Runtime frontend config is serialized with `toJson`; API CORS origin is encoded as a TOML string. Separate rendered-config checksums roll only the affected Deployment when either public origin changes.
- Nginx serves static assets directly, returns 404 for missing files, restricts methods, revalidates assets and disables configuration caching. Its config has no API proxy. The image runs unprivileged, uses `/tmp` for pid/temp files, and the dashboard Deployment provides read-only root/config plus writable temporary storage. Root API Docker context/COPY rules do not embed dashboard files.
- The workflow matrix expands to API/dashboard times amd64/arm64. Build contexts, image names, caches, digest artifact names and publication download patterns remain separated by product. Publication is configured for push events; this static inspection does not itself prove a published image or running deployment.
- UI-generated account/provider/token/route values use text nodes or DOM properties. No HTML interpolation, browser token/password storage, `eval`, or token-bearing URL construction was found. API calls include credentials and the management mutation header. One-time tokens clear on dialog/account transitions; OAuth requests and polling are aborted on close/logout, and successful results check the current flow/account epoch.
- Provider edits and route PUT failures preserve form input. Existing provider status is presented as credential presence, not a live entitlement check. Token statistics use the actual API and distinguish admitted-request history. Initial/empty/error states and desktop/mobile layouts are present; final visual and keyboard behavior belongs to browser QA.

## Validation boundary

The coordinator reported the targeted configuration/management/OAuth tests and Helm checks passing. This reviewer performed source/diff review only during this phase; browser, container and final integration results must be recorded by the coordinator after the final UI corrections. The user subsequently authorized image publication and cluster installation, which are later operational checks and not claimed by this implementation review.

The new `tests/dashboard.cjs` uses the real local API across a distinct origin for signup/session/token/provider/routing/account operations. It includes a short multibyte password, secret clearing, literal rendering of an HTML-shaped token label, cookie/storage assertions, durable route-error rollback, reload, mobile overflow, token revocation, and delayed refresh followed by logout/login. Only provider OAuth UI responses are intercepted; existing backend OAuth tests cover their actual server implementation. The material remaining regressions to add are overlapping routing reads and a failed logout while a draft/form operation is active. No broader duplicate backend suite is needed for this UI pass.

Review status: **PASS** after verifying all three fixes in the current source. No remaining concrete implementation blocker was found. Final delivery remains subject to the planned browser/container QA, including the resolved regressions, and the separately authorized publishing/deployment checks.
