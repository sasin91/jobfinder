-- A new saved match: the post and its score; the CV it was matched with is linked to it after
-- (match_documents). ?1 job_title, ?2 company, ?3 job_url, ?4 job_text, ?5 score,
-- ?6 points, ?7 max_points, ?8 tag, ?9 created_at,
-- ?10 the application deadline read from the post (matches::still_open::deadline_of).
-- ?11 explains any ranking adjustment based on prior rejections.
INSERT INTO matches
  (job_title, company, job_url, job_text, score, points, max_points, tag,
   created_at, deadline, feedback_note)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
