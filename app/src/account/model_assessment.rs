//! Typed assessment storage. Header and task measurements are replaced atomically;
//! readers see a consistent snapshot when another check or reassessment is saved.
use sqlx::SqlitePool;

use super::model_plan::{Assessment, Check};
use crate::llama::hardware::{Gpu, GpuVendor, Inventory};

pub async fn read(db: &SqlitePool) -> sqlx::Result<Option<Assessment>> {
    let mut tx = db.begin().await?;
    let Some(row) = sqlx::query_file!("queries/model_assessment/read.sql")
        .fetch_optional(&mut *tx)
        .await?
    else {
        tx.commit().await?;
        return Ok(None);
    };
    let results = sqlx::query_file!("queries/model_assessment/checks.sql")
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    let gpu = match (row.gpu_name, row.gpu_vendor, row.gpu_vram_bytes) {
        (Some(name), Some(vendor), Some(vram)) => Some(Gpu {
            name,
            vendor: match vendor.as_str() {
                "nvidia" => GpuVendor::Nvidia,
                "amd" => GpuVendor::Amd,
                "intel" => GpuVendor::Intel,
                "other" => GpuVendor::Other,
                _ => {
                    return Err(sqlx::Error::Decode(Box::new(std::io::Error::other(
                        "Unknown GPU vendor",
                    ))));
                }
            },
            vram_bytes: u64::try_from(vram).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        }),
        (None, None, None) => None,
        _ => {
            return Err(sqlx::Error::Decode(Box::new(std::io::Error::other(
                "Incomplete GPU inventory",
            ))));
        }
    };
    let checks = results
        .into_iter()
        .map(|row| {
            Ok(Check {
                task: row.task,
                model: row.model,
                at: row.checked_at,
                milliseconds: u64::try_from(row.elapsed_ms).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
                passed: row.passed,
                detail: row.detail,
            })
        })
        .collect::<sqlx::Result<Vec<_>>>()?;
    Ok(Some(Assessment {
        version: u32::try_from(row.evaluation_version).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        at: row.assessed_at,
        inventory: Inventory {
            memory_bytes: u64::try_from(row.memory_bytes).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            gpu,
        },
        runtime_release: row.runtime_release,
        checks,
    }))
}

pub async fn save(db: &SqlitePool, assessment: &Assessment) -> sqlx::Result<()> {
    let version = i64::from(assessment.version);
    let memory = i64::try_from(assessment.inventory.memory_bytes).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    let gpu = assessment.inventory.gpu.as_ref();
    let name = gpu.map(|g| g.name.as_str());
    let vendor = gpu.map(|g| match g.vendor {
        GpuVendor::Nvidia => "nvidia",
        GpuVendor::Amd => "amd",
        GpuVendor::Intel => "intel",
        GpuVendor::Other => "other",
    });
    let vram = gpu
        .map(|g| i64::try_from(g.vram_bytes))
        .transpose()
        .map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    let mut tx = crate::db::write_transaction(db).await?;
    sqlx::query_file!(
        "queries/model_assessment/save.sql",
        version,
        assessment.at,
        memory,
        name,
        vendor,
        vram,
        assessment.runtime_release
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM model_checks WHERE assessment_id = 1")
        .execute(&mut *tx)
        .await?;
    for check in &assessment.checks {
        let elapsed = i64::try_from(check.milliseconds).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
        sqlx::query_file!(
            "queries/model_assessment/insert_check.sql",
            check.task,
            check.model,
            check.at,
            elapsed,
            check.passed,
            check.detail
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::model_plan::EVALUATION_VERSION;
    use sqlx::Executor;

    fn sample(gpu: Option<Gpu>) -> Assessment {
        Assessment {
            version: EVALUATION_VERSION,
            at: 100,
            inventory: Inventory {
                memory_bytes: 16 * 1024 * 1024 * 1024,
                gpu,
            },
            runtime_release: "test-runtime".into(),
            checks: vec![Check {
                task: "application_form".into(),
                model: "candidate.gguf".into(),
                at: 101,
                milliseconds: 2_000,
                passed: false,
                detail: "Needs attention".into(),
            }],
        }
    }

    #[tokio::test]
    async fn typed_rows_round_trip_cpu_and_each_gpu_vendor_and_remove_old_checks() {
        let db = crate::db::open_in_memory().await.unwrap();
        assert!(read(&db).await.unwrap().is_none());
        let cpu = sample(None);
        save(&db, &cpu).await.unwrap();
        assert_eq!(read(&db).await.unwrap().unwrap(), cpu);
        for vendor in [GpuVendor::Nvidia, GpuVendor::Amd, GpuVendor::Intel, GpuVendor::Other] {
            let value = sample(Some(Gpu {
                name: "Graphics card".into(),
                vendor,
                vram_bytes: 0,
            }));
            save(&db, &value).await.unwrap();
            assert_eq!(read(&db).await.unwrap().unwrap(), value, "unknown VRAM remains zero");
        }
        let mut reassessed = cpu;
        reassessed.checks.clear();
        reassessed.runtime_release = "new-runtime".into();
        save(&db, &reassessed).await.unwrap();
        assert_eq!(read(&db).await.unwrap().unwrap(), reassessed);
    }

    #[tokio::test]
    async fn failed_check_insert_rolls_back_header_and_previous_checks() {
        let db = crate::db::open_in_memory().await.unwrap();
        let original = sample(None);
        save(&db, &original).await.unwrap();
        let mut invalid = original.clone();
        invalid.runtime_release = "must not be saved".into();
        invalid.checks[0].task = "unknown-task".into();
        assert!(save(&db, &invalid).await.is_err());
        assert_eq!(read(&db).await.unwrap().unwrap(), original);
        assert!(db.execute("INSERT INTO model_checks (assessment_id, task, model, checked_at, elapsed_ms, passed, detail) VALUES (2, 'judge', 'm', 1, 0, 1, '')").await.is_err(), "checks require their assessment");
    }

    #[tokio::test]
    async fn json_migration_preserves_progress_inventory_and_measurements() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        db.execute("CREATE TABLE app_state (name TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .await
            .unwrap();
        let old_progress =
            serde_json::json!({"version":1,"completed":false,"manual_scan":true,"skipped_scan":true,"current_step":4})
                .to_string();
        sqlx::query("INSERT INTO app_state VALUES ('onboarding', ?1)")
            .bind(old_progress)
            .execute(&db)
            .await
            .unwrap();
        let expected = sample(Some(Gpu {
            name: "GPU".into(),
            vendor: GpuVendor::Nvidia,
            vram_bytes: 8 * 1024 * 1024 * 1024,
        }));
        let old_assessment = serde_json::json!({
            "version":expected.version,"at":expected.at,"inventory":expected.inventory,
            "runtime_release":expected.runtime_release,"checks":[{
                "task":"application_form","model":"candidate.gguf","at":101,"milliseconds":2000,"passed":false,"detail":"Needs attention"
            }]
        }).to_string();
        sqlx::query("INSERT INTO app_state VALUES ('model-assessment', ?1)")
            .bind(old_assessment)
            .execute(&db)
            .await
            .unwrap();
        db.execute(include_str!("../../migrations/0019_typed_setup_state.sql"))
            .await
            .unwrap();
        assert_eq!(
            crate::setup::progress::read(&db).await.unwrap(),
            crate::setup::progress::Progress {
                completed: false,
                manual_scan: true,
                skipped_scan: true,
                current_step: 4,
            }
        );
        assert_eq!(read(&db).await.unwrap().unwrap(), expected);
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name = 'app_state'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(remaining, 0);
    }
}
