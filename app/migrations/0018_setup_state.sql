CREATE TABLE onboarding (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  completed INTEGER NOT NULL DEFAULT 0 CHECK (completed IN (0, 1)),
  manual_scan INTEGER NOT NULL DEFAULT 0 CHECK (manual_scan IN (0, 1)),
  skipped_scan INTEGER NOT NULL DEFAULT 0 CHECK (skipped_scan IN (0, 1)),
  current_step INTEGER NOT NULL DEFAULT 0 CHECK (current_step BETWEEN 0 AND 5)
);
-- Populated databases can revisit setup without being forced through it.
INSERT INTO onboarding (id, completed)
SELECT 1, EXISTS (SELECT 1 FROM profile) OR EXISTS (SELECT 1 FROM matches);

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
