#!/usr/bin/env python3
"""Synthetic API + worker proof; requires an isolated, migrated test database.

KERNEL_TEST_DATABASE_URL must be an owner connection. Uses psql, or
KERNEL_TEST_PSQL_CONTAINER to execute psql inside a local test container.
Build workspace binaries first: cargo build --workspace --locked.
"""
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from tier0_provider_fixture import HTTPSFixture

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target' / 'debug'
OWNER = os.environ['KERNEL_TEST_DATABASE_URL']
OWNER_PARTS = urllib.parse.urlsplit(OWNER)
ENV = {k: v for k, v in os.environ.items() if not k.startswith('KERNEL_')}
SUFFIX = secrets.token_hex(6)
API_ROLE, WORKER_ROLE = 'tier0_api_' + SUFFIX, 'tier0_worker_' + SUFFIX
PASSWORD = secrets.token_hex(24)
TENANT, OTHER = 'tier0-http-' + SUFFIX, 'tier0-other-' + SUFFIX
CHECKS = []


def sql(statement):
    container = os.environ.get('KERNEL_TEST_PSQL_CONTAINER')
    if container:
        cmd = ['docker', 'exec', '-i', container, 'psql', '-U', urllib.parse.unquote(OWNER_PARTS.username), '-d', OWNER_PARTS.path.lstrip('/')]
    else:
        cmd = ['psql', OWNER]
    return subprocess.run(cmd + ['-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'],
                          input=statement, text=True, capture_output=True, check=True).stdout.strip()


def quote(value):
    return "'" + value.replace("'", "''") + "'"


def login_url(role):
    netloc = f'{role}:{PASSWORD}@{OWNER_PARTS.hostname}:{OWNER_PARTS.port or 5432}'
    return urllib.parse.urlunsplit(OWNER_PARTS._replace(netloc=netloc))


def check(name):
    CHECKS.append(name)
    print('PASS ' + name, flush=True)


def cli(*args, **env):
    result = subprocess.run([str(BIN / 'kernelctl'), *args], env={**ENV, 'DATABASE_URL': OWNER, **env},
                            text=True, capture_output=True, check=True)
    return result.stdout.strip()


def wait_for(predicate, seconds=15):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        try:
            value = predicate()
            if value:
                return value
        except (OSError, urllib.error.URLError):
            pass
        time.sleep(0.05)
    raise AssertionError('condition did not become true')


