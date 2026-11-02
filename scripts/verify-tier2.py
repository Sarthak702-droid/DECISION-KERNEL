#!/usr/bin/env python3
"""Tier 2 proof: multiple packs, validation and release lifecycle through real API/worker.
Requires built binaries and a migrated disposable KERNEL_TEST_DATABASE_URL.
KERNEL_TEST_PSQL_CONTAINER optionally runs psql inside the test container.
"""
import copy
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

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/debug'
OWNER = os.environ['KERNEL_TEST_DATABASE_URL']
PARTS = urllib.parse.urlsplit(OWNER)
ENV = {k: v for k, v in os.environ.items() if not k.startswith('KERNEL_')}
SUFFIX = secrets.token_hex(6)
API, WORKER = 'tier2_api_' + SUFFIX, 'tier2_worker_' + SUFFIX
PASSWORD, TOKEN = secrets.token_hex(24), secrets.token_hex(24)
TENANT = 'tier2-' + SUFFIX
CHECKS = []


def sql(statement):
    container = os.environ.get('KERNEL_TEST_PSQL_CONTAINER')
    cmd = ['docker', 'exec', '-i', container, 'psql', '-U', urllib.parse.unquote(PARTS.username), '-d', PARTS.path.lstrip('/')] if container else ['psql', OWNER]
    return subprocess.run(cmd + ['-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'], input=statement, text=True, capture_output=True, check=True).stdout.strip()


def check(name):
    CHECKS.append(name)
    print('PASS ' + name, flush=True)


def wait_for(predicate):
    until = time.monotonic() + 20
    while time.monotonic() < until:
        result = predicate()
        if result:
            return result
        time.sleep(.05)
    raise AssertionError('condition timed out')


