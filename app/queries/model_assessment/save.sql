INSERT INTO model_assessments
  (id, evaluation_version, assessed_at, memory_bytes, gpu_name, gpu_vendor, gpu_vram_bytes, runtime_release)
VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
ON CONFLICT (id) DO UPDATE SET
  evaluation_version = excluded.evaluation_version,
  assessed_at = excluded.assessed_at,
  memory_bytes = excluded.memory_bytes,
  gpu_name = excluded.gpu_name,
  gpu_vendor = excluded.gpu_vendor,
  gpu_vram_bytes = excluded.gpu_vram_bytes,
  runtime_release = excluded.runtime_release
