# Claude response quota collection

Collect the five-hour and seven-day usage-window observations returned by the official Claude CLI during inference. Store them in the existing provider quota state so routing and provider-state consumers can use the information already available from the response.

- Consume explicit `rate_limit_event.rate_limit_info.unifiedWindows` observations only; missing or malformed windows remain unknown.
- Preserve existing token usage, streaming, OAuth rotation, cancellation and function-handoff behavior.
- Record each returned window independently using its observed time, utilization and reset time. Values above 100% mean no remaining base-window capacity.
- Persist credentials before quota, with no second pool connection while the credential transaction is held. Quota persistence failure must not roll back rotated credentials.
- No quota polling, new schema, endpoint, model request or overage-entitlement policy is added. Existing five-minute freshness and reset handling remain unchanged.

The default router remains conservative about base windows: an exhausted recorded five-hour or seven-day window excludes the provider while fresh, even if Claude permits extra paid usage.
