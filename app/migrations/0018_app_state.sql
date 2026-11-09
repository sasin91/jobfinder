-- Small versioned documents shared by setup and model assessment, per database.
CREATE TABLE app_state (
  name TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
-- Existing users can revisit Setup without being forced through it. A new database
-- remains unfinished across CV import, search creation and application restarts.
INSERT INTO app_state (name, value)
SELECT 'onboarding', CASE
  WHEN EXISTS (SELECT 1 FROM profile) OR EXISTS (SELECT 1 FROM matches)
  THEN '{"version":1,"completed":true,"manual_scan":false,"skipped_scan":false}'
  ELSE '{"version":1,"completed":false,"manual_scan":false,"skipped_scan":false}'
END;
