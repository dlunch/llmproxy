CREATE TABLE accounts (
    id uuid PRIMARY KEY,
    label text NOT NULL,
    enabled boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE provider_instances (
    id text PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    UNIQUE (account_id, id)
);
CREATE TABLE provider_models (
    provider_id text NOT NULL REFERENCES provider_instances(id),
    model text NOT NULL CHECK (model <> ''),
    upstream_model text NOT NULL CHECK (upstream_model <> ''),
    PRIMARY KEY (provider_id, model)
);
CREATE TABLE account_ip_ranges (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    network cidr NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE api_tokens (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    label text NOT NULL,
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz
);
CREATE TABLE provider_credentials (
    provider_id text PRIMARY KEY REFERENCES provider_instances(id),
    access_token text NOT NULL,
    refresh_token text NOT NULL,
    account_id text NOT NULL,
    expires_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE claude_credentials (
    provider_id text PRIMARY KEY REFERENCES provider_instances(id),
    credentials jsonb NOT NULL CHECK (jsonb_typeof(credentials) = 'object'),
    oauth_account jsonb CHECK (jsonb_typeof(oauth_account) = 'object'),
    user_id text,
    has_completed_onboarding boolean NOT NULL DEFAULT true,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE requests (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    api_token_id uuid REFERENCES api_tokens(id),
    ip_range_id uuid REFERENCES account_ip_ranges(id),
    client_ip inet,
    CHECK ((api_token_id IS NOT NULL AND ip_range_id IS NULL AND client_ip IS NULL)
        OR (api_token_id IS NULL AND ip_range_id IS NOT NULL AND client_ip IS NOT NULL)),
    requested_model text NOT NULL,
    provider_id text NOT NULL,
    FOREIGN KEY (account_id, provider_id) REFERENCES provider_instances(account_id, id),
    upstream_model text NOT NULL,
    started_at timestamptz NOT NULL,
    deadline_at timestamptz NOT NULL,
    finished_at timestamptz,
    outcome text NOT NULL CHECK (outcome IN ('running','succeeded','failed','cancelled','timed_out','interrupted')),
    error_kind text,
    input_tokens bigint CHECK (input_tokens >= 0),
    output_tokens bigint CHECK (output_tokens >= 0)
);
CREATE INDEX requests_account ON requests(account_id);
CREATE INDEX requests_provider ON requests(provider_id);
CREATE INDEX requests_running_deadline ON requests(deadline_at) WHERE outcome = 'running';
CREATE TABLE provider_quota (
    provider_id text NOT NULL REFERENCES provider_instances(id),
    window_key text NOT NULL,
    observed_at timestamptz NOT NULL,
    remaining_fraction double precision NOT NULL CHECK (remaining_fraction >= 0 AND remaining_fraction <= 1),
    resets_at timestamptz,
    PRIMARY KEY(provider_id, window_key)
);
CREATE VIEW provider_statistics AS
SELECT p.account_id, p.id AS provider_id,
       count(r.id) AS request_count,
       count(r.id) FILTER (WHERE r.outcome = 'running') AS running_requests,
       count(r.id) FILTER (WHERE r.outcome = 'succeeded') AS succeeded_requests,
       count(r.id) FILTER (WHERE r.outcome = 'failed') AS failed_requests,
       count(r.id) FILTER (WHERE r.outcome = 'cancelled') AS cancelled_requests,
       count(r.id) FILTER (WHERE r.outcome = 'timed_out') AS timed_out_requests,
       count(r.id) FILTER (WHERE r.outcome = 'interrupted') AS interrupted_requests,
       sum(r.input_tokens) AS input_tokens,
       sum(r.output_tokens) AS output_tokens,
       count(r.id) FILTER (WHERE r.input_tokens IS NULL OR r.output_tokens IS NULL) AS missing_usage_requests,
       avg(extract(epoch FROM (r.finished_at - r.started_at)) * 1000)::double precision AS mean_latency_ms,
       max(r.started_at) AS last_request_at
FROM provider_instances p
LEFT JOIN requests r ON r.provider_id = p.id AND r.account_id = p.account_id
GROUP BY p.account_id, p.id;
