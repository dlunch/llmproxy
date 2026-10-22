-- Take the request lock first: existing writers may still need the parent rows.
LOCK TABLE requests IN SHARE ROW EXCLUSIVE MODE;

ALTER TABLE accounts
    ADD COLUMN input_tokens bigint NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    ADD COLUMN output_tokens bigint NOT NULL DEFAULT 0 CHECK (output_tokens >= 0);
ALTER TABLE provider_instances
    ADD COLUMN input_tokens bigint NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    ADD COLUMN output_tokens bigint NOT NULL DEFAULT 0 CHECK (output_tokens >= 0);

UPDATE accounts a SET input_tokens = totals.input_tokens, output_tokens = totals.output_tokens
FROM (
    SELECT account_id, COALESCE(sum(input_tokens), 0)::bigint AS input_tokens,
           COALESCE(sum(output_tokens), 0)::bigint AS output_tokens
    FROM requests GROUP BY account_id
) totals WHERE a.id = totals.account_id;
UPDATE provider_instances p SET input_tokens = totals.input_tokens, output_tokens = totals.output_tokens
FROM (
    SELECT provider_id, COALESCE(sum(input_tokens), 0)::bigint AS input_tokens,
           COALESCE(sum(output_tokens), 0)::bigint AS output_tokens
    FROM requests GROUP BY provider_id
) totals WHERE p.id = totals.provider_id;

CREATE FUNCTION update_usage_totals() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    input_delta bigint;
    output_delta bigint;
    owner_id uuid;
    provider text;
BEGIN
    IF TG_OP = 'DELETE' THEN
        input_delta := -COALESCE(OLD.input_tokens, 0);
        output_delta := -COALESCE(OLD.output_tokens, 0);
        owner_id := OLD.account_id;
        provider := OLD.provider_id;
    ELSE
        input_delta := COALESCE(NEW.input_tokens, 0);
        output_delta := COALESCE(NEW.output_tokens, 0);
        owner_id := NEW.account_id;
        provider := NEW.provider_id;
        IF TG_OP = 'UPDATE' THEN
            input_delta := input_delta - COALESCE(OLD.input_tokens, 0);
            output_delta := output_delta - COALESCE(OLD.output_tokens, 0);
        END IF;
    END IF;
    IF input_delta <> 0 OR output_delta <> 0 THEN
        UPDATE accounts SET input_tokens = input_tokens + input_delta,
                            output_tokens = output_tokens + output_delta
        WHERE id = owner_id;
        UPDATE provider_instances SET input_tokens = input_tokens + input_delta,
                                      output_tokens = output_tokens + output_delta
        WHERE id = provider;
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER requests_usage_totals
AFTER INSERT OR UPDATE OF input_tokens, output_tokens OR DELETE ON requests
FOR EACH ROW EXECUTE FUNCTION update_usage_totals();
