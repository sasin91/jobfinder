-- Let the application form use a separate model on the application key.
CREATE TABLE llm_task_models_with_application_form (
  task TEXT PRIMARY KEY CHECK (task IN ('read_job', 'read_cv', 'judge', 'application', 'application_form', 'resume')),
  model TEXT NOT NULL DEFAULT ''
);
INSERT INTO llm_task_models_with_application_form (task, model)
SELECT task, model FROM llm_task_models;
DROP TABLE llm_task_models;
ALTER TABLE llm_task_models_with_application_form RENAME TO llm_task_models;
