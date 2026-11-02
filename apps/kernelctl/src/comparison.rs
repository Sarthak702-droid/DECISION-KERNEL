use kernel_compiler::{CompiledDecisionGraph, DecisionPack, Operation, compile};
use kernel_core::{DecisionReceipt, DecisionRequest};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs};
type Error = Box<dyn std::error::Error>;
fn read(path: &str) -> Result<Value, Error> {
    Ok(kernel_core::strict_json(&fs::read(path)?)?)
}
fn semantics(graph: &CompiledDecisionGraph) -> Value {
    json!({"outputs":graph.pack.outputs,"questions":graph.pack.nodes.iter().filter(|n|matches!(n.operation,Operation::SemanticQuestion{..})).collect::<Vec<_>>(),
        "input_schema":graph.pack.input_schema,"preparation":graph.pack.state_preparation,"evidence_requirements":graph.pack.evidence_requirements,"fields":graph.pack.semantic_state_fields,"evidence":graph.pack.required_evidence,
        "sensitivities":graph.pack.permitted_provider_sensitivities,
        "provider":graph.pack.qualification.provider_id,"model":graph.pack.qualification.model_id,"adapter":graph.pack.qualification.adapter_version})
}
pub fn run(args: &[String]) -> Result<(), Error> {
    if args.len() != 5 {
        return Err("usage: kernelctl compare-stored <baseline-pack.json> <candidate-pack.json> <scope-export.json>".into());
    }
    let baseline = compile(serde_json::from_value::<DecisionPack>(read(&args[2])?)?)?;
    let candidate = compile(serde_json::from_value::<DecisionPack>(read(&args[3])?)?)?;
    if baseline.pack.id != candidate.pack.id || semantics(&baseline) != semantics(&candidate) {
        return Err("stored predictions cannot qualify changed semantic questions, inputs, output contracts or provider identity; collect fresh held-out predictions".into());
    }
    let export = read(&args[4])?;
    if export["schema_version"] != "kernel-scope-export/v1" {
        return Err("scope export required".into());
    }
    let scope: kernel_core::Scope = serde_json::from_value(export["scope"].clone())?;
    if !scope.is_valid() {
        return Err("invalid export scope".into());
    }
    let rows = export["data"]["recorded_requests"]
        .as_array()
        .ok_or("recorded requests absent")?;
    let mut requests = BTreeMap::new();
    for row in rows {
        let req: DecisionRequest = serde_json::from_value(row["request"].clone())?;
        if requests.insert(req.idempotency_key.clone(), req).is_some() {
            return Err("duplicate recorded request".into());
        }
    }
    let mut comparisons = Vec::new();
    let mut excluded = 0;
    for row in export["data"]["decision_receipts"]
        .as_array()
        .ok_or("receipts absent")?
    {
        if row["status"] != "committed" {
            excluded += 1;
            continue;
        }
        let receipt: DecisionReceipt = serde_json::from_value(row["receipt"].clone())?;
        if receipt.scope != scope {
            return Err("receipt scope mismatch".into());
        }
        if receipt.release_digest_sha256 != baseline.content_hash_sha256 {
            excluded += 1;
            continue;
        }
        let request = requests
            .get(row["idempotency_key"].as_str().ok_or("invalid key")?)
            .ok_or("request unavailable; legacy receipts cannot be faithfully replayed")?;
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(&scope, request))?)
        );
        if digest != receipt.request_digest_sha256
            || receipt.release != format!("{}@{}", baseline.pack.id, baseline.pack.version)
        {
            return Err("stored request digest or release mismatch".into());
        }
        let before = kernel_runtime::evaluate_recorded(
            &baseline,
            scope.clone(),
            request.clone(),
            receipt.created_at_unix_ms,
            Some(&receipt.answers),
        )?;
        let mut after_request = request.clone();
        after_request.release = format!("{}@{}", candidate.pack.id, candidate.pack.version);
        let after = kernel_runtime::evaluate_recorded(
            &candidate,
            scope.clone(),
            after_request,
            receipt.created_at_unix_ms,
            Some(&receipt.answers),
        )?;
        let changed = before.disposition != after.disposition
            || serde_json::to_value(&before.answers)? != serde_json::to_value(&after.answers)?;
        comparisons.push(json!({"key":request.idempotency_key,"source_receipt_id":receipt.receipt_id,
            "source_request_digest_sha256":digest,"observed_at_unix_ms":receipt.created_at_unix_ms,
            "source_disposition":receipt.disposition,"baseline":before,"candidate":after,"changed":changed}));
    }
    if comparisons.is_empty() {
        return Err("no replayable baseline receipts".into());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"schema_version":"kernel-comparison/v1",
        "baseline_digest":baseline.content_hash_sha256,"candidate_digest":candidate.content_hash_sha256,
        "export_digest_sha256":format!("{:x}",Sha256::digest(fs::read(&args[4])?)),
        "offline_policy_simulation":true,"semantic_qualification_granted":false,"excluded_receipts":excluded,
        "compared":comparisons.len(),"changed":comparisons.iter().filter(|r|r["changed"]==true).count(),"rows":comparisons}))?
    );
    Ok(())
}
