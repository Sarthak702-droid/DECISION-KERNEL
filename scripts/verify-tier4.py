#!/usr/bin/env python3
"""Tier 4 developer onboarding and application-consumed receipt proof.
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
API, WORKER = 'tier4_api_' + SUFFIX, 'tier4_worker_' + SUFFIX
PASSWORD, TOKEN = secrets.token_hex(24), secrets.token_hex(24)
TENANT = 'tier4-' + SUFFIX
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


from tier0_provider_fixture import HTTPSFixture

CONTRACT = json.loads((ROOT / 'api/openapi.json').read_text())
if os.environ.get('KERNEL_VALIDATE_OPENAPI'):
    from openapi_spec_validator import validate
    from jsonschema import Draft202012Validator
    validate(CONTRACT)

def validate_response(method, path, status, data):
    if not os.environ.get('KERNEL_VALIDATE_OPENAPI'):
        return
    import re
    for template, operations in CONTRACT['paths'].items():
        pattern = re.sub(r'\{[^}]+\}', '[^/]+', template)
        if re.fullmatch(pattern, path):
            response = operations[method.lower()]['responses'][str(status)]
            if 'content' in response:
                schema = response['content']['application/json']['schema']
                Draft202012Validator({**schema, 'components': CONTRACT['components']}).validate(data)
            return

with tempfile.TemporaryDirectory(prefix='kernel-tier4-') as directory:
    processes, logs = [], []
    try:
        sql(f"CREATE ROLE {API} LOGIN PASSWORD '{PASSWORD}' NOSUPERUSER NOBYPASSRLS; GRANT kernel_runtime TO {API};")
        reviewer, admin, app_token = secrets.token_hex(32), secrets.token_hex(32), secrets.token_hex(32)
        for product,token in [('support-suite',TOKEN),('other-product','other-'+TOKEN)]:
            sql(f"INSERT INTO service_tokens (token_hash_sha256,tenant_id,project_id,environment,product_id) VALUES ('{hashlib.sha256(token.encode()).hexdigest()}','{TENANT}','test','test','{product}');")
        cli_env = {**ENV,'DATABASE_URL':OWNER,'KERNEL_TENANT_ID':TENANT,'KERNEL_PROJECT_ID':'test','KERNEL_ENVIRONMENT':'test','KERNEL_PRODUCT_ID':'support-suite'}
        subprocess.run([str(BIN/'kernelctl'),'register-reviewer'],env={**cli_env,'KERNEL_NEW_REVIEWER_TOKEN':reviewer,'KERNEL_REVIEWER_ID':'reviewer:alice'},check=True,capture_output=True)
        for purpose in ['support.triage','connection.test']:
            sql(f"INSERT INTO budget_buckets (tenant_id,project_id,environment,product_id,purpose,cap_nano_usd) VALUES ('{TENANT}','test','test','support-suite','{purpose}',10000000);")
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',0)); app_port=sock.getsockname()[1]
        base=f'http://127.0.0.1:{port}'
        with HTTPSFixture(directory) as fixture:
            fixture.response_model='served-model'
            def answers(semantic):
                message=semantic.get('evidence',{}).get('latest_message',{}).get('body','').lower()
                # A transport fixture, not an evaluated intelligence model.
                return {q['id']:({'kind':'categorical','selected_id':'technical','probabilities':None} if q['output']['kind']=='categorical' else {'kind':'binary','proposition':q['output']['proposition'],'value':'urgent' in message or not message,'probability_true':None}) for q in semantic['questions']}
            fixture.answer_factory=answers
            login=urllib.parse.urlunsplit(PARTS._replace(netloc=f'{API}:{PASSWORD}@{PARTS.hostname}:{PARTS.port or 5432}'))
            env={**ENV,'DATABASE_URL':login,'KERNEL_LISTEN_ADDR':f'127.0.0.1:{port}','KERNEL_CONNECTION_MASTER_KEY':secrets.token_hex(32),'KERNEL_CONNECTION_ADMIN_TOKEN':admin,'KERNEL_PROVIDER_ALLOWED_HOSTS':'127.0.0.1','KERNEL_PROVIDER_CA_CERT_PATH':fixture.environment['KERNEL_PROVIDER_CA_CERT_PATH']}
            log=open(Path(directory)/'api.log','w+');logs.append(log)
            processes.append(subprocess.Popen([str(BIN/'kerneld')],env=env,stdout=log,stderr=log))
            def http(method,path,body=None,review=False,other=False):
                headers={'Authorization':'Bearer '+('other-'+TOKEN if other else TOKEN),'Content-Type':'application/json','X-Kernel-Admin-Token':admin}
                if review:headers['X-Kernel-Reviewer-Token']=reviewer
                req=urllib.request.Request(base+path,method=method,headers=headers,data=json.dumps(body).encode() if body is not None else None)
                try:r=urllib.request.urlopen(req,timeout=5)
                except urllib.error.HTTPError as e:r=e
                except urllib.error.URLError:return 0,None
                with r:
                    raw=r.read()
                    if path in ['/console','/console.js']:return r.status,raw.decode()
                    data=json.loads(raw) if raw else None
                    validate_response(method,path,r.status,data)
                    return r.status,data
            wait_for(lambda:http('GET','/readyz')[0]==200)
            assert http('GET','/console')[0]==200 and 'Provider connections' in http('GET','/console')[1]
            assert http('GET','/console.js')[0]==200
            schema=http('GET','/openapi.json')[1];assert schema['openapi']=='3.1.0'
            pack=json.loads((ROOT/'packs/examples/support-triage-byok.json').read_text());pack['qualification']['model_id']='requested-model'
            pack_file=Path(directory)/'pack.json';pack_file.write_text(json.dumps(pack))
            for args in [('validate',str(pack_file)),('publish',str(pack_file)),('promote','support.triage','1.2.0','0')]:
                subprocess.run([str(BIN/'kernelctl'),*args],env=cli_env,check=True,capture_output=True)
            client_env={**ENV,'KERNEL_URL':base,'KERNEL_TOKEN':TOKEN,'KERNEL_ADMIN_TOKEN':admin,'KERNEL_REVIEWER_TOKEN':reviewer,'KERNEL_PRODUCT_ID':'support-suite','PROVIDER_API_KEY':'synthetic-tier4-key','PROVIDER_BASE_URL':fixture.environment['KERNEL_PROVIDER_BASE_URL'],'PROVIDER_MODEL':'requested-model','PROVIDER_RETURNED_MODELS':'served-model','PROVIDER_MAX_DISPATCH_NANO_USD':'100'}
            connected=subprocess.run(['node','examples/support-triage/connect.ts'],cwd=ROOT,env=client_env,capture_output=True,text=True)
            assert connected.returncode==0,connected.stderr
            assert fixture.auth_headers[-1]=='Bearer synthetic-tier4-key'
            assert http('GET','/v1/packs')[1][0]['active']
            check('docs onboarding: publish/promote, TypeScript BYOK create/probe/bind, packs and console')
            app_log=open(Path(directory)/'app.log','w+');logs.append(app_log)
            processes.append(subprocess.Popen(['node','examples/support-triage/server.ts'],cwd=ROOT,env={**client_env,'SUPPORT_APP_TOKEN':app_token,'SUPPORT_PORT':str(app_port)},stdout=app_log,stderr=app_log))
            def application(ticket):
                req=urllib.request.Request(f'http://127.0.0.1:{app_port}/tickets/triage',headers={'Authorization':'Bearer '+app_token,'Content-Type':'application/json'},data=json.dumps(ticket).encode())
                try:
                    with urllib.request.urlopen(req,timeout=5) as r:return json.load(r)
                except urllib.error.URLError:return None
            ticket={'ticket_id':'real-app-1','revision':1,'observed_at_unix_ms':int(time.time()*1000),'message':'Database unavailable; urgent production outage.'}
            result=wait_for(lambda:application(ticket))
            assert result['receipt']['answers']['queue']['selected_id']=='technical',result
            assert result['receipt']['attempts']==1 and result['receipt']['returned_model_id']=='served-model',result
            assert result['application']['action']=='human_review',result
            key='ticket-real-app-1-v1';path='/v1/decisions/'+key
            assert application(ticket)['receipt']['receipt_id']==result['receipt']['receipt_id']
            assert http('GET',path)[1]['receipt_id']==result['receipt']['receipt_id']
            assert http('GET',path,other=True)[0]==404
            check('real HTTP application consumes typed receipt, replays idempotently and respects product isolation')
            review={'disposition':'accept','reason_code':'queue_correction','corrections':{'queue':{'kind':'categorical','selected_id':'billing'}}}
            assert http('POST',path+'/reviews',review)[0]==403
            assert http('POST',path+'/reviews',review,review=True,other=True)[0]==403
            assert http('POST',path+'/reviews',{**review,'reviewer_id':'spoofed'},review=True)[0]==403
            invalid={**review,'corrections':{'queue':{'kind':'categorical','selected_id':'made_up'}}}
            assert http('POST',path+'/reviews',invalid,review=True)[0]==400
            assert http('POST',path+'/reviews',review,review=True)[0]==201
            history=http('GET',path+'/reviews')[1];assert history[0]['reviewer_id']=='reviewer:alice' and history[0]['corrections']['queue']['selected_id']=='billing'
            for state in ['pending','verified','disputed']:
                body={'outcome':'resolved','status':state}
                if state!='pending':
                    assert http('POST',path+'/outcomes',body,review=True)[0]==400
                    body['verification_ref']='audit:'+state
                    assert http('POST',path+'/outcomes',body)[0]==403
                assert http('POST',path+'/outcomes',body,review=state!='pending')[0]==201
            assert [r['status'] for r in http('GET',path+'/outcomes')[1]]==['disputed','verified','pending']
            assert http('GET',path)[1]==result['receipt']
            sql(f"UPDATE reviewer_tokens SET active=false WHERE token_hash_sha256='{hashlib.sha256(reviewer.encode()).hexdigest()}';")
            assert http('POST',path+'/reviews',review,review=True)[0]==403
            check('authenticated/revoked reviewer, typed corrections, proof-backed outcome states and immutable receipt')
            out=ROOT/'audit/2026-11-02-tier4';out.mkdir(parents=True,exist_ok=True)
            evaluated=subprocess.run(['node','examples/support-triage/evaluate.ts','datasets/support-triage-heldout.json',str(out/'synthetic-pilot.json')],env=client_env,cwd=ROOT,capture_output=True,text=True)
            assert evaluated.returncode==0,evaluated.stderr
            report=json.loads((out/'synthetic-pilot.json').read_text());assert report['summary']['completed']==48 and report['summary']['review_count']==48
            assert report['summary']['unknown_cost_count']==48 and report['summary']['provider_reported_cost_nano_usd']==3840 and report['summary']['attempts']==48,report['summary']
            assert report['summary']['queue_accuracy_all']==.25 and report['summary']['urgent_missed_or_unresolved']>0,report['summary']
            # Dataset leakage guard must fail before any provider request.
            bad=json.loads((ROOT/'datasets/support-triage-heldout.json').read_text());bad['rows'][1]['partition']='dev'
            bad_file=Path(directory)/'leaked.json';bad_file.write_text(json.dumps(bad))
            rejected=subprocess.run(['node','examples/support-triage/evaluate.ts',str(bad_file),str(Path(directory)/'bad-report.json')],env=client_env,cwd=ROOT,capture_output=True,text=True)
            assert rejected.returncode!=0 and 'Group leakage' in rejected.stderr
            check('held-out runner reports quality, review workload and ledger cost; rejects cross-partition group leakage')
            (out/'results.json').write_text(json.dumps({'checks':CHECKS,'synthetic':True,'external_provider_pilot':False,'openapi_validated':bool(os.environ.get('KERNEL_VALIDATE_OPENAPI')),'summary':report['summary']},indent=2)+'\n')
    finally:
        for process in reversed(processes):
            process.terminate()
            try:process.wait(timeout=5)
            except subprocess.TimeoutExpired:process.kill();process.wait()
        for log in logs:
            log.seek(0);text=log.read();assert 'synthetic-tier4-key' not in text,text
            if text:print(text[-2500:])
            log.close()
        sql(f"DROP OWNED BY {API}; DROP ROLE {API};")
