SELECT task, model, checked_at, elapsed_ms, passed AS "passed!: bool", detail
FROM model_checks WHERE assessment_id = 1 ORDER BY task
