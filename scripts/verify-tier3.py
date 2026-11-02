#!/usr/bin/env python3
"""Release-mode HTTP performance contract against a disposable migrated database.
Standard-library open-loop driver; scheduling delay is included in client latency.
All provider traffic goes to the local HTTPS fixture, never a paid service.
"""
import argparse
import concurrent.futures
import copy
import hashlib
import http.client as http_client
import json
import math
import os
from pathlib import Path
import platform
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import urllib.parse
from tier0_provider_fixture import HTTPSFixture

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--requests', type=int, default=3000)
parser.add_argument('--rps', type=float, default=100)
parser.add_argument('--concurrency', type=int, default=32)
parser.add_argument('--output', default='audit/2026-11-02-tier3/results.json')
args = parser.parse_args()
assert args.requests >= 100 and 0 < args.rps and args.concurrency > 0
OWNER = os.environ['KERNEL_TEST_DATABASE_URL']
PARTS = urllib.parse.urlsplit(OWNER)
BIN = ROOT / 'target/release'
ENV = {k: v for k, v in os.environ.items() if not k.startswith('KERNEL_')}
SUFFIX = secrets.token_hex(6)
API, WORKER = 'tier3_api_' + SUFFIX, 'tier3_worker_' + SUFFIX
TENANT, PASSWORD, TOKEN = 'tier3-' + SUFFIX, secrets.token_hex(24), secrets.token_hex(24)
REPORT = {'workload': {'requests_per_path': args.requests, 'offered_rps': args.rps,
    'driver_threads': args.concurrency, 'api_admission': 64, 'provider_concurrency': 16,
    'db_connections': 16, 'provider_fixture_delay_ms': 20, 'http_keep_alive': True},
    'host': {'platform': platform.platform(), 'cpu_count': os.cpu_count(),
        'rustc': subprocess.check_output(['rustc', '--version'], text=True).strip(),
        'cpu': next((line.split(':', 1)[1].strip() for line in Path('/proc/cpuinfo').read_text().splitlines() if line.startswith('model name')), 'unknown')}, 'paths': {}}


def sql(statement):
    container = os.environ.get('KERNEL_TEST_PSQL_CONTAINER')
    cmd = ['docker', 'exec', '-i', container, 'psql', '-U', urllib.parse.unquote(PARTS.username), '-d', PARTS.path.lstrip('/')] if container else ['psql', OWNER]
    return subprocess.run(cmd + ['-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'], input=statement, text=True, capture_output=True, check=True).stdout.strip()


def login(role):
    return urllib.parse.urlunsplit(PARTS._replace(netloc=f'{role}:{PASSWORD}@{PARTS.hostname}:{PARTS.port or 5432}'))


def percentiles(values):
    values = sorted(values)
    return {f'p{p}': values[max(0, math.ceil(len(values) * p / 100) - 1)] for p in [50, 95, 99]} | {'max': values[-1]}


