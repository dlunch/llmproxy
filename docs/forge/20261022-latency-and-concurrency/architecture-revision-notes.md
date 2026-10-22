# Architecture review revisions

- Accepted migration lock ordering: acquire the request writer lock before ALTERing the account/provider parent tables, preventing parent-lock/request-lock inversion with existing inserts.
- Accepted logging error retention: the flush barrier must report the first prior sink write/flush failure even when its final flush succeeds; the appender worker otherwise discards write errors.
- Accepted quota lock ordering: sort borrowed window references before multirow UPSERT, since concurrent Claude workers originate windows from independently ordered maps.
- Accepted central Claude refresh ownership with official control protocol: local CLI evidence confirms a failed access token can be replaced during inference. Compare the child's rejected token to the locked current database token before forcing a refresh, and track exchange/persistence independently of child cancellation.
