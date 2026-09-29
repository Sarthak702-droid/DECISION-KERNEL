use kernel_compiler::{DecisionPack, compile};
use kernel_evaluation::{EvaluationDataset, evaluate, meets_criteria};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{env, fs};

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("kernelctl: {e}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        return Err(
            "usage: kernelctl <migrate|validate|compile|publish|inspect|inspect-active|promote|rollback|revoke-release|reconcile-attempt> ..."
                .into(),
        );
    }
    if args[1] == "migrate" {
        let pool = sqlx::PgPool::connect(&env::var("DATABASE_URL")?).await?;
        migrate(&pool).await?;
        return Ok(());
    }
    if args[1] == "evaluate" || args[1] == "publish-qualification" {
        let publishing = args[1] == "publish-qualification";
        if (!publishing && args.len() != 4) || (publishing && args.len() != 6) {
            return Err("usage: kernelctl evaluate <pack.json> <dataset.json> | publish-qualification <pack.json> <dataset.json> <approval-ref> <valid-until-unix-ms>".into());
        }
        let pack: DecisionPack =
            serde_json::from_value(kernel_core::strict_json(&fs::read(&args[2])?)?)?;
        let graph = compile(pack)?;
        let dataset: EvaluationDataset =
            serde_json::from_value(kernel_core::strict_json(&fs::read(&args[3])?)?)?;
        let report = evaluate(&graph, dataset)?;
        if !publishing {
            println!("{}", serde_json::to_string_pretty(&report)?);
            return Ok(());
        }
        let qualification = &graph.pack.qualification;
        let criteria = graph
            .pack
            .qualification_criteria
            .as_ref()
            .ok_or("pack has no qualification criteria")?;
        if !qualification.semantic_acceptance_qualified
            || !graph.pack.policy.accept_qualified_semantic
        {
            return Err("pack does not enable qualified semantic acceptance".into());
        }
        if qualification.provider_id.as_deref() != Some(&report.provider_id)
            || qualification.model_id.as_deref() != Some(&report.requested_model_id)
            || qualification.adapter_version.as_deref() != Some(&report.adapter_version)
        {
            return Err("evaluation identity does not match pack binding".into());
        }
        meets_criteria(&report, criteria)?;
        let approval_ref = &args[4];
        if approval_ref.is_empty() || approval_ref.len() > 256 {
            return Err("invalid approval reference".into());
        }
        let approved_by = env::var("KERNEL_QUALIFICATION_APPROVER")?;
        if approved_by.is_empty() || approved_by.len() > 128 {
            return Err("invalid qualification approver".into());
        }
        let valid_until: i64 = args[5].parse()?;
        let now_ms: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
            .try_into()?;
        if valid_until <= now_ms {
            return Err("qualification expiry must be in the future".into());
        }
        let tenant = env::var("KERNEL_TENANT_ID")?;
        let project = env::var("KERNEL_PROJECT_ID")?;
        let environment = env::var("KERNEL_ENVIRONMENT")?;
        let pool = sqlx::PgPool::connect(&env::var("DATABASE_URL")?).await?;
        let released:bool=sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM behavior_releases WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type=$4 AND version=$5 AND content_hash_sha256=$6 AND revoked_at IS NULL)")
            .bind(&tenant).bind(&project).bind(&environment).bind(&graph.pack.id).bind(&graph.pack.version).bind(&graph.content_hash_sha256).fetch_one(&pool).await?;
        if !released {
            return Err("matching active published release is required".into());
        }
        let reference = qualification
            .qualification_ref
            .as_ref()
            .ok_or("missing qualification reference")?;
        let inserted=sqlx::query("INSERT INTO provider_qualifications (tenant_id,project_id,environment,qualification_ref,release_hash_sha256,provider_id,model_id,returned_model_id,adapter_version,evaluation_digest_sha256,reviewed_sample_count,valid_until_unix_ms,evaluation_report,approval_ref,approved_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) ON CONFLICT DO NOTHING")
            .bind(&tenant).bind(&project).bind(&environment).bind(reference).bind(&graph.content_hash_sha256).bind(&report.provider_id).bind(&report.requested_model_id).bind(&report.model_id).bind(&report.adapter_version).bind(&report.dataset_digest_sha256).bind(i64::try_from(report.reviewed_test_cases)?).bind(valid_until).bind(serde_json::to_value(&report)?).bind(approval_ref).bind(approved_by)
            .execute(&pool).await?;
        if inserted.rows_affected() != 1 {
            return Err("qualification reference already exists".into());
        }
        println!(
            "published qualification {reference} for {}@{}",
            graph.pack.id, graph.pack.version
        );
        return Ok(());
    }
    if args[1] == "inspect" {
        if args.len() != 4 {
            return Err("usage: kernelctl inspect <decision-type> <version>".into());
        }
        let pool = sqlx::PgPool::connect(&env::var("DATABASE_URL")?).await?;
        let row = sqlx::query("SELECT compiled_graph FROM behavior_releases WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type=$4 AND version=$5")
            .bind(env::var("KERNEL_TENANT_ID")?).bind(env::var("KERNEL_PROJECT_ID")?).bind(env::var("KERNEL_ENVIRONMENT")?).bind(&args[2]).bind(&args[3]).fetch_one(&pool).await?;
        println!(
            "{}",
            serde_json::to_string_pretty(&row.get::<serde_json::Value, _>("compiled_graph"))?
        );
        return Ok(());
    }
    if args[1] == "inspect-active" {
        if args.len() != 3 {
            return Err("usage: kernelctl inspect-active <decision-type>".into());
        }
        let pool = sqlx::PgPool::connect(&env::var("DATABASE_URL")?).await?;
        let row = sqlx::query("SELECT p.version,p.content_hash_sha256,p.revision,r.revoked_at IS NOT NULL AS revoked FROM release_pointers p JOIN behavior_releases r ON r.tenant_id=p.tenant_id AND r.project_id=p.project_id AND r.environment=p.environment AND r.decision_type=p.decision_type AND r.version=p.version WHERE p.tenant_id=$1 AND p.project_id=$2 AND p.environment=$3 AND p.decision_type=$4")
            .bind(env::var("KERNEL_TENANT_ID")?).bind(env::var("KERNEL_PROJECT_ID")?).bind(env::var("KERNEL_ENVIRONMENT")?).bind(&args[2])
            .fetch_optional(&pool).await?.ok_or("release pointer not found")?;
        println!(
            "{}@{} revision {} {}{}",
            args[2],
            row.get::<String, _>("version"),
            row.get::<i64, _>("revision"),
            row.get::<String, _>("content_hash_sha256"),
            if row.get::<bool, _>("revoked") {
                " (revoked)"
            } else {
                ""
            },
        );
        return Ok(());
    }
    if matches!(args[1].as_str(), "promote" | "rollback") {
        if args.len() != 5 {
            return Err("usage: kernelctl promote|rollback <decision-type> <version> <expected-revision> (use 0 to create a pointer)".into());
        }
        let expected: i64 = args[4].parse()?;
        if expected < 0 {
            return Err("expected revision must be nonnegative".into());
        }
        let pool = sqlx::PgPool::connect(&env::var("DATABASE_URL")?).await?;
        let tenant = env::var("KERNEL_TENANT_ID")?;
        let project = env::var("KERNEL_PROJECT_ID")?;
        let environment = env::var("KERNEL_ENVIRONMENT")?;
        let mut tx = pool.begin().await?;
        let hash: Option<String> = sqlx::query_scalar("SELECT content_hash_sha256 FROM behavior_releases WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type=$4 AND version=$5 AND revoked_at IS NULL")
            .bind(&tenant).bind(&project).bind(&environment).bind(&args[2]).bind(&args[3])
            .fetch_optional(&mut *tx).await?;
        let hash = hash.ok_or("target release is missing or revoked")?;
        let changed = if expected == 0 {
            sqlx::query("INSERT INTO release_pointers (tenant_id,project_id,environment,decision_type,version,content_hash_sha256,revision) VALUES ($1,$2,$3,$4,$5,$6,1) ON CONFLICT DO NOTHING")
                .bind(&tenant).bind(&project).bind(&environment).bind(&args[2]).bind(&args[3]).bind(&hash)
                .execute(&mut *tx).await?.rows_affected()
        } else {
            sqlx::query("UPDATE release_pointers SET version=$5,content_hash_sha256=$6,revision=revision+1,updated_at=now() WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type=$4 AND revision=$7")
                .bind(&tenant).bind(&project).bind(&environment).bind(&args[2]).bind(&args[3]).bind(&hash).bind(expected)
                .execute(&mut *tx).await?.rows_affected()
        };
        if changed != 1 {
            return Err("release pointer revision conflict".into());
        }
        sqlx::query("INSERT INTO outbox_events (event_id,tenant_id,project_id,environment,event_type,payload) VALUES (gen_random_uuid()::text,$1,$2,$3,'release.promoted',$4)")
            .bind(&tenant).bind(&project).bind(&environment)
            .bind(serde_json::json!({"decision_type":args[2],"version":args[3],"revision":expected+1,"content_hash_sha256":hash}))
            .execute(&mut *tx).await?;
        tx.commit().await?;
        println!(
            "active {}@{} revision {} {}",
            args[2],
            args[3],
            expected + 1,
            hash
        );
        return Ok(());
    }
    if args[1] == "revoke-release" {
        if args.len() != 4 {
            return Err("usage: kernelctl revoke-release <decision-type> <version>".into());
        }
        let pool = sqlx::PgPool::connect(&env::var("DATABASE_URL")?).await?;
        let mut tx = pool.begin().await?;
        let changed = sqlx::query("UPDATE behavior_releases SET revoked_at=now() WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type=$4 AND version=$5 AND revoked_at IS NULL")
            .bind(env::var("KERNEL_TENANT_ID")?).bind(env::var("KERNEL_PROJECT_ID")?).bind(env::var("KERNEL_ENVIRONMENT")?).bind(&args[2]).bind(&args[3])
            .execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err("active release not found".into());
        }
        sqlx::query("INSERT INTO outbox_events (event_id,tenant_id,project_id,environment,event_type,payload) VALUES (gen_random_uuid()::text,$1,$2,$3,'release.revoked',$4)")
            .bind(env::var("KERNEL_TENANT_ID")?).bind(env::var("KERNEL_PROJECT_ID")?).bind(env::var("KERNEL_ENVIRONMENT")?)
            .bind(serde_json::json!({"decision_type":args[2],"version":args[3]}))
            .execute(&mut *tx).await?;
        tx.commit().await?;
        println!("revoked {}@{}", args[2], args[3]);
        return Ok(());
    }
    if args[1] == "register-token" {
        if args.len() != 2 {
            return Err("usage: kernelctl register-token (reads KERNEL_NEW_SERVICE_TOKEN and scope environment)".into());
        }
        let token = env::var("KERNEL_NEW_SERVICE_TOKEN")?;
        if token.len() < 32 {
            return Err("service token must contain at least 32 characters".into());
        }
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        let pool = sqlx::PgPool::connect(&env::var("DATABASE_URL")?).await?;
        let inserted=sqlx::query("INSERT INTO service_tokens (token_hash_sha256,tenant_id,project_id,environment,product_id) VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
            .bind(&hash).bind(env::var("KERNEL_TENANT_ID")?).bind(env::var("KERNEL_PROJECT_ID")?).bind(env::var("KERNEL_ENVIRONMENT")?).bind(env::var("KERNEL_PRODUCT_ID")?)
            .execute(&pool).await?;
        if inserted.rows_affected() != 1 {
            return Err("token hash already exists; rotate to a new token".into());
        }
        println!("registered token hash {hash}");
        return Ok(());
    }
    if args[1] == "revoke-token" {
        if args.len() != 3 || args[2].len() != 64 || !args[2].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("usage: kernelctl revoke-token <sha256-hash>".into());
        }
        let pool = sqlx::PgPool::connect(&env::var("DATABASE_URL")?).await?;
        let changed=sqlx::query("UPDATE service_tokens SET active=false,revoked_at=now() WHERE token_hash_sha256=$1 AND active")
            .bind(&args[2]).execute(&pool).await?;
        if changed.rows_affected() != 1 {
            return Err("active token not found".into());
        }
        println!("revoked token hash {}", args[2]);
        return Ok(());
    }
    if args[1] == "reconcile-attempt" {
        if args.len() != 5 {
            return Err("usage: kernelctl reconcile-attempt <idempotency-key> <verified-charge-nano-usd> <external-proof-ref>".into());
        }
        let charged: i64 = args[3].parse()?;
        if charged < 0 || args[4].is_empty() || args[4].len() > 256 {
            return Err("invalid reconciliation charge or proof reference".into());
        }
        let pool = sqlx::PgPool::connect(&env::var("DATABASE_URL")?).await?;
        let tenant = env::var("KERNEL_TENANT_ID")?;
        let project = env::var("KERNEL_PROJECT_ID")?;
        let environment = env::var("KERNEL_ENVIRONMENT")?;
        let mut tx = pool.begin().await?;
        let row = sqlx::query("SELECT purpose,reserved_nano_usd,status FROM provider_attempts WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND attempt_no=1 FOR UPDATE")
            .bind(&tenant).bind(&project).bind(&environment).bind(&args[2]).fetch_one(&mut *tx).await?;
        let reserved: i64 = row.get("reserved_nano_usd");
        let purpose: String = row.get("purpose");
        if row.get::<String, _>("status") == "reconciled" || charged > reserved {
            return Err("attempt already reconciled or verified charge exceeds hold".into());
        }
        let updated = sqlx::query("UPDATE budget_buckets SET held_nano_usd=held_nano_usd-$4,spent_nano_usd=spent_nano_usd+$5 WHERE tenant_id=$1 AND project_id=$2 AND purpose=$3 AND held_nano_usd >= $4")
            .bind(&tenant).bind(&project).bind(&purpose).bind(reserved).bind(charged).execute(&mut *tx).await?;
        if updated.rows_affected() != 1 {
            return Err("budget hold missing".into());
        }
        sqlx::query("UPDATE provider_attempts SET status='reconciled',actual_charge_nano_usd=$5,reconciliation_ref=$6 WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND attempt_no=1")
            .bind(&tenant).bind(&project).bind(&environment).bind(&args[2]).bind(charged).bind(&args[4]).execute(&mut *tx).await?;
        tx.commit().await?;
        println!(
            "reconciled attempt {} with verified charge {} nano-USD",
            args[2], charged
        );
        return Ok(());
    }
    if args[1] == "replay" {
        if args.len() != 4 {
            return Err("usage: kernelctl replay <pack.json> <recorded_decisions.json>".into());
        }
        let pack: DecisionPack =
            serde_json::from_value(kernel_core::strict_json(&fs::read(&args[2])?)?)?;
        let graph = compile(pack)?;
        let records_raw = fs::read(&args[3])?;
        let records: Vec<serde_json::Value> =
            serde_json::from_value(kernel_core::strict_json(&records_raw)?)?;
        let mut matched = 0;
        let mut mismatched = 0;
        for (i, rec) in records.iter().enumerate() {
            let request: kernel_core::DecisionRequest =
                serde_json::from_value(rec.get("request").cloned().unwrap_or_else(|| rec.clone()))?;
            let scope = kernel_core::Scope {
                tenant_id: "replay-tenant".into(),
                project_id: "replay-project".into(),
                environment: "replay".into(),
                product_id: "replay-product".into(),
            };
            let receipt = kernel_runtime::evaluate_pure(&graph, scope, request)?;
            if let Some(expected_disp) = rec.get("expected_disposition").and_then(|v| v.as_str()) {
                let actual_disp = serde_json::to_string(&receipt.disposition)?
                    .trim_matches('"')
                    .to_string();
                if actual_disp == expected_disp {
                    matched += 1;
                } else {
                    eprintln!(
                        "Record {i}: disposition mismatch: expected {expected_disp}, got {actual_disp}"
                    );
                    mismatched += 1;
                }
            } else {
                matched += 1;
            }
        }
        println!(
            "Replayed {} decisions: {} matched, {} mismatched",
            records.len(),
            matched,
            mismatched
        );
        if mismatched > 0 {
            return Err("replay found non-reproducible decisions".into());
        }
        return Ok(());
    }
    if args[1] == "bench" {
        let pack_path = if args.len() > 2 {
            &args[2]
        } else {
            "packs/examples/support-triage.json"
        };
        let pack: DecisionPack =
            serde_json::from_value(kernel_core::strict_json(&fs::read(pack_path)?)?)?;
        let graph = compile(pack)?;

        println!("=== Decision Kernel Performance Acceptance Benchmark (PRD Sec 17) ===");
        println!("Target Pack:  {}@{}", graph.pack.id, graph.pack.version);
        println!("Content Hash: {}", graph.content_hash_sha256);
        println!(
            "Nodes: {}, Edges: {}",
            graph.pack.nodes.len(),
            graph.dependencies.iter().map(|d| d.len()).sum::<usize>()
        );

        let scope = kernel_core::Scope {
            tenant_id: "bench-tenant".into(),
            project_id: "bench-project".into(),
            environment: "bench".into(),
            product_id: "bench-product".into(),
        };

        // Construct 4 KiB bounded fixture state
        let mut state = std::collections::BTreeMap::new();
        state.insert("egress_denied".into(), serde_json::json!(true));
        for i in 0..50 {
            state.insert(
                format!("field_{i}"),
                serde_json::json!(format!("bounded_string_value_payload_{i:04}")),
            );
        }

        let request = kernel_core::DecisionRequest {
            schema_version: "1.1".into(),
            decision_type: graph.pack.id.clone(),
            release: format!("{}@{}", graph.pack.id, graph.pack.version),
            idempotency_key: "bench-key-001".into(),
            state,
            evidence: vec![],
            constraints: kernel_core::RequestLimits::default(),
        };

        // Warm up pure deterministic evaluator
        for _ in 0..200 {
            let _ = kernel_runtime::evaluate_pure(&graph, scope.clone(), request.clone())?;
        }

        // Run 5,000 pure iterations
        let runs = 5000;
        let mut pure_latencies = Vec::with_capacity(runs);
        let bench_start = std::time::Instant::now();
        for _ in 0..runs {
            let t0 = std::time::Instant::now();
            let _ = kernel_runtime::evaluate_pure(&graph, scope.clone(), request.clone())?;
            pure_latencies.push(t0.elapsed().as_nanos() as f64 / 1_000_000.0);
        }
        let total_pure_time = bench_start.elapsed();

        pure_latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p50 = pure_latencies[(runs as f64 * 0.50) as usize];
        let p95 = pure_latencies[(runs as f64 * 0.95) as usize];
        let p99 = pure_latencies[(runs as f64 * 0.99) as usize];
        let throughput = runs as f64 / total_pure_time.as_secs_f64();

        println!("\n[Lane A: Pure Deterministic Core]");
        println!("Iterations:        {runs}");
        println!("Throughput:        {:.1} ops/sec", throughput);
        println!("Latency p50:       {:.4} ms", p50);
        println!(
            "Latency p95:       {:.4} ms  (Target: < 1.0 ms -> {})",
            p95,
            if p95 < 1.0 { "PASS" } else { "FAIL" }
        );
        println!("Latency p99:       {:.4} ms", p99);

        // Audited storage benchmark if DATABASE_URL is available
        if let Ok(db_url) = env::var("DATABASE_URL")
            && let Ok(pool) = sqlx::PgPool::connect(&db_url).await
        {
            println!("\n[Lane A: Audited No-Model Request (DB + Receipt Commit)]");
            let db_runs = 200;
            let store = std::sync::Arc::new(kernel_runtime::postgres::PostgresStore {
                pool: pool.clone(),
                purpose: graph.pack.id.clone(),
            });
            let engine = std::sync::Arc::new(kernel_runtime::Engine {
                graph: std::sync::Arc::new(graph.clone()),
                provider: None,
                store: store.clone(),
                provider_permits: std::sync::Arc::new(tokio::sync::Semaphore::new(16)),
                platform_deadline_ms: 3000,
                platform_max_cost_nano_usd: 1_000_000,
            });

            // Ensure release exists in DB
            let _ = sqlx::query("INSERT INTO behavior_releases (tenant_id,project_id,environment,decision_type,version,content_hash_sha256,compiled_graph) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING")
                    .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(&graph.pack.id).bind(&graph.pack.version).bind(&graph.content_hash_sha256).bind(serde_json::to_value(&graph)?)
                    .execute(&pool).await;

            let mut db_latencies = Vec::with_capacity(db_runs);
            let db_bench_start = std::time::Instant::now();
            for i in 0..db_runs {
                let mut req = request.clone();
                req.idempotency_key = format!(
                    "bench-audit-{}-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_nanos(),
                    i
                );
                let t0 = std::time::Instant::now();
                let _ = engine.decide(scope.clone(), req).await?;
                db_latencies.push(t0.elapsed().as_nanos() as f64 / 1_000_000.0);
            }
            let total_db_time = db_bench_start.elapsed();
            db_latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let db_p50 = db_latencies[(db_runs as f64 * 0.50) as usize];
            let db_p95 = db_latencies[(db_runs as f64 * 0.95) as usize];
            let db_p99 = db_latencies[(db_runs as f64 * 0.99) as usize];
            let db_throughput = db_runs as f64 / total_db_time.as_secs_f64();

            println!("Iterations:        {db_runs}");
            println!("Throughput:        {:.1} req/sec", db_throughput);
            println!("Latency p50:       {:.2} ms", db_p50);
            println!(
                "Latency p95:       {:.2} ms  (Target: < 50.0 ms -> {})",
                db_p95,
                if db_p95 < 50.0 { "PASS" } else { "FAIL" }
            );
            println!("Latency p99:       {:.2} ms", db_p99);
        }
        println!("\n=== Benchmark Completed Successfully ===");
        return Ok(());
    }
    if args.len() != 3 || !matches!(args[1].as_str(), "validate" | "compile" | "publish") {
        return Err("usage: kernelctl <validate|compile|publish|replay|bench> <pack.json>".into());
    }
    let pack: DecisionPack = serde_json::from_slice(&fs::read(&args[2])?)?;
    let graph = compile(pack)?;
    match args[1].as_str() {
        "validate" => println!("valid {}", graph.content_hash_sha256),
        "compile" => println!("{}", serde_json::to_string_pretty(&graph)?),
        "publish" => {
            let pool = sqlx::PgPool::connect(&env::var("DATABASE_URL")?).await?;
            let tenant = env::var("KERNEL_TENANT_ID")?;
            let project = env::var("KERNEL_PROJECT_ID")?;
            let environment = env::var("KERNEL_ENVIRONMENT")?;
            let result = sqlx::query("INSERT INTO behavior_releases (tenant_id,project_id,environment,decision_type,version,content_hash_sha256,compiled_graph) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING")
                .bind(&tenant).bind(&project).bind(&environment).bind(&graph.pack.id).bind(&graph.pack.version).bind(&graph.content_hash_sha256).bind(serde_json::to_value(&graph)?).execute(&pool).await?;
            if result.rows_affected() == 0 {
                let row = sqlx::query("SELECT content_hash_sha256 FROM behavior_releases WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type=$4 AND version=$5")
                    .bind(&tenant).bind(&project).bind(&environment).bind(&graph.pack.id).bind(&graph.pack.version).fetch_one(&pool).await?;
                if row.get::<String, _>("content_hash_sha256") != graph.content_hash_sha256 {
                    return Err("release version already exists with different content".into());
                }
            }
            println!(
                "published {}@{} {}",
                graph.pack.id, graph.pack.version, graph.content_hash_sha256
            );
        }
        _ => unreachable!(),
    }
    Ok(())
}

