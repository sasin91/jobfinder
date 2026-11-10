-- Use active, explicit rejections as preference signals for future matches.
SELECT DISTINCT
  r.reason,
  CASE r.reason WHEN 'irrelevant' THEN m.job_title ELSE m.company END AS "value!: String"
FROM match_rejections r
JOIN matches m ON m.id = r.match_id
WHERE m.rejected_at IS NOT NULL AND r.reason IN ('irrelevant', 'company')
