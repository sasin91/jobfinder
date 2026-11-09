SELECT evaluation_version, assessed_at, memory_bytes, gpu_name, gpu_vendor,
       gpu_vram_bytes, runtime_release
FROM model_assessments WHERE id = 1