async fn migrate(pool: &sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let migrations: [(i64, &str); 13] = [
        (1, include_str!("../../../migrations/0001_init.sql")),
        (
            2,
            include_str!("../../../migrations/0002_prediction_cache.sql"),
        ),
        (
            3,
            include_str!("../../../migrations/0003_service_tokens.sql"),
        ),
        (
            4,
            include_str!("../../../migrations/0004_qualifications.sql"),
        ),
        (
            5,
            include_str!("../../../migrations/0005_compatibility.sql"),
        ),
        (
            6,
            include_str!("../../../migrations/0006_qualification_review.sql"),
        ),
        (
            7,
            include_str!("../../../migrations/0007_reviews_outcomes.sql"),
        ),
        (
            8,
            include_str!("../../../migrations/0008_release_pointers.sql"),
        ),
        (
            9,
            include_str!("../../../migrations/0009_returned_model_qualification.sql"),
        ),
        (10, include_str!("../../../migrations/0010_tenant_rls.sql")),
        (11, include_str!("../../../migrations/0011_outbox.sql")),
        (
            12,
            include_str!("../../../migrations/0012_outbox_failure.sql"),
        ),
        (
            13,
            include_str!("../../../migrations/0013_decision_jobs.sql"),
        ),
    ];
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(61273751)")
        .execute(&mut *tx)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS kernel_schema_migrations (version bigint PRIMARY KEY,checksum_sha256 text NOT NULL,applied_at timestamptz NOT NULL DEFAULT now())")
        .execute(&mut *tx).await?;
    for (version, sql) in migrations {
        let checksum = format!("{:x}", Sha256::digest(sql.as_bytes()));
        let prior: Option<String> = sqlx::query_scalar(
            "SELECT checksum_sha256 FROM kernel_schema_migrations WHERE version=$1",
        )
        .bind(version)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(prior) = prior {
            if prior != checksum {
                return Err(format!("migration {version} changed after application").into());
            }
            continue;
        }
        sqlx::raw_sql(sql).execute(&mut *tx).await?;
        sqlx::query(
            "INSERT INTO kernel_schema_migrations (version,checksum_sha256) VALUES ($1,$2)",
        )
        .bind(version)
        .bind(checksum)
        .execute(&mut *tx)
        .await?;
        println!("applied migration {version}");
    }
    tx.commit().await?;
    Ok(())
}