with tempfile.TemporaryDirectory(prefix='kernel-tier2-') as directory:
    processes, logs = [], []
    try:
        sql(f"CREATE ROLE {API} LOGIN PASSWORD '{PASSWORD}' NOSUPERUSER NOBYPASSRLS; GRANT kernel_runtime TO {API}; CREATE ROLE {WORKER} LOGIN PASSWORD '{PASSWORD}' NOSUPERUSER NOBYPASSRLS; GRANT kernel_worker TO {WORKER};")
        sql(f"INSERT INTO service_tokens (token_hash_sha256,tenant_id,project_id,environment,product_id) VALUES ('{hashlib.sha256(TOKEN.encode()).hexdigest()}','{TENANT}','test','test','product');")
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        base = f'http://127.0.0.1:{port}'
        cli_env = {**ENV, 'DATABASE_URL': OWNER, 'KERNEL_TENANT_ID': TENANT, 'KERNEL_PROJECT_ID': 'test', 'KERNEL_ENVIRONMENT': 'test'}

        def cli(*args, success=True):
            result = subprocess.run([str(BIN / 'kernelctl'), *args], env=cli_env, capture_output=True, text=True)
            assert (result.returncode == 0) == success, (args, result.stderr)
            return result.stdout

        def start(binary, role):
            log = open(Path(directory) / (binary + '.log'), 'w+')
            logs.append(log)
            login = urllib.parse.urlunsplit(PARTS._replace(netloc=f'{role}:{PASSWORD}@{PARTS.hostname}:{PARTS.port or 5432}'))
            p = subprocess.Popen([str(BIN / binary)], env={**ENV, 'DATABASE_URL': login, 'KERNEL_LISTEN_ADDR': f'127.0.0.1:{port}'}, stdout=log, stderr=log)
            processes.append(p)

        def http(method, path, body=None):
            headers = {'Authorization': 'Bearer ' + TOKEN, 'Content-Type': 'application/json'}
            if body and 'idempotency_key' in body:
                headers['Idempotency-Key'] = body['idempotency_key']
            request = urllib.request.Request(base + path, method=method, headers=headers, data=json.dumps(body).encode() if body is not None else None)
            try:
                response = urllib.request.urlopen(request, timeout=5)
            except urllib.error.HTTPError as error:
                response = error
            except urllib.error.URLError:
                return 0, None
            with response:
                raw = response.read()
                return response.status, json.loads(raw) if raw else None

        expense = json.loads((ROOT / 'packs/examples/expense-approval.json').read_text())
        content = json.loads((ROOT / 'packs/examples/content-moderation.json').read_text())
        expense_file = Path(directory) / 'expense.json'
        expense_file.write_text(json.dumps(expense))
        for path in [expense_file, ROOT / 'packs/examples/content-moderation.json', ROOT / 'packs/examples/support-triage.json']:
            cli('validate', str(path))
            cli('publish', str(path))
        cli('publish', str(expense_file))
        altered = copy.deepcopy(expense)
        altered['nodes'][2]['value'] = 50
        expense_file.write_text(json.dumps(altered))
        cli('publish', str(expense_file), success=False)
        altered['version'] = '2.0.0'
        expense_file.write_text(json.dumps(altered))
        cli('publish', str(expense_file))
        invalid = copy.deepcopy(expense)
        invalid['version'] = 'bad'
        invalid['state_preparation']['version'] = 'unknown'
        expense_file.write_text(json.dumps(invalid))
        cli('publish', str(expense_file), success=False)
        expense_file.write_text('{"kind":"DecisionPack","kind":"DecisionPack"}')
        cli('validate', str(expense_file), success=False)
        assert sql(f"SELECT count(*) FROM behavior_releases WHERE tenant_id='{TENANT}';") == '4'
        check('strict validation, immutable publication, idempotent publication and invalid release rejection')
        cli('promote', expense['id'], '1.0.0', '0')
        cli('promote', content['id'], '1.0.0', '0')
        cli('promote', 'support.triage', '1.1.0', '0')
        start('kerneld', API)
        wait_for(lambda: http('GET', '/readyz')[0] == 200)
        evidence = {'reference': 'receipt-1'}
        status, _ = http('POST', '/v1/evidence', {'evidence_id': 'receipt', 'revision': 1,
            'observed_at_unix_ms': int(time.time() * 1000) - 1000, 'expires_at_unix_ms': None,
            'digest_sha256': hashlib.sha256(json.dumps(evidence, separators=(',', ':'), sort_keys=True).encode()).hexdigest(),
            'sensitivity': 'public', 'value': evidence})
        assert status == 201, status

        def body(key, state=None, pack='expense.approval', profile='fast'):
            return {'schema_version': '1.1', 'product_id': 'product', 'decision_type': pack, 'release': 'active',
                'idempotency_key': key, 'mode': 'evaluate', 'execution_profile': profile,
                'state': state if state is not None else {'amount': 100, 'vendor_allowed': True, 'country': ' IN '},
                'evidence_refs': [{'id': 'receipt', 'revision': 1}] if pack == 'expense.approval' else [], 'constraints': {}}

        for key, changes, expected in [('accept', {}, 'accept'), ('review', {'amount': 1000}, 'review'),
                ('abstain', {'uncertain': True}, 'abstain'), ('deny', {'vendor_allowed': False, 'uncertain': True, 'amount': 1000}, 'deny')]:
            request = body(key)
            request['state'].update(changes)
            status, receipt = http('POST', '/v1/decisions', request)
            assert status == 200 and receipt['disposition'] == expected, (status, receipt)
            assert len(receipt['answers']) == 3 and receipt['attempts'] == 0
            assert receipt['answers']['priority']['selected_anchor'] == 'normal'
        check('one generic engine produces all four dispositions and binary/categorical/ordinal answers')
        status, receipt = http('POST', '/v1/decisions', body('content', {'text': ' hello ', 'blocked': False}, content['id']))
        assert status == 200 and receipt['disposition'] == 'abstain' and receipt['reason_codes'] == ['provider_unavailable'], (status, receipt)
        legacy = body('legacy', {'egress_denied': True}, 'support.triage')
        legacy['release'] = 'support.triage@1.1.0'
        assert http('POST', '/v1/decisions', legacy)[1]['disposition'] == 'deny'
        check('three application packs execute without engine-specific application code; absent provider uses declared fallback')
        for profile, endpoint in [('fast', '/v1/decisions'), ('deferred', '/v1/decision-jobs')]:
            invalid = body('invalid-' + profile, {'amount': '100', 'vendor_allowed': True, 'country': 'in'}, profile=profile)
            assert http('POST', endpoint, invalid)[0] == 400
        assert sql(f"SELECT count(*) FROM decision_receipts WHERE tenant_id='{TENANT}' AND idempotency_key LIKE 'invalid-%';") == '0'
        assert sql(f"SELECT count(*) FROM decision_jobs WHERE tenant_id='{TENANT}' AND idempotency_key LIKE 'invalid-%';") == '0'
        check('invalid pack input is rejected before receipt claims and deferred admission')
        status, queued = http('POST', '/v1/decision-jobs', body('pinned-job', profile='deferred'))
        assert status == 202, (status, queued)
        cli('promote', expense['id'], '2.0.0', '1')
        cli('promote', expense['id'], '1.0.0', '1', success=False)
        status, receipt = http('POST', '/v1/decisions', body('after-promotion'))
        assert status == 200 and receipt['disposition'] == 'review' and receipt['release'].endswith('@2.0.0'), (status, receipt)
        start('kernel-worker', WORKER)
        def completed():
            status, job = http('GET', '/v1/decision-jobs/' + queued['job_id'])
            return job if status == 200 and job['status'] == 'completed' else None
        job = wait_for(completed)
        assert job['receipt']['release'].endswith('@1.0.0') and job['receipt']['disposition'] == 'accept', job
        check('promotion uses revision compare-and-swap; deferred execution retains its pinned pack and state preparation')
        cli('rollback', expense['id'], '1.0.0', '2')
        status, receipt = http('POST', '/v1/decisions', body('after-rollback'))
        assert status == 200 and receipt['disposition'] == 'accept' and receipt['release'].endswith('@1.0.0'), (status, receipt)
        assert sql(f"SELECT count(*) FROM outbox_events WHERE tenant_id='{TENANT}' AND event_type='release.rolled_back';") == '1'
        cli('revoke-release', expense['id'], '2.0.0')
        cli('promote', expense['id'], '2.0.0', '3', success=False)
        check('rollback restores prior behavior, emits its own event and revoked releases cannot be promoted')
        print(json.dumps({'result': 'PASS', 'count': len(CHECKS), 'checks': CHECKS}, indent=2))
    except BaseException:
        for log in logs:
            log.flush()
            log.seek(0)
            print(log.read(), flush=True)
        raise
    finally:
        for p in reversed(processes):
            if p.poll() is None:
                p.terminate()
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                p.kill()
                p.wait(timeout=5)
        for log in logs:
            log.close()
        sql(f'DROP ROLE IF EXISTS {API}; DROP ROLE IF EXISTS {WORKER};')
