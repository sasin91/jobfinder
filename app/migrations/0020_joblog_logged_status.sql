-- Tracks which status was last synced to Jobnet so status changes in JobFinder
-- (e.g. Søgt -> Afslag or Søgt -> Samtale) can be surfaced by the Jobnet extension
-- to update existing Jobnet joblog entries.
ALTER TABLE match_integrations ADD COLUMN logged_status TEXT DEFAULT NULL;

UPDATE match_integrations
SET logged_status = status
WHERE logged_at IS NOT NULL AND logged_status IS NULL;
