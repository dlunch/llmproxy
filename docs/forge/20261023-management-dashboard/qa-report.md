# Static dashboard QA report

## Result

**PASS for code and local validation.** No unresolved Critical, High, Medium or Low finding remains. All three implementation-review findings are fixed and have browser regression coverage. Image publication and the authorized cluster deployment are subsequent operational gates; this report does not claim they have completed.

The coordinator ran the final augmented browser suite successfully against the real local API and PostgreSQL from a separate static Nginx origin. Targeted Rust configuration/management/OAuth tests passed **5/5**. The coordinator also reported all-target Clippy, formatting, Helm lint, actionlint, image build and static-container checks passing. The reviewer independently reran JavaScript syntax checks, Helm rendering assertions and `git diff --check`; all passed.

## Coverage and evidence

| Requirement | Evidence | Result |
| --- | --- | --- |
| Separate static image and direct API access | Reviewed Nginx/Docker/Helm implementation; coordinator built and ran the static image with read-only root and writable temporary storage; browser suite asserts distinct origins | Pass |
| Exact credentialed management CORS, readable errors and inference isolation | Rust management tests cover allowed/hostile preflight, credential headers, unauthenticated errors and inference isolation; real browser signup/login uses the configured API origin | Pass |
| Account/session and API-token UI | Real signup/login, short multibyte password, account rename, reload/logout, two issued tokens, one-time display/clearing, revocation and statistics display | Pass |
| Safe content and secret handling | HTML-shaped token label remains literal text; no injected image/script; cookie HttpOnly/SameSite/Path checked; password/code/token fields clear; browser storage stays empty | Pass |
| Provider registration/configuration and OAuth UI | Real provider create/rename/enable/disable calls; controlled Codex pending/completed polling and Claude code submission fixtures; existing backend OAuth tests cover persistence/protocol behavior | Pass |
| Atomic routing and retained invalid input | Real route PUT, duplicate rejection, retained draft and unchanged persisted routes; valid correction saves successfully | Pass |
| Overlapping routing reads | A held bootstrap GET completes after a newer refresh and subsequent edit; the draft remains unchanged after the stale response finishes | Pass |
| Failed logout with an active form | Test holds a submitted account PATCH, confirms disabled input, fails logout with 503, checks retained/enabled draft, releases the aborted request and successfully retries the real save | Pass |
| Refresh/logout/account transition | Delayed token refresh is aborted by logout; subsequent login retains an enabled Refresh control; account/token DOM and secrets clear on final logout | Pass |
| Responsive presentation | Browser overflow assertion at 390 px; reviewer independently inspected captured login, desktop overview and mobile screenshots with no overlap or clipping found | Pass |
| Deployment and image separation | Helm assertions cover required origins, valid JSON/TOML, disjoint selectors/images/services, configuration-triggered rollouts and container settings; workflow review/actionlint cover product/architecture builds and separate artifacts | Pass |

## Test quality

`tests/dashboard.cjs` exercises real browser-to-API operations and checks both UI behavior and persisted HTTP results. External subscription OAuth is mocked intentionally; controlled interception also creates the stale-read, aborted-request and logout-failure conditions. The final failed-logout case now submits a pending form before failure, so it detects the original disabled-control regression rather than checking only an idle draft. Assertions are behavioral, and the race tests use explicit gates. No vacuous assertions or implementation-mirroring replacement of the application was found.

Independent checks executed in this pass:

```sh
node --check dashboard/app.js
node --check tests/dashboard.cjs
python3 tests/helm_chart.py
git diff --check
```

Cargo and Docker checks were not redundantly rerun by this reviewer while the coordinator owned the test server. Their successful results above are coordinator evidence. The API phase's separate full-suite result remains recorded in its QA report; this dashboard phase does not claim a new full Rust-suite run.

## Remaining operational validation

The user has authorized push, image publication and deployment at `https://api-llmproxy.dlun.ch` and `https://llmproxy.dlun.ch`. After CI publishes the intended images, verify the actual cluster rollout, both Service/Gateway routes, public HTTPS, exact management CORS and inference authentication. Those results must be recorded separately from this local PASS.

No real subscription OAuth, live provider entitlement, or live upstream inference was exercised. Browser cookie evidence uses local cross-origin development settings; production HTTPS/Secure-cookie behavior belongs to deployment validation. No load/performance claim is made for the dashboard, and this change leaves inference routing and provider execution unchanged.
