#!/usr/bin/env python3
"""BYOK proof against an isolated migrated owner DB and local HTTPS fixture.
Set KERNEL_TEST_DATABASE_URL and optionally KERNEL_TEST_PSQL_CONTAINER.
Never uses external providers. Build workspace before running.
"""
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
BIN = ROOT / 'target/debug'
OWNER = os.environ['KERNEL_TEST_DATABASE_URL']
PARTS = urllib.parse.urlsplit(OWNER)
ENV = {k: v for k, v in os.environ.items() if not k.startswith('KERNEL_')}
SUFFIX = secrets.token_hex(6)
ROLE = 'tier1_api_' + SUFFIX
WORKER = 'tier1_worker_' + SUFFIX
PASSWORD = secrets.token_hex(24)
TENANT = 'tier1-' + SUFFIX
ADMIN = secrets.token_hex(32)
KEY1, KEY2 = 'synthetic-key-one', 'synthetic-key-two'
CHECKS = []


def sql(statement):
    container = os.environ.get('KERNEL_TEST_PSQL_CONTAINER')
    cmd = ['docker', 'exec', '-i', container, 'psql', '-U', urllib.parse.unquote(PARTS.username), '-d', PARTS.path.lstrip('/')] if container else ['psql', OWNER]
    return subprocess.run(cmd + ['-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'], input=statement, text=True, capture_output=True, check=True).stdout.strip()


def check(name):
    CHECKS.append(name)
    print('PASS ' + name, flush=True)


def url(role):
    return urllib.parse.urlunsplit(PARTS._replace(netloc=f'{role}:{PASSWORD}@{PARTS.hostname}:{PARTS.port or 5432}'))


def wait_for(predicate):
    until = time.monotonic() + 15
    while time.monotonic() < until:
        try:
            if predicate():
                return
        except (OSError, urllib.error.URLError):
            pass
        time.sleep(.05)
    raise AssertionError('condition timed out')


