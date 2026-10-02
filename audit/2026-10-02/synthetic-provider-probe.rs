use kernel_compiler::{DecisionPack,compile};
use kernel_core::{Answer,DecisionRequest,RequestLimits,Scope};
use kernel_provider::{FixtureProvider,SemanticResponse,ProviderIdentity,SemanticQuestion,normalize_system_one};
use kernel_runtime::{Engine,postgres::PostgresStore};
use serde_json::json;
use std::{collections::BTreeMap,sync::Arc};
use tokio::sync::Semaphore;
use sqlx::Row;

#[tokio::main]
async fn main() -> Result<(),Box<dyn std::error::Error>> {
 let scope=Scope{tenant_id:"audit-rust".into(),project_id:"audit".into(),environment:"test".into(),product_id:"product-a".into()};
 let pack:DecisionPack=serde_json::from_value(json!({
  "api_version":"kernel/v1","kind":"DecisionPack","id":"audit.cost","version":"1.1.0","required_evidence":[],
  "outputs":[{"kind":"binary","id":"ok","proposition":"p"}],
  "nodes":[{"id":"q","op":"semantic_question","output":"ok","prompt":"Is p true?"}],
  "policy":{"accept_deterministic":false,"accept_qualified_semantic":false},
  "limits":{"deadline_ms":1000,"max_billable_attempts":1,"max_cost_nano_usd":1000},
  "qualification":{"semantic_acceptance_qualified":false,"provider_id":"fixture","model_id":"fixture","adapter_version":"fixture-v1"}
 }))?;
 let graph=compile(pack)?;
 let pool=sqlx::PgPool::connect(&std::env::var("DATABASE_URL")?).await?;
 sqlx::query("INSERT INTO behavior_releases (tenant_id,project_id,environment,decision_type,version,content_hash_sha256,compiled_graph) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING")
  .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(&graph.pack.id).bind(&graph.pack.version).bind(&graph.content_hash_sha256).bind(serde_json::to_value(&graph)?).execute(&pool).await?;
 sqlx::query("INSERT INTO budget_buckets (tenant_id,project_id,purpose,cap_nano_usd) VALUES ($1,$2,'audit.cost',1000) ON CONFLICT DO NOTHING").bind(&scope.tenant_id).bind(&scope.project_id).execute(&pool).await?;
 let answer=Answer::Binary{proposition:"p".into(),value:Some(true),probability_true:None,probability_provenance:None};
 let provider=FixtureProvider{response:SemanticResponse{provider_id:"fixture".into(),model_id:"fixture".into(),answers:BTreeMap::from([("q".into(),answer)]),reported_cost_nano_usd:Some(100)}};
 let engine=Engine{graph:Arc::new(graph.clone()),provider:Some(Arc::new(provider)),store:Arc::new(PostgresStore{pool:pool.clone(),purpose:"audit.cost".into()}),provider_permits:Arc::new(Semaphore::new(1)),platform_deadline_ms:1000,platform_max_cost_nano_usd:1000};
 let request=DecisionRequest{schema_version:"1.1".into(),decision_type:"audit.cost".into(),release:"audit.cost@1.1.0".into(),idempotency_key:"underreserved".into(),state:BTreeMap::new(),evidence:vec![],constraints:RequestLimits{deadline_ms:Some(1000),max_billable_attempts:Some(1),max_cost_nano_usd:Some(1)}};
 let receipt=engine.decide(scope.clone(),request).await?;
 let row=sqlx::query("SELECT reserved_nano_usd,status FROM provider_attempts WHERE tenant_id='audit-rust' AND idempotency_key='underreserved'").fetch_one(&pool).await?;
 println!("{}",json!({"check":"reported_cost_exceeds_reserved_cap","attempts":receipt.attempts,"disposition":receipt.disposition,"reasons":receipt.reason_codes,"provider_fixture_reported_charge":100,"reserved_nano_usd":row.get::<i64,_>("reserved_nano_usd"),"attempt_status":row.get::<String,_>("status"),"receipt_provider_reported_cost":receipt.provider_reported_cost_nano_usd}));
 let identity=ProviderIdentity{provider_id:"typesafe".into(),model_id:"jev-1.13.0".into(),adapter_version:"system-one-v1".into(),allowed_returned_models:vec![]};
 let questions=vec![SemanticQuestion{id:"is_urgent".into(),prompt:"Does this convey urgency?".into(),output:kernel_core::OutputDefinition::Binary{id:"urgent".into(),proposition:"needs_urgent_review".into()}}];
 // The published TypeSafe sample is synthetic and used without a live API call.
 let published=json!({"model":"jev-1.13.0","answers":{"is_urgent":{"type":"noul","noul":0.95}},"usage":{"input_tokens":296,"output_tokens":20}});
 println!("{}",json!({"check":"system_one_published_schema_rejected","result":format!("{:?}",normalize_system_one(&serde_json::to_vec(&published)?,&identity,&questions))}));
 let mut modified=published.clone();modified["predictions"]=modified["answers"].clone();modified["predictions"]["is_urgent"]=json!({"value":true,"confidence":0.95});
 println!("{}",json!({"check":"system_one_native_probability_rejected","result":format!("{:?}",normalize_system_one(&serde_json::to_vec(&modified)?,&identity,&questions))}));
 Ok(())
}
