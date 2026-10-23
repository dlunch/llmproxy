ALTER TABLE accounts
    ADD COLUMN email text UNIQUE,
    ADD COLUMN password_hash text,
    ADD COLUMN routing_strategy text NOT NULL DEFAULT 'least_used'
        CHECK (routing_strategy IN ('least_used', 'priority')),
    ADD CONSTRAINT account_login_fields CHECK ((email IS NULL) = (password_hash IS NULL));

CREATE TABLE signup_codes (
    code_hash bytea PRIMARY KEY CHECK (octet_length(code_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE management_sessions (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX management_sessions_expiry ON management_sessions(expires_at);

ALTER TABLE provider_instances
    ADD COLUMN kind text CHECK (kind IN ('codex', 'claude')),
    ADD COLUMN label text,
    ADD COLUMN enabled boolean NOT NULL DEFAULT true,
    ADD COLUMN login_generation bigint NOT NULL DEFAULT 0 CHECK (login_generation >= 0);

UPDATE provider_instances p
SET label = p.id,
    kind = CASE
        WHEN EXISTS (SELECT 1 FROM provider_credentials c WHERE c.provider_id = p.id)
             AND NOT EXISTS (SELECT 1 FROM claude_credentials c WHERE c.provider_id = p.id)
            THEN 'codex'
        WHEN EXISTS (SELECT 1 FROM claude_credentials c WHERE c.provider_id = p.id)
             AND NOT EXISTS (SELECT 1 FROM provider_credentials c WHERE c.provider_id = p.id)
            THEN 'claude'
    END;
ALTER TABLE provider_instances ALTER COLUMN label SET NOT NULL;

ALTER TABLE provider_models ADD COLUMN priority integer NOT NULL DEFAULT 0;
ALTER TABLE provider_quota
    ADD COLUMN login_generation bigint NOT NULL DEFAULT 0 CHECK (login_generation >= 0);

CREATE TABLE provider_login_sessions (
    id uuid PRIMARY KEY,
    provider_id text NOT NULL UNIQUE REFERENCES provider_instances(id),
    status text NOT NULL CHECK (status IN ('pending', 'processing', 'completed', 'failed')),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    device_auth_id text,
    user_code text,
    interval_seconds bigint,
    next_poll_at timestamptz,
    state text,
    code_verifier text
);
CREATE INDEX provider_login_sessions_expiry ON provider_login_sessions(expires_at);
CREATE INDEX requests_token_started ON requests(api_token_id, started_at);
