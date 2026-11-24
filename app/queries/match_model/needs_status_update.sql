-- Logged matches whose status in JobFinder changed since they were last synced on Jobnet,
-- newest update first, for the Jobnet extension to offer updating on jobnet.dk/joblog.
-- ?1 the integration the Jobnet joblog is kept under, ?2 how many.
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
JOIN match_integrations j ON j.match_id = m.id AND j.integration = ?1
WHERE m.rejected_at IS NULL
  AND j.logged_at IS NOT NULL
  AND (j.logged_status IS NULL OR j.logged_status <> j.status)
ORDER BY j.updated_at DESC, m.id DESC
LIMIT ?2
