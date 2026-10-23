WITH issued AS MATERIALIZED (
    SELECT replace(gen_random_uuid()::text || gen_random_uuid()::text, '-', '') AS code
), saved AS (
    INSERT INTO signup_codes(code_hash)
    SELECT sha256(convert_to(code, 'UTF8')) FROM issued
    RETURNING code_hash
)
SELECT issued.code
FROM issued JOIN saved ON saved.code_hash = sha256(convert_to(issued.code, 'UTF8'));
