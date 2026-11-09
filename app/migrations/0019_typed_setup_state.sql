-- Replace the PR's document store without losing choices or model measurements.
CREATE TABLE onboarding (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  completed INTEGER NOT NULL DEFAULT 0 CHECK (completed IN (0, 1)),
  manual_scan INTEGER NOT NULL DEFAULT 0 CHECK (manual_scan IN (0, 1)),
  skipped_scan INTEGER NOT NULL DEFAULT 0 CHECK (skipped_scan IN (0, 1)),
  current_step INTEGER NOT NULL DEFAULT 0 CHECK (current_step BETWEEN 0 AND 5)
);
INSERT INTO onboarding (id, completed, manual_scan, skipped_scan, current_step)
SELECT 1,
  COALESCE(json_extract(value, '$.completed'), 0),
  COALESCE(json_extract(value, '$.manual_scan'), 0),
  COALESCE(json_extract(value, '$.skipped_scan'), 0),
  COALESCE(json_extract(value, '$.current_step'), 0)
FROM app_state WHERE name = 'onboarding';

CREATE TABLE model_assessments (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  evaluation_version INTEGER NOT NULL CHECK (evaluation_version > 0),
  assessed_at INTEGER NOT NULL,
  memory_bytes INTEGER NOT NULL CHECK (memory_bytes >= 0),
  gpu_name TEXT,
  gpu_vendor TEXT CHECK (gpu_vendor IN ('nvidia', 'amd', 'intel', 'other')),
  gpu_vram_bytes INTEGER CHECK (gpu_vram_bytes >= 0),
  runtime_release TEXT NOT NULL,
  CHECK ((gpu_name IS NULL AND gpu_vendor IS NULL AND gpu_vram_bytes IS NULL)
      OR (gpu_name IS NOT NULL AND gpu_vendor IS NOT NULL AND gpu_vram_bytes IS NOT NULL))
);
INSERT INTO model_assessments
  (id, evaluation_version, assessed_at, memory_bytes, gpu_name, gpu_vendor, gpu_vram_bytes, runtime_release)
SELECT 1, json_extract(value, '$.version'), json_extract(value, '$.at'),
  json_extract(value, '$.inventory.memory_bytes'),
  json_extract(value, '$.inventory.gpu.name'),
  lower(json_extract(value, '$.inventory.gpu.vendor')),
  json_extract(value, '$.inventory.gpu.vram_bytes'),
  COALESCE(json_extract(value, '$.runtime_release'), '')
FROM app_state WHERE name = 'model-assessment';

CREATE TABLE model_checks (
  assessment_id INTEGER NOT NULL REFERENCES model_assessments (id) ON DELETE CASCADE,
  task TEXT NOT NULL CHECK (task IN ('read_job', 'read_cv', 'judge', 'application', 'application_form', 'resume')),
  model TEXT NOT NULL,
  checked_at INTEGER NOT NULL,
  elapsed_ms INTEGER NOT NULL CHECK (elapsed_ms >= 0),
  passed INTEGER NOT NULL CHECK (passed IN (0, 1)),
  detail TEXT NOT NULL,
  PRIMARY KEY (assessment_id, task)
);
INSERT INTO model_checks (assessment_id, task, model, checked_at, elapsed_ms, passed, detail)
SELECT 1, json_extract(check_result.value, '$.task'), json_extract(check_result.value, '$.model'),
  json_extract(check_result.value, '$.at'), json_extract(check_result.value, '$.milliseconds'),
  json_extract(check_result.value, '$.passed'), json_extract(check_result.value, '$.detail')
FROM app_state, json_each(app_state.value, '$.checks') AS check_result
WHERE app_state.name = 'model-assessment';

DROP TABLE app_state;
