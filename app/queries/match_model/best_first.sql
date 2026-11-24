-- A batch of the user's saved matches in one list (LISTS), ordered by score or date saved.
-- ?1 the integration the Jobnet joblog is kept under, ?2 the list,
-- ?3 the applied statuses as a JSON array (APPLIED_STATUSES),
-- ?4 ?5 ?6 the score, created_at and id of the row the batch comes after (all NULL for the first),
-- ?7 how many, ?8 the sort order (0 best, 1 newest, 2 oldest), ?9 the search text.
-- Selects what recent.sql selects, so both read into ListedRow.
SELECT
  m.id,
  m.job_title,
  m.company,
  m.job_url,
  COALESCE((
    SELECT d.name
    FROM match_documents md
    JOIN documents d ON d.id = md.document_id AND d.kind = 'cv'
    WHERE md.match_id = m.id
  ), '') AS "cv_name!: String",
  m.score,
  m.points,
  m.max_points,
  m.tag,
  EXISTS (
    SELECT 1 FROM match_documents md JOIN documents d ON d.id = md.document_id
    WHERE md.match_id = m.id AND d.kind = 'application'
  ) AS "has_application: bool",
  EXISTS (
    SELECT 1 FROM match_documents md JOIN documents d ON d.id = md.document_id
    WHERE md.match_id = m.id AND d.kind = 'resume'
  ) AS "has_resume: bool",
  m.created_at,
  (
    SELECT i.text
    FROM match_items i
    WHERE i.match_id = m.id AND i.criterion IN ('location', 'language', 'level') AND i.verdict = 'missing'
    ORDER BY i.sort_order
    LIMIT 1
  ) AS missed_disqualifier,
  j.status AS "jobnet_status?",
  j.logged_at AS jobnet_logged_at,
  j.logged_status AS "jobnet_logged_status?",
  j.external_id AS "jobnet_external_id?",
  m.rejected_at,
  m.deadline,
  m.closed_at,
  m.closed_why
FROM matches m
LEFT JOIN match_integrations j ON j.match_id = m.id AND j.integration = ?1
WHERE CASE ?2
    -- The top and good ones not applied for yet, while they are open.
    WHEN 'shortlist' THEN
      m.rejected_at IS NULL
      AND m.closed_at IS NULL
      AND m.tag IN ('top', 'good')
      AND (j.status IS NULL OR j.status NOT IN (SELECT value FROM json_each(?3)))
      AND j.logged_at IS NULL
    -- Applied for, but not yet entered in Jobnet's joblog.
    WHEN 'not_joblogged' THEN
      m.rejected_at IS NULL
      AND j.status IN (SELECT value FROM json_each(?3))
      AND j.logged_at IS NULL
    -- Entered in Jobnet's joblog.
    WHEN 'joblogged' THEN
      m.rejected_at IS NULL
      AND j.logged_at IS NOT NULL
    -- The jobs that no longer take applications (matches::still_open).
    WHEN 'closed' THEN
      m.rejected_at IS NULL
      AND m.closed_at IS NOT NULL
    -- The ones the user rejected.
    WHEN 'rejected' THEN
      m.rejected_at IS NOT NULL
    -- Every other list (All) leaves the rejected and the closed ones out.
    ELSE
      m.rejected_at IS NULL
      AND m.closed_at IS NULL
  END
  AND (?9 = '' OR instr(lower(m.job_title), lower(?9)) > 0 OR instr(lower(m.company), lower(?9)) > 0)
  AND (?6 IS NULL OR CASE ?8
    WHEN 1 THEN (m.created_at, m.id) < (?5, ?6)
    WHEN 2 THEN (m.created_at, m.id) > (?5, ?6)
    ELSE (m.score, m.created_at, m.id) < (?4, ?5, ?6)
  END)
ORDER BY
  CASE WHEN ?8 = 0 THEN m.score END DESC,
  CASE WHEN ?8 = 2 THEN m.created_at END ASC,
  CASE WHEN ?8 = 2 THEN m.id END ASC,
  m.created_at DESC,
  m.id DESC
LIMIT ?7
