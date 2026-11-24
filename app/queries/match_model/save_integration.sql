-- Saves a match's status in an outside service. ?1 the match, ?2 the integration, ?3 the status,
-- ?4 when it was applied for (NULL unless ?3 is an applied status), ?5 when it was logged there
-- (NULL unless it is), ?6 now, ?7 whether ?3 is an applied status, ?8 whether it is logged,
-- ?9 whether ?3 was synced to the outside service now, ?10 the synced status (NULL unless ?9),
-- ?11 the outside service's entry id when known (kept when NULL).
-- The first applied status and the first time logged keep their dates; any other status clears
-- applied_at. Unlogging clears logged_at and logged_status; changing status without syncing keeps
-- the previous logged_status so the extension knows Jobnet needs an update.
INSERT INTO match_integrations
  (match_id, integration, status, external_id, applied_at, logged_at, logged_status, created_at, updated_at)
VALUES (?1, ?2, ?3, ?11, ?4, ?5, CASE WHEN ?8 AND ?9 THEN ?10 ELSE NULL END, ?6, ?6)
ON CONFLICT (match_id, integration) DO UPDATE SET
  status = excluded.status,
  external_id = COALESCE(excluded.external_id, external_id),
  updated_at = excluded.updated_at,
  applied_at = CASE WHEN ?7 THEN COALESCE(applied_at, excluded.applied_at) END,
  logged_at = CASE WHEN ?8 THEN COALESCE(logged_at, excluded.logged_at) END,
  logged_status = CASE
    WHEN NOT ?8 THEN NULL
    WHEN ?9 THEN ?10
    ELSE logged_status
  END