with tempfile.TemporaryDirectory(prefix='kernel-tier3-') as directory:
    processes, logs, connections = [], [], []
    local = threading.local()
    fixture = HTTPSFixture(directory)
    fixture.keep_alive = True
    fixture.delay_seconds = .020
    try:
        sql(f"CREATE ROLE {API} LOGIN PASSWORD '{PASSWORD}' NOSUPERUSER NOBYPASSRLS; GRANT kernel_runtime TO {API}; CREATE ROLE {WORKER} LOGIN PASSWORD '{PASSWORD}' NOSUPERUSER NOBYPASSRLS; GRANT kernel_worker TO {WORKER};")
        sql(f"INSERT INTO service_tokens(token_hash_sha256,tenant_id,project_id,environment,product_id) VALUES ('{hashlib.sha256(TOKEN.encode()).hexdigest()}','{TENANT}','test','test','product');")
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]

        def cli(*command):
            subprocess.run([str(BIN / 'kernelctl'), *command], env={**ENV, 'DATABASE_URL': OWNER,
                'KERNEL_TENANT_ID': TENANT, 'KERNEL_PROJECT_ID': 'test', 'KERNEL_ENVIRONMENT': 'test'}, capture_output=True, text=True, check=True)

        def start(binary, role, extra=None):
            log = open(Path(directory) / (binary + str(len(processes)) + '.log'), 'w+')
            logs.append(log)
            process = subprocess.Popen([str(BIN / binary)], env={**ENV, **fixture.environment,
                'DATABASE_URL': login(role), 'KERNEL_LISTEN_ADDR': f'127.0.0.1:{port}',
                'KERNEL_REQUEST_CONCURRENCY': '64', 'KERNEL_REQUESTS_PER_MINUTE': '1000000', 'KERNEL_PROVIDER_CONCURRENCY': '16',
                'KERNEL_DB_CONNECTIONS': '16', **(extra or {})}, stdout=log, stderr=log)
            processes.append(process)
            return process

        def http(method, path, body=None, due=None):
            start_at = time.perf_counter()
            if not hasattr(local, 'connection'):
                local.connection = http_client.HTTPConnection('127.0.0.1', port, timeout=5)
                connections.append(local.connection)
            headers = {'Authorization': 'Bearer ' + TOKEN, 'Content-Type': 'application/json'}
            if body and 'idempotency_key' in body:
                headers['Idempotency-Key'] = body['idempotency_key']
            try:
                local.connection.request(method, path, json.dumps(body) if body is not None else None, headers)
                response = local.connection.getresponse()
                raw = response.read()
                timings = {}
                for key, value in response.getheaders():
                    if key.lower() == 'server-timing':
                        for part in value.split(','):
                            name, duration = part.strip().split(';dur=')
                            timings[name + '_ms'] = float(duration)
                return {'status': response.status, 'body': json.loads(raw) if raw else None,
                    'client_ms': (time.perf_counter() - (due or start_at)) * 1000,
                    'schedule_delay_ms': (start_at - (due or start_at)) * 1000, **timings}
            except (OSError, http_client.HTTPException) as error:
                local.connection.close()
                del local.connection
                return {'status': 0, 'error': type(error).__name__, 'client_ms': (time.perf_counter() - (due or start_at)) * 1000}

        def wait_for(predicate, seconds=15):
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                value = predicate()
                if value:
                    return value
                time.sleep(.01)
            raise AssertionError('timed out')

        def body(key, semantic=False, case='cache', profile='fast'):
            return {'schema_version': '1.1', 'product_id': 'product', 'decision_type': 'tier3.semantic' if semantic else 'tier3.rules',
                'release': 'active', 'idempotency_key': key, 'mode': 'evaluate', 'execution_profile': profile,
                'state': {'permission': True, 'case': case, 'payload': 'x' * 1024} if semantic else {'amount': 100, 'vendor_allowed': True, 'country': ' IN '},
                'evidence_refs': [] if semantic else [{'id': 'receipt', 'revision': 1}], 'constraints': {}}

        def load(label, factory):
            rows = []
            start_at = time.perf_counter() + .05
            with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrency) as executor:
                futures = []
                for i in range(args.requests):
                    due = start_at + i / args.rps
                    time.sleep(max(0, due - time.perf_counter()))
                    futures.append(executor.submit(http, 'POST', '/v1/decisions', factory(i), due))
                rows = [future.result() for future in futures]
            assert all(r['status'] == 200 for r in rows), [(r['status'], r.get('body')) for r in rows if r['status'] != 200][:10]
            for row in rows:
                row['non_provider_ms'] = row['http_ms'] - row['provider_ms']
            metrics = {name: percentiles([r[name] for r in rows]) for name in
                ['client_ms', 'engine_ms', 'db_ms', 'provider_ms', 'queue_ms', 'http_ms', 'schedule_delay_ms', 'non_provider_ms']}
            REPORT['paths'][label] = {'metrics': metrics, 'samples': rows,
                'completion_rps': len(rows) / (time.perf_counter() - start_at)}
            print(label, json.dumps(metrics), flush=True)
            return rows

        with fixture:
            pack = {'api_version': 'kernel/v1', 'kind': 'DecisionPack', 'id': 'tier3.rules', 'version': '1',
                'required_evidence': [], 'semantic_state_fields': ['case', 'payload'],
                'outputs': [{'kind': 'binary', 'id': 'ok', 'proposition': 'ok'}],
                'nodes': [{'id': 'deny', 'op': 'fact_equals', 'field': 'permission', 'value': False},
                    {'id': 'answer', 'op': 'literal_answer', 'output': 'ok', 'answer': {'kind': 'binary', 'proposition': 'ok', 'value': True, 'probability_true': None, 'probability_provenance': None}}],
                'policy': {'hard_deny_nodes': ['deny'], 'accept_deterministic': True, 'accept_qualified_semantic': False},
                'limits': {'deadline_ms': 2000, 'max_billable_attempts': 1, 'max_cost_nano_usd': 1000},
                'qualification': {'semantic_acceptance_qualified': False}}
            for semantic in [False, True]:
                current = copy.deepcopy(pack)
                if not semantic:
                    current = json.loads((ROOT / 'packs/examples/expense-approval.json').read_text())
                    current.update(id='tier3.rules', version='1')
                if semantic:
                    current['id'] = 'tier3.semantic'
                    current['nodes'][1] = {'id': 'answer', 'op': 'semantic_question', 'output': 'ok', 'prompt': 'Is it okay?'}
                    current['qualification'] |= {'provider_id': 'local-fixture', 'model_id': 'test-model', 'adapter_version': 'compatible-v1'}
                path = Path(directory) / 'pack.json'
                path.write_text(json.dumps(current))
                cli('publish', str(path))
                cli('promote', current['id'], '1', '0')
            # Budget reservation is required even for review-only fixture output.
            sql(f"INSERT INTO budget_buckets(tenant_id,project_id,environment,product_id,purpose,cap_nano_usd) VALUES ('{TENANT}','test','test','product','tier3.semantic',1000000000);")
            api = start('kerneld', API)
            wait_for(lambda: http('GET', '/readyz')['status'] == 200)
            evidence = {'reference': 'tier3-receipt'}
            assert http('POST', '/v1/evidence', {'evidence_id': 'receipt', 'revision': 1,
                'observed_at_unix_ms': int(time.time() * 1000), 'expires_at_unix_ms': None,
                'digest_sha256': hashlib.sha256(json.dumps(evidence, separators=(',', ':'), sort_keys=True).encode()).hexdigest(),
                'sensitivity': 'public', 'value': evidence})['status'] == 201
            for i in range(30):
                assert http('POST', '/v1/decisions', body(f'warm-{i}'))['status'] == 200
            warm = http('POST', '/v1/decisions', body('seed', True))
            assert warm['status'] == 200 and warm['body']['attempts'] == 1, warm
            before = fixture.count('cache')
            rules = load('rules', lambda i: body(f'rules-{i}'))
            cache = load('cache', lambda i: body(f'cache-{i}', True))
            assert fixture.count('cache') == before and all(r['body']['prediction_cache_hit'] and r['body']['attempts'] == 0 for r in cache)
            assert all(r['body']['disposition'] == 'accept' and r['body']['attempts'] == 0 for r in rules)
            fresh = load('fresh', lambda i: body(f'fresh-{i}', True, f'fresh-{i}'))
            assert all(r['body']['attempts'] == 1 and not r['body']['prediction_cache_hit'] for r in fresh)
            assert sum(v for k, v in fixture.calls.items() if k.startswith('fresh-')) == args.requests
            REPORT['connection_reuse'] = {'provider_calls': sum(fixture.calls.values()), 'tcp_connections': len(fixture.connections)}
            assert len(fixture.connections) < args.requests / 2, REPORT['connection_reuse']
            # Concurrent equivalent semantics with distinct durable request IDs.
            with concurrent.futures.ThreadPoolExecutor(max_workers=16) as executor:
                duplicates = list(executor.map(lambda i: http('POST', '/v1/decisions', body(f'dup-{i}', True, 'duplicate')), range(16)))
            assert all(r['status'] == 200 for r in duplicates) and fixture.count('duplicate') == 1
            assert sum(r['body']['attempts'] for r in duplicates) == 1
            REPORT['duplicate_suppression'] = {'requests': 16, 'provider_calls': fixture.count('duplicate')}
            # An idle worker should pick up jobs within its polling envelope.
            worker = start('kernel-worker', WORKER, {'KERNEL_WORKER_POLL_MS': '100'})
            time.sleep(.3)
            worker_rows = []
            for i in range(30):
                began = time.perf_counter()
                response = http('POST', '/v1/decision-jobs', body(f'worker-{i}', profile='deferred'))
                assert response['status'] == 202, response
                job_id = response['body']['job_id']
                done = wait_for(lambda: (lambda r: r['body'] if r['status'] == 200 and r['body']['status'] == 'completed' else None)(http('GET', '/v1/decision-jobs/' + job_id)))
                assert done['receipt']['disposition'] == 'accept'
                worker_rows.append((time.perf_counter() - began) * 1000)
            REPORT['worker'] = {'samples': worker_rows, 'enqueue_to_observed_completion_ms': percentiles(worker_rows)}
            worker.terminate()
            worker.wait(timeout=5)
            worker_log = logs[-1]
            worker_log.seek(0)
            REPORT['worker']['runtime_timings'] = [json.loads(line) for line in worker_log if line.startswith('{') and 'decision_job_timing' in line]
            assert len(REPORT['worker']['runtime_timings']) == 30
            api.terminate()
            api.wait(timeout=5)
            for connection in connections:
                connection.close()
            local = threading.local()
            fixture.delay_seconds = .15
            fixture.max_active = 0
            api = start('kerneld', API, {'KERNEL_REQUEST_CONCURRENCY': '4', 'KERNEL_PROVIDER_CONCURRENCY': '2'})
            wait_for(lambda: http('GET', '/readyz')['status'] == 200)
            barrier = threading.Barrier(64)
            def saturate(i):
                barrier.wait()
                return http('POST', '/v1/decisions', body(f'saturation-{i}', True, f'saturation-{i}'))
            with concurrent.futures.ThreadPoolExecutor(max_workers=64) as executor:
                saturated = list(executor.map(saturate, range(64)))
            statuses = {str(status): sum(r['status'] == status for r in saturated) for status in {r['status'] for r in saturated}}
            assert set(statuses) == {'200', '429'} and fixture.max_active <= 2, (statuses, fixture.max_active)
            REPORT['saturation'] = {'offered_concurrency': 64, 'admission': 4, 'provider_limit': 2,
                'statuses': statuses, 'observed_provider_concurrency': fixture.max_active,
                'rejection_latency_ms': percentiles([r['client_ms'] for r in saturated if r['status'] == 429])}
            # Fixed gates: a run cannot redefine its own targets from its results.
            targets = {'rules': {'client_ms': (25, 50)}, 'cache': {'client_ms': (25, 50)}, 'fresh': {'engine_ms': (5, 10), 'non_provider_ms': (25, 50)}}
            REPORT['targets'] = targets
            failures = []
            for label, metrics in targets.items():
                for metric, (p95, p99) in metrics.items():
                    measured = REPORT['paths'][label]['metrics'][metric]
                    if measured['p95'] > p95 or measured['p99'] > p99:
                        failures.append(f'{label}.{metric}: {measured}, target p95={p95}, p99={p99}')
            if REPORT['worker']['enqueue_to_observed_completion_ms']['p99'] > 500:
                failures.append('worker p99 exceeds 500 ms')
            REPORT['failures'] = failures
            REPORT['result'] = 'FAIL' if failures else 'PASS'
            output = ROOT / args.output
            output.parent.mkdir(parents=True, exist_ok=True)
            for path in REPORT['paths'].values():
                for row in path['samples']:
                    receipt = row.pop('body')
                    row['receipt'] = {k: receipt[k] for k in ['attempts', 'prediction_cache_hit', 'disposition']}
            output.write_text(json.dumps(REPORT, indent=2) + '\n')
            print(json.dumps({k: v for k, v in REPORT.items() if k != 'paths'}, indent=2), flush=True)
            assert not failures, failures
    except BaseException:
        for log in logs:
            log.flush()
            log.seek(0)
            print(log.read(), flush=True)
        raise
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
        for connection in connections:
            connection.close()
        for log in logs:
            log.close()
        sql(f'DROP ROLE IF EXISTS {API}; DROP ROLE IF EXISTS {WORKER};')
