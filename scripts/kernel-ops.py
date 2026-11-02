#!/usr/bin/env python3
"""Owner-only maintenance; never supply this database credential to API/worker.
Use PGSERVICE/PGPASSFILE or DATABASE_URL. KERNEL_OPS_PSQL_CONTAINER is for local drills.
All commands are scoped to all four required KERNEL_* identity variables.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess

TABLES = ['recorded_requests', 'decision_reviews', 'decision_outcomes', 'decision_receipts',
          'evidence', 'prediction_cache', 'provider_attempts', 'decision_jobs', 'outbox_events',
          'budget_buckets', 'provider_bindings', 'operation_limits', 'operation_windows']
TOKEN_TABLES = ['service_tokens', 'reviewer_tokens']

def quote(value):
    return "convert_from(decode('" + str(value).encode().hex() + "','hex'),'UTF8')"

def query(statement):
    container = os.environ.get('KERNEL_OPS_PSQL_CONTAINER')
    command = ['docker', 'exec', '-i', container, 'psql', '-U', os.environ.get('PGUSER', 'postgres'), '-d', os.environ.get('PGDATABASE', 'kernel_test')] if container else ['psql', os.environ.get('DATABASE_URL', '')]
    result = subprocess.run(command + ['-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'], input=statement, text=True, capture_output=True)
    if result.returncode:
        raise SystemExit(result.stderr)
    return result.stdout.strip()

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['block', 'export', 'delete', 'retention', 'status'])
    parser.add_argument('--output', type=Path)
    parser.add_argument('--export-sha256')
    parser.add_argument('--older-than-days', type=int, default=30)
    args = parser.parse_args()
    scope = dict(zip(['tenant_id','project_id','environment','product_id'], [os.environ['KERNEL_'+k] for k in ['TENANT_ID','PROJECT_ID','ENVIRONMENT','PRODUCT_ID']]))
    if any(not v or len(v)>128 for v in scope.values()):
        raise SystemExit('invalid scope')
    where = ' AND '.join(k+'='+quote(v) for k,v in scope.items())
    owner = "DO $$ BEGIN IF NOT (SELECT rolsuper FROM pg_roles WHERE rolname=current_user) AND NOT pg_has_role(current_user,(SELECT relowner FROM pg_class WHERE oid='decision_receipts'::regclass),'USAGE') THEN RAISE EXCEPTION 'owner maintenance credential required'; END IF; END $$;"
    # A single lock protects concurrent maintenance. Export is repeatable-read.
    begin = "BEGIN ISOLATION LEVEL REPEATABLE READ; SET LOCAL statement_timeout='60s'; SET LOCAL lock_timeout='5s'; SELECT pg_advisory_xact_lock(61273752);" + owner
    if args.command == 'block':
        # Persist block before quiescing workloads, including static-auth callers.
        query(begin + 'INSERT INTO lifecycle_blocks ('+','.join(scope)+') VALUES ('+','.join(quote(v) for v in scope.values())+') ON CONFLICT DO NOTHING;' + ''.join('UPDATE '+t+' SET active=false WHERE '+where+';' for t in TOKEN_TABLES) + 'COMMIT;')
        print(json.dumps({'scope':scope,'blocked':True,'next':'Stop API/worker and provider/outbox dispatch before export/delete; reconcile uncertain charges.'}))
    elif args.command == 'export':
        if not args.output:
            raise SystemExit('--output required (contains sensitive customer data)')
        # Token hashes and encrypted provider secrets are intentionally excluded.
        fields = ','.join(quote(t)+",(SELECT COALESCE(jsonb_agg(to_jsonb(t)), '[]'::jsonb) FROM "+t+' t WHERE '+where+')' for t in TABLES)
        raw = query(begin + 'SELECT jsonb_build_object('+fields+');COMMIT;').splitlines()[-1]
        data = {'schema_version':'kernel-scope-export/v1','scope':scope,'data':json.loads(raw)}
        payload = (json.dumps(data,sort_keys=True,indent=2)+'\n').encode()
        fd = os.open(args.output, os.O_WRONLY|os.O_CREAT|os.O_EXCL, 0o600)
        with os.fdopen(fd,'wb') as handle: handle.write(payload)
        import hashlib
        print(json.dumps({'path':str(args.output),'sha256':hashlib.sha256(payload).hexdigest()}))
    elif args.command == 'delete':
        if not args.output or not args.export_sha256:
            raise SystemExit('delete requires --output existing export and --export-sha256')
        import hashlib
        payload=args.output.read_bytes()
        exported=json.loads(payload)
        if hashlib.sha256(payload).hexdigest()!=args.export_sha256 or exported.get('scope')!=scope:
            raise SystemExit('export digest/scope mismatch')
        # Locks block in-flight writes; caller must stop processes first. Block survives restore/deletion.
        guard = "DO $$ BEGIN IF NOT EXISTS(SELECT 1 FROM lifecycle_blocks WHERE "+where+") THEN RAISE EXCEPTION 'block first'; END IF; IF EXISTS(SELECT 1 FROM provider_attempts WHERE "+where+" AND status<>'reconciled') OR EXISTS(SELECT 1 FROM budget_buckets WHERE "+where+" AND held_nano_usd<>0) OR EXISTS(SELECT 1 FROM decision_jobs WHERE "+where+" AND status IN ('queued','leased')) OR EXISTS(SELECT 1 FROM decision_receipts WHERE "+where+" AND status='pending') THEN RAISE EXCEPTION 'unresolved financial or work state'; END IF; END $$;"
        # Check fresh snapshot equals export before destructive work. Reject stale exports.
        checks=''
        for table in TABLES:
            expected=json.dumps(exported['data'][table],sort_keys=True)
            # JSON arrays have unstable SQL ordering; compare sets in both directions.
            checks+="DO $$ BEGIN IF EXISTS((SELECT to_jsonb(t) FROM "+table+' t WHERE '+where+") EXCEPT SELECT value FROM jsonb_array_elements("+quote(expected)+"::jsonb)) OR EXISTS((SELECT value FROM jsonb_array_elements("+quote(expected)+"::jsonb)) EXCEPT SELECT to_jsonb(t) FROM "+table+' t WHERE '+where+") THEN RAISE EXCEPTION 'export stale'; END IF; END $$;"
        query(begin+'LOCK TABLE '+','.join(TABLES+TOKEN_TABLES)+' IN EXCLUSIVE MODE;'+guard+checks+"SET LOCAL kernel.lifecycle_delete='on';"+''.join('DELETE FROM '+t+' WHERE '+where+';' for t in TABLES+TOKEN_TABLES)+'COMMIT;')
        print(json.dumps({'scope':scope,'deleted':True,'export_sha256':args.export_sha256,'block_retained':True}))
    elif args.command == 'retention':
        if not 1 <= args.older_than_days <= 3650: raise SystemExit('days must be 1..3650')
        # No financial records, receipts, feedback or inputs are removed by routine cache retention.
        query(begin+'DELETE FROM prediction_cache WHERE '+where+' AND expires_at_unix_ms < extract(epoch FROM now())*1000; DELETE FROM operation_windows WHERE '+where+f' AND window_start < floor(extract(epoch FROM now())/60)-{args.older_than_days*1440};COMMIT;')
        print(json.dumps({'scope':scope,'expired_cache_and_idle_windows_pruned':True}))
    else:
        print(query(begin+"SELECT jsonb_build_object('unresolved_attempts',(SELECT count(*) FROM provider_attempts WHERE "+where+" AND status<>'reconciled'),'held_nano_usd',(SELECT COALESCE(sum(held_nano_usd),0) FROM budget_buckets WHERE "+where+"),'queued_jobs',(SELECT count(*) FROM decision_jobs WHERE "+where+" AND status IN ('queued','leased')),'outbox_pending',(SELECT count(*) FROM outbox_events WHERE "+where+" AND delivered_at IS NULL));COMMIT;").splitlines()[-1])

if __name__ == '__main__': main()