with tempfile.TemporaryDirectory(prefix='kernel-tier1-') as directory:
    processes, logs = [], []
    try:
        sql(f"CREATE ROLE {ROLE} LOGIN PASSWORD '{PASSWORD}' NOSUPERUSER NOBYPASSRLS; GRANT kernel_runtime TO {ROLE}; CREATE ROLE {WORKER} LOGIN PASSWORD '{PASSWORD}' NOSUPERUSER NOBYPASSRLS; GRANT kernel_worker TO {WORKER};")
        tokens = {k: secrets.token_hex(24) for k in ['a', 'project-b', 'other']}
        scopes = {'a': (TENANT, 'a'), 'project-b': (TENANT, 'b'), 'other': (TENANT + '-other', 'a')}
        for label, token in tokens.items():
            tenant, project = scopes[label]
            digest = hashlib.sha256(token.encode()).hexdigest()
            sql(f"INSERT INTO service_tokens (token_hash_sha256,tenant_id,project_id,environment,product_id) VALUES ('{digest}','{tenant}','{project}','test','product');")
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        base = f'http://127.0.0.1:{port}'
        with HTTPSFixture(directory) as fixture:
            fixture.response_model = 'served-model'
            extra = {'KERNEL_CONNECTION_MASTER_KEY': secrets.token_hex(32), 'KERNEL_CONNECTION_ADMIN_TOKEN': ADMIN,
                     'KERNEL_PROVIDER_ALLOWED_HOSTS': '127.0.0.1', 'KERNEL_PROVIDER_CA_CERT_PATH': fixture.environment['KERNEL_PROVIDER_CA_CERT_PATH']}

            def start(binary, login):
                log = open(Path(directory) / (binary + '.log'), 'w+')
                logs.append(log)
                p = subprocess.Popen([str(BIN / binary)], env={**ENV, **extra, 'DATABASE_URL': url(login), 'KERNEL_LISTEN_ADDR': f'127.0.0.1:{port}'}, stdout=log, stderr=log)
                processes.append(p)
                return p

            def http(method, path, body=None, label='a', admin=True):
                headers = {'Authorization': 'Bearer ' + tokens[label], 'Content-Type': 'application/json'}
                if admin:
                    headers['x-kernel-admin-token'] = ADMIN
                if body and 'idempotency_key' in body:
                    headers['Idempotency-Key'] = body['idempotency_key']
                req = urllib.request.Request(base + path, method=method, headers=headers, data=json.dumps(body).encode() if body is not None else None)
                try:
                    response = urllib.request.urlopen(req, timeout=5)
                except urllib.error.HTTPError as error:
                    response = error
                with response:
                    raw = response.read()
                    assert KEY1.encode() not in raw and KEY2.encode() not in raw, raw
                    return response.status, json.loads(raw) if raw else None

            api = start('kerneld', ROLE)
            wait_for(lambda: http('GET', '/readyz')[0] == 200)
            assert http('GET', '/v1/provider-connections', admin=False)[0] == 403
            check('service token alone cannot manage BYOK connections')
            config = {'adapter': 'compatible', 'endpoint': fixture.environment['KERNEL_PROVIDER_BASE_URL'], 'provider_id': 'local-fixture', 'model_id': 'requested-model',
                      'allowed_returned_models': ['served-model'], 'max_output_tokens': 128, 'max_dispatch_nano_usd': 100}
            write = {'connection_id': 'byok', 'tenant_level': False, 'config': config, 'api_key': KEY1}
            invalid = {**write, 'config': {**config, 'endpoint': 'https://unapproved.example/v1'}}
            assert http('POST', '/v1/provider-connections', invalid)[0] == 400
            assert http('POST', '/v1/provider-connections', write)[0] == 204
            assert http('POST', '/v1/provider-connections', write)[0] == 409
            assert http('GET', '/v1/provider-connections', label='other')[1] == []
            assert http('GET', '/v1/provider-connections', label='project-b')[1] == []
            assert http('POST', '/v1/provider-connections', {**write, 'connection_id': 'tenant', 'tenant_level': True})[0] == 204
            visible = http('GET', '/v1/provider-connections', label='project-b')[1]
            assert len(visible) == 1 and visible[0]['connection_id'] == 'tenant'
            check('operator host allowlist and tenant/project RLS isolation')
            encrypted = sql(f"SELECT encode(encrypted_key,'hex') FROM provider_connections WHERE tenant_id='{TENANT}' AND project_id='a' AND connection_id='byok';")
            assert KEY1.encode().hex() not in encrypted and encrypted.startswith('01')
            assert http('POST', '/v1/provider-connections', {**write, 'api_key': KEY2, 'expected_revision': 1})[0] == 204
            assert http('POST', '/v1/provider-connections', {**write, 'expected_revision': 1})[0] == 409
            rotated = sql(f"SELECT encode(encrypted_key,'hex') FROM provider_connections WHERE tenant_id='{TENANT}' AND project_id='a' AND connection_id='byok';")
            assert rotated != encrypted
            check('ciphertext storage, atomic key rotation and stale revision rejection')
            for purpose in ['connection.test', 'tier1.live']:
                sql(f"INSERT INTO budget_buckets (tenant_id,project_id,environment,product_id,purpose,cap_nano_usd) VALUES ('{TENANT}','a','test','product','{purpose}',10000);")
            result = http('POST', '/v1/provider-connections/byok/test', {'idempotency_key': 'compatible'})
            assert result[0] == 200 and result[1]['connected'], result
            assert result[1]['requested_model'] == 'requested-model' and result[1]['served_model'] == 'served-model'
            assert fixture.auth_headers[-1] == 'Bearer ' + KEY2
            assert result[1]['capabilities']['supports_batch']
            assert fixture.count('probe') == 1
            assert http('POST', '/v1/provider-connections/byok/test', {'idempotency_key': 'compatible'})[1]['connected']
            assert fixture.count('probe') == 1
            native = {**write, 'connection_id': 'native', 'config': {**config, 'adapter': 'system-one'}}
            assert http('POST', '/v1/provider-connections', native)[0] == 204
            result = http('POST', '/v1/provider-connections/native/test', {'idempotency_key': 'native'})
            assert result[0] == 200 and result[1]['connected'], result
            check('budgeted idempotent compatible/native HTTPS probes preserve requested/served identity')
            pack = {'api_version': 'kernel/v1', 'kind': 'DecisionPack', 'id': 'tier1.live', 'version': '1', 'required_evidence': [], 'semantic_state_fields': ['case'],
                    'outputs': [{'kind': 'binary', 'id': 'ok', 'proposition': 'ok'}],
                    'nodes': [{'id': 'question', 'op': 'semantic_question', 'output': 'ok', 'prompt': 'Return true.'}],
                    'policy': {'accept_qualified_semantic': False}, 'limits': {'deadline_ms': 1000, 'max_billable_attempts': 1, 'max_cost_nano_usd': 1000},
                    'qualification': {'semantic_acceptance_qualified': False, 'provider_id': 'local-fixture', 'model_id': 'requested-model'}}
            file = Path(directory) / 'pack.json'
            file.write_text(json.dumps(pack))
            cli_env = {**ENV, 'DATABASE_URL': OWNER, 'KERNEL_TENANT_ID': TENANT, 'KERNEL_PROJECT_ID': 'a', 'KERNEL_ENVIRONMENT': 'test'}
            for args in [('publish', str(file)), ('promote', 'tier1.live', '1', '0')]:
                subprocess.run([str(BIN / 'kernelctl'), *args], env=cli_env, check=True, capture_output=True)
            assert http('PUT', '/v1/provider-bindings/tier1.live', {'connection_id': 'byok'})[0] == 204

            def decision(key, profile='fast'):
                return {'schema_version': '1.1', 'product_id': 'product', 'decision_type': 'tier1.live', 'release': 'active', 'idempotency_key': key,
                        'mode': 'evaluate', 'execution_profile': profile, 'state': {'case': key}, 'evidence_refs': [], 'constraints': {}}

            result = http('POST', '/v1/decisions', decision('byok-fast'))
            assert result[0] == 200 and result[1]['returned_model_id'] == 'served-model', result
            worker = start('kernel-worker', WORKER)
            queued = http('POST', '/v1/decision-jobs', decision('byok-job', 'deferred'))
            assert queued[0] == 202, queued
            job_id = queued[1]['job_id']
            wait_for(lambda: http('GET', '/v1/decision-jobs/' + job_id)[1].get('status') == 'completed')
            assert fixture.count('byok-job') == 1
            check('API and queued worker dispatch through external BYOK binding')
            assert http('DELETE', '/v1/provider-connections/byok', {'expected_revision': 2})[0] == 204
            assert sql(f"SELECT encrypted_key IS NULL FROM provider_connections WHERE tenant_id='{TENANT}' AND project_id='a' AND connection_id='byok';") == 't'
            before = sum(fixture.calls.values())
            assert http('POST', '/v1/decisions', decision('after-delete'))[0] == 503
            assert sum(fixture.calls.values()) == before
            assert http('POST', '/v1/provider-connections', {**write, 'expected_revision': 3})[0] == 409
            check('deletion erases active ciphertext and bound dispatch fails closed')
            stored = sql(f"SELECT row_to_json(r)::text FROM decision_receipts r WHERE tenant_id='{TENANT}'; SELECT row_to_json(a)::text FROM provider_attempts a WHERE tenant_id='{TENANT}'; SELECT semantic_response::text FROM prediction_cache WHERE tenant_id='{TENANT}';")
            assert KEY1 not in stored and KEY2 not in stored and 'debug_secret' not in stored
            check('provider echoes cannot leak keys into receipts, billing evidence or prediction cache')
            for p in processes:
                p.terminate()
            for p in processes:
                p.wait(timeout=10)
            for log in logs:
                log.flush(); log.seek(0)
                content = log.read()
                assert KEY1 not in content and KEY2 not in content, 'secret in process log'
            check('API and worker logs contain no provider keys')
        print(json.dumps({'checks': CHECKS, 'count': len(CHECKS)}, indent=2))
    finally:
        for p in processes:
            if p.poll() is None:
                p.terminate()
                p.wait(timeout=10)
        for log in logs:
            log.close()
        # Retain synthetic ledger rows for inspection; remove disposable logins.
        sql(f'DROP ROLE IF EXISTS {ROLE}; DROP ROLE IF EXISTS {WORKER};')