with tempfile.TemporaryDirectory(prefix='kernel-tier0-') as directory:
    log_path = Path(directory)
    processes = []
    logs = []
    try:
        sql(f"CREATE ROLE {API_ROLE} LOGIN PASSWORD '{PASSWORD}' NOSUPERUSER NOBYPASSRLS; GRANT kernel_runtime TO {API_ROLE}; "
            f"CREATE ROLE {WORKER_ROLE} LOGIN PASSWORD '{PASSWORD}' NOSUPERUSER NOBYPASSRLS; GRANT kernel_worker TO {WORKER_ROLE};")
        tokens = {label: secrets.token_hex(24) for label in ['a', 'b', 'other']}
        for label, token in tokens.items():
            tenant = OTHER if label == 'other' else TENANT
            product = 'a' if label == 'other' else label
            cli('register-token', KERNEL_NEW_SERVICE_TOKEN=token, KERNEL_TENANT_ID=tenant,
                KERNEL_PROJECT_ID='test', KERNEL_ENVIRONMENT='test', KERNEL_PRODUCT_ID=product)
        pack = {
            'api_version': 'kernel/v1', 'kind': 'DecisionPack', 'id': 'tier0.http', 'version': '1',
            'required_evidence': [],
            'outputs': [{'kind': 'binary', 'id': 'ok', 'proposition': 'ok'}],
            'nodes': [
                {'id': 'deny', 'op': 'fact_equals', 'field': 'permission', 'value': False},
                {'id': 'answer', 'op': 'literal_answer', 'output': 'ok', 'answer': {'kind': 'binary', 'proposition': 'ok', 'value': True, 'probability_true': None, 'probability_provenance': None}}],
            'policy': {'hard_deny_nodes': ['deny'], 'accept_deterministic': True, 'accept_qualified_semantic': False},
            'limits': {'deadline_ms': 1000, 'max_billable_attempts': 1, 'max_cost_nano_usd': 1000},
            'qualification': {'semantic_acceptance_qualified': False},
        }
        pack_file = log_path / 'pack.json'
        pack_file.write_text(json.dumps(pack))
        for tenant in [TENANT, OTHER]:
            cli('publish', str(pack_file), KERNEL_TENANT_ID=tenant, KERNEL_PROJECT_ID='test', KERNEL_ENVIRONMENT='test')
            cli('promote', 'tier0.http', '1', '0', KERNEL_TENANT_ID=tenant, KERNEL_PROJECT_ID='test', KERNEL_ENVIRONMENT='test')
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        base = f'http://127.0.0.1:{port}'

        def start(binary, url, extra=None):
            log = open(log_path / (binary + str(len(processes)) + '.log'), 'w+')
            logs.append(log)
            process = subprocess.Popen([str(BIN / binary)], env={**ENV, 'DATABASE_URL': url, 'KERNEL_LISTEN_ADDR': f'127.0.0.1:{port}', **(extra or {})}, stdout=log, stderr=log)
            processes.append(process)
            return process

        def http(method, path, label='a', body=None):
            headers = {'Authorization': 'Bearer ' + tokens[label], 'Content-Type': 'application/json'}
            if body and 'idempotency_key' in body:
                headers['Idempotency-Key'] = body['idempotency_key']
            request = urllib.request.Request(base + path, method=method, headers=headers,
                                             data=json.dumps(body).encode() if body is not None else None)
            try:
                response = urllib.request.urlopen(request, timeout=5)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                raw = response.read()
                return response.status, json.loads(raw) if raw else None

        def body(key, label='a', profile='fast', state=None, refs=None):
            return {'schema_version': '1.1', 'product_id': 'a' if label == 'other' else label,
                    'decision_type': 'tier0.http', 'release': 'active', 'idempotency_key': key,
                    'mode': 'evaluate', 'execution_profile': profile,
                    'state': {'permission': True} if state is None else state,
                    'evidence_refs': refs or [], 'constraints': {}}

        api = start('kerneld', login_url(API_ROLE))
        wait_for(lambda: http('GET', '/readyz')[0] == 200)
        check('daemon starts with runtime login and readiness succeeds')
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            duplicates = list(pool.map(lambda _: http('POST', '/v1/decisions', body=body('same')), range(8)))
        assert all(status == 200 for status, _ in duplicates), duplicates
        receipt = duplicates[0][1]
        assert receipt['disposition'] == 'accept'
        assert len({r['receipt_id'] for _, r in duplicates}) == 1
        count = sql(f"SELECT count(*) FROM outbox_events WHERE tenant_id='{TENANT}' AND product_id='a' AND event_type='decision.completed' AND payload->>'idempotency_key'='same';")
        assert count == '1', count
        check('eight concurrent duplicates produce one receipt and one completed event')
        assert http('POST', '/v1/decisions', body=body('same', state={'permission': False}))[0] == 409
        for label in ['b', 'other']:
            assert http('GET', '/v1/decisions/same', label)[0] == 404
            status, isolated = http('POST', '/v1/decisions', label, body('same', label))
            assert status == 200 and isolated['scope']['product_id'] == ('a' if label == 'other' else label)
            assert isolated['receipt_id'] != receipt['receipt_id']
        check('authenticated duplicate POST, GET and conflicts isolate products and tenants')
        assert http('POST', '/v1/decisions', body=body('unknown', state={}))[1]['reason_codes'] == ['missing_permission']
        assert http('POST', '/v1/decisions', body=body('denied', state={'permission': False}))[1]['disposition'] == 'deny'
        missing = body('missing', refs=[{'id': 'unavailable-a', 'revision': 1}])
        status, review = http('POST', '/v1/decisions', body=missing)
        assert status == 200 and review['reason_codes'] == ['missing_evidence']
        assert review['evidence_revisions'] == {'unavailable-a': 1}
        missing['evidence_refs'][0]['id'] = 'unavailable-b'
        assert http('POST', '/v1/decisions', body=missing)[0] == 409
        check('missing permission and unresolved evidence fail closed with input identity preserved')
        value = {'source': 'product-a'}
        digest = hashlib.sha256(json.dumps(value, separators=(',', ':'), sort_keys=True).encode()).hexdigest()
        evidence = {'evidence_id': 'same-ev', 'revision': 1, 'observed_at_unix_ms': 1,
                    'expires_at_unix_ms': None, 'digest_sha256': digest, 'sensitivity': 'internal', 'value': value}
        assert http('POST', '/v1/evidence', body=evidence)[0] == 201
        status, invisible = http('POST', '/v1/decisions', 'b', body('ev-b', 'b', refs=[{'id': 'same-ev', 'revision': 1}]))
        assert status == 200 and invisible['reason_codes'] == ['missing_evidence']
        assert http('POST', '/v1/evidence', 'b', evidence)[0] == 201
        check('evidence ingestion and reads are product scoped')
        invalid_job = body('bad-job', profile='deferred')
        invalid_job['schema_version'] = 'bad'
        assert http('POST', '/v1/decision-jobs', body=invalid_job)[0] == 400
        queued = body('job', profile='deferred')
        status, job = http('POST', '/v1/decision-jobs', body=queued)
        assert status == 202, job
        assert http('POST', '/v1/decision-jobs', body=queued)[1]['job_id'] == job['job_id']
        assert http('GET', '/v1/decision-jobs/' + job['job_id'], 'b')[0] == 404
        worker = start('kernel-worker', login_url(WORKER_ROLE))
        def completed():
            status, found = http('GET', '/v1/decision-jobs/' + job['job_id'])
            return found if status == 200 and found['status'] == 'completed' else None
        found = wait_for(completed)
        assert found['receipt']['disposition'] == 'accept'
        worker.terminate(); worker.wait(timeout=5)
        # Expired lease emulates a worker process dying after claiming a job.
        status, crashed_job = http('POST', '/v1/decision-jobs', body=body('job-crash', profile='deferred'))
        assert status == 202
        sql(f"UPDATE decision_jobs SET status='leased',lease_owner='dead-worker',lease_generation=1,lease_until=now()-interval '1 second' WHERE job_id={quote(crashed_job['job_id'])};")
        worker = start('kernel-worker', login_url(WORKER_ROLE))
        wait_for(lambda: http('GET', '/v1/decision-jobs/' + crashed_job['job_id'])[1]['status'] == 'completed')
        check('worker login completes API jobs and reclaims crashed leases without duplicate receipts')
        # Lock token authentication itself: the caller deadline must include it.
        lock_statement = 'BEGIN; LOCK TABLE service_tokens IN ACCESS EXCLUSIVE MODE; SELECT pg_sleep(1.5); ROLLBACK;'
        container = os.environ.get('KERNEL_TEST_PSQL_CONTAINER')
        command = (['docker', 'exec', '-i', container, 'psql', '-U', urllib.parse.unquote(OWNER_PARTS.username), '-d', OWNER_PARTS.path.lstrip('/')]
                   if container else ['psql', OWNER])
        locker = subprocess.Popen(command + ['-X', '-q', '-v', 'ON_ERROR_STOP=1', '-c', lock_statement], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        processes.append(locker)
        wait_for(lambda: sql("SELECT count(*) FROM pg_locks WHERE relation='service_tokens'::regclass AND mode='AccessExclusiveLock' AND granted;") == '1')
        bounded = body('auth-deadline'); bounded['constraints']['deadline_ms'] = 50
        began = time.monotonic(); status, timed = http('POST', '/v1/decisions', body=bounded); elapsed = time.monotonic() - began
        assert status == 504 and timed['error']['code'] == 'DEADLINE_EXCEEDED' and elapsed < 0.5, (status, timed, elapsed)
        locker.wait(timeout=5)
        check('50 ms deadline includes authentication DB lock wait')
        # Repeat for release resolution and evidence lookup before engine entry.
        for table, refs in [('release_pointers', []), ('evidence', [{'id': 'same-ev', 'revision': 1}])]:
            statement = f'BEGIN; LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE; SELECT pg_sleep(1.5); ROLLBACK;'
            locker = subprocess.Popen(command + ['-X', '-q', '-v', 'ON_ERROR_STOP=1', '-c', statement], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            processes.append(locker)
            wait_for(lambda: sql(f"SELECT count(*) FROM pg_locks WHERE relation='{table}'::regclass AND mode='AccessExclusiveLock' AND granted;") == '1')
            bounded = body('deadline-' + table, refs=refs); bounded['constraints']['deadline_ms'] = 50
            began = time.monotonic(); status, timed = http('POST', '/v1/decisions', body=bounded)
            assert status == 504 and time.monotonic() - began < 0.5, (table, status, timed)
            locker.wait(timeout=5)
        check('deadline includes release and evidence DB waits')
        # Dependency loss gives explicit readiness failure, then recovers.
        sql(f'REVOKE UPDATE (revoked_at) ON behavior_releases FROM kernel_runtime;')
        assert http('GET', '/readyz')[0] == 503
        sql('GRANT UPDATE (revoked_at) ON behavior_releases TO kernel_runtime;')
        assert http('GET', '/readyz')[0] == 200
        check('readiness detects lost write privileges and recovers')
        # Fresh process must reject owner credentials and wrong worker credentials.
        for binary, url in [('kerneld', OWNER), ('kernel-worker', login_url(API_ROLE))]:
            bad = start(binary, url); assert bad.wait(timeout=8) == 1
        check('startup rejects owner API credentials and runtime-only worker credentials')
        for binary, url in [('kerneld', login_url(API_ROLE)), ('kernel-worker', login_url(WORKER_ROLE))]:
            invalid = start(binary, url, {'KERNEL_PROVIDER_BASE_URL': 'https://example.invalid',
                'KERNEL_PROVIDER_ALLOWED_HOST': 'example.invalid', 'KERNEL_PROVIDER_API_KEY': 'synthetic',
                'KERNEL_PROVIDER_ID': 'synthetic', 'KERNEL_PROVIDER_MODEL': 'synthetic',
                'KERNEL_PROVIDER_ADAPTER': 'unknown'})
            assert invalid.wait(timeout=8) == 1
        unreachable = urllib.parse.urlsplit(login_url(API_ROLE))._replace(netloc=f'{API_ROLE}:{PASSWORD}@127.0.0.1:1')
        failed = start('kerneld', urllib.parse.urlunsplit(unreachable))
        assert failed.wait(timeout=8) == 1
        check('invalid provider configuration and unavailable database fail startup explicitly')
        # The complete paid path uses real TLS/HTTP adapter traffic and synthetic
        # invoices; it never contacts an external provider.
        api.terminate(); api.wait(timeout=5)
        worker.terminate(); worker.wait(timeout=5)
        with HTTPSFixture(log_path) as fixture:
            semantic_pack = {**pack, 'id': 'tier0.semantic', 'semantic_state_fields': ['case'],
                'nodes': [pack['nodes'][0], {'id': 'q', 'op': 'semantic_question', 'output': 'ok', 'prompt': 'Is it okay?'}],
                'policy': {'hard_deny_nodes': ['deny'], 'accept_deterministic': False, 'accept_qualified_semantic': False},
                'qualification': {'semantic_acceptance_qualified': False, 'provider_id': 'local-fixture', 'model_id': 'test-model', 'adapter_version': 'compatible-v1'}}
            pack_file.write_text(json.dumps(semantic_pack))
            cli('publish', str(pack_file), KERNEL_TENANT_ID=TENANT, KERNEL_PROJECT_ID='test', KERNEL_ENVIRONMENT='test')
            cli('promote', 'tier0.semantic', '1', '0', KERNEL_TENANT_ID=TENANT, KERNEL_PROJECT_ID='test', KERNEL_ENVIRONMENT='test')
            sql(f"INSERT INTO budget_buckets (tenant_id,project_id,environment,product_id,purpose,cap_nano_usd) VALUES ('{TENANT}','test','test','a','tier0.semantic',1000),('{TENANT}','test','test','b','tier0.semantic',1000);")
            api = start('kerneld', login_url(API_ROLE), fixture.environment)
            wait_for(lambda: http('GET', '/readyz')[0] == 200)

            def paid_body(key, case='valid', label='a', profile='fast'):
                value = body(key, label, profile, state={'permission': True, 'case': case})
                value['decision_type'] = 'tier0.semantic'
                return value

            def reconcile(key, cost, product='a'):
                cli('reconcile-attempt', key, str(cost), 'fixture-invoice-' + key,
                    KERNEL_TENANT_ID=TENANT, KERNEL_PROJECT_ID='test', KERNEL_ENVIRONMENT='test', KERNEL_PRODUCT_ID=product)

            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                results = list(pool.map(lambda _: http('POST', '/v1/decisions', body=paid_body('paid-same')), range(8)))
            assert all(status == 200 for status, _ in results), results
            paid_receipt = results[0][1]
            assert paid_receipt['attempts'] == 1 and paid_receipt['provider_reported_cost_nano_usd'] == 80
            assert paid_receipt['provider_usage'] == {'input_tokens': 10, 'output_tokens': 2}
            assert fixture.count('valid') == 1
            status, cached = http('POST', '/v1/decisions', body=paid_body('paid-cache'))
            assert status == 200 and cached['prediction_cache_hit'] and cached['attempts'] == 0
            assert fixture.count('valid') == 1
            status, isolated = http('POST', '/v1/decisions', 'b', paid_body('paid-same', label='b'))
            assert status == 200 and isolated['attempts'] == 1 and not isolated['prediction_cache_hit']
            assert fixture.count('valid') == 2
            reconcile('paid-same', 80); reconcile('paid-same', 80)
            reconcile('paid-same', 80, 'b')
            check('HTTPS paid duplicates dispatch once; exact cache and billing isolate products')
            cheap = paid_body('unaffordable', 'fresh'); cheap['constraints']['max_cost_nano_usd'] = 99
            status, denied = http('POST', '/v1/decisions', body=cheap)
            assert status == 200 and denied['attempts'] == 0 and denied['reason_codes'] == ['budget_unavailable']
            assert fixture.count('fresh') == 0
            for case, cost in [('overspend', 150), ('invalid', 90)]:
                status, invalid = http('POST', '/v1/decisions', body=paid_body('paid-' + case, case))
                assert status == 200 and invalid['disposition'] == 'review', invalid
                assert invalid['provider_reported_cost_nano_usd'] == cost and invalid['provider_usage']['input_tokens'] == 10
                reconcile('paid-' + case, cost)
                assert http('GET', '/v1/decisions/paid-' + case)[1]['cost_nano_usd'] == cost
            check('pre-dispatch price bound, invalid-output billing and overspend reconciliation pass')
            status, paid_job = http('POST', '/v1/decision-jobs', body=paid_body('paid-job', 'worker', profile='deferred'))
            assert status == 202
            worker = start('kernel-worker', login_url(WORKER_ROLE), fixture.environment)
            def paid_completed():
                status, found = http('GET', '/v1/decision-jobs/' + paid_job['job_id'])
                return found if status == 200 and found['status'] == 'completed' else None
            found = wait_for(paid_completed)
            assert found['receipt']['attempts'] == 1 and found['receipt']['provider_reported_cost_nano_usd'] == 80
            assert fixture.count('worker') == 1
            reconcile('paid-job', 80)
            worker.terminate(); worker.wait(timeout=5)
            check('API queued semantic job executes through HTTPS with the worker login')
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                pending = pool.submit(http, 'POST', '/v1/decisions', 'a', paid_body('process-crash', 'slow'))
                wait_for(lambda: fixture.count('slow') == 1)
                api.kill(); api.wait(timeout=5)
                try:
                    pending.result(timeout=5)
                except (OSError, urllib.error.URLError):
                    pass
            sql(f"UPDATE decision_receipts SET created_at=now()-interval '2 minutes' WHERE tenant_id='{TENANT}' AND product_id='a' AND idempotency_key='process-crash';")
            api = start('kerneld', login_url(API_ROLE), fixture.environment)
            worker = start('kernel-worker', login_url(WORKER_ROLE), fixture.environment)
            wait_for(lambda: http('GET', '/readyz')[0] == 200)
            def recovered_receipt():
                status, receipt = http('GET', '/v1/decisions/process-crash')
                return receipt if status == 200 else None
            recovered = wait_for(recovered_receipt)
            assert recovered['disposition'] == 'review' and recovered['reason_codes'] == ['crash_recovery']
            assert recovered['cost_nano_usd'] is None
            status, repeated = http('POST', '/v1/decisions', body=paid_body('process-crash', 'slow'))
            assert status == 200 and repeated['receipt_id'] == recovered['receipt_id'] and fixture.count('slow') == 1
            reconcile('process-crash', 80)
            usage = http('GET', '/v1/usage')[1]
            ledger = next(row for row in usage if row['purpose'] == 'tier0.semantic')
            assert ledger['spent_nano_usd'] == 480 and ledger['held_nano_usd'] == 0, ledger
            check('actual API process death recovers one terminal receipt with no paid redispatch')
        print(json.dumps({'result': 'PASS', 'checks': CHECKS}, indent=2))
    except BaseException:
        for log in logs:
            log.flush(); log.seek(0)
            print(log.read(), flush=True)
        raise
    finally:
        # Restore privileges even after a failed assertion.
        try:
            sql('GRANT UPDATE (revoked_at) ON behavior_releases TO kernel_runtime;')
        finally:
            for process in reversed(processes):
                if process.poll() is None:
                    process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill(); process.wait(timeout=5)
            for log in logs:
                log.close()
            sql(f'DROP ROLE {API_ROLE}; DROP ROLE {WORKER_ROLE};')
