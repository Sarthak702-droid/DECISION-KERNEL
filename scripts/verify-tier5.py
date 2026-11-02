#!/usr/bin/env python3
"""Isolated production-operations drills. Never point at a customer database.
Requires migrated KERNEL_TEST_DATABASE_URL and KERNEL_TEST_PSQL_CONTAINER.
Image drill requires --images and --baseline-images manifests from package-images.py.
"""
import argparse
import concurrent.futures
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

ROOT=Path(__file__).resolve().parents[1]
BIN=ROOT/'target/debug'
P=argparse.ArgumentParser(description=__doc__)
P.add_argument('--images',type=Path)
P.add_argument('--baseline-images',type=Path)
P.add_argument('--prior-tier-evidence',type=Path)
A=P.parse_args()
URL=os.environ['KERNEL_TEST_DATABASE_URL']
CONTAINER=os.environ['KERNEL_TEST_PSQL_CONTAINER']
PARTS=urllib.parse.urlsplit(URL)
DB=PARTS.path.lstrip('/')
ENV={k:v for k,v in os.environ.items() if not k.startswith('KERNEL_')}
SUFFIX=secrets.token_hex(6)
TENANT='tier5-'+SUFFIX
API='t5api_'+SUFFIX
WORKER='t5worker_'+SUFFIX
PASSWORD=secrets.token_hex(24)
TOKEN=secrets.token_hex(32)
METRICS=secrets.token_hex(32)
SCOPE={'KERNEL_TENANT_ID':TENANT,'KERNEL_PROJECT_ID':'project','KERNEL_ENVIRONMENT':'test','KERNEL_PRODUCT_ID':'product'}
CLI={**ENV,**SCOPE,'DATABASE_URL':URL}
OPS={**CLI,'KERNEL_OPS_PSQL_CONTAINER':CONTAINER,'PGDATABASE':DB}
OUT=ROOT/'audit/2026-11-02-tier5'
OUT.mkdir(parents=True,exist_ok=True)
REPORT={'synthetic':True,'checks':[],'image_drill':False,'production_qualified':False,'prior_tiers_passed':os.environ.get('KERNEL_PRIOR_TIERS_PASSED')=='1','tested_commit':os.environ.get('GITHUB_SHA')}
if A.prior_tier_evidence:
    prior=A.prior_tier_evidence.read_bytes()
    previous_proof=json.loads(prior)
    REPORT['prior_tiers_passed']=previous_proof.get('passed') is True and previous_proof.get('prior_tiers_passed') is True
    REPORT['prior_tier_evidence_sha256']=hashlib.sha256(prior).hexdigest()
    REPORT['tested_commit']=previous_proof.get('tested_commit')

def run(args,env=None,input=None,check=True):
    r=subprocess.run([str(x) for x in args],env=env,input=input,capture_output=True,text=not isinstance(input,bytes))
    if check and r.returncode: raise AssertionError(f'{args[0]} failed: {r.stderr}')
    return r

def sql(statement,database=DB):
    return run(['docker','exec','-i',CONTAINER,'psql','-U','postgres','-d',database,'-X','-q','-A','-t','-v','ON_ERROR_STOP=1'],input=statement).stdout.strip()

def cli(*args,check=True): return run([BIN/'kernelctl',*args],env=CLI,check=check)
def ops(command,*args,check=True): return run(['python3',ROOT/'scripts/kernel-ops.py',command,*args],env=OPS,check=check)
def check(name): REPORT['checks'].append(name);print('PASS '+name,flush=True)
def port():
    with socket.socket() as sock: sock.bind(('127.0.0.1',0));return sock.getsockname()[1]
def login(role):return urllib.parse.urlunsplit(PARTS._replace(netloc=f'{role}:{PASSWORD}@{PARTS.hostname}:{PARTS.port}'))
def http(base,path='/readyz',body=None,token=TOKEN):
    headers={'Authorization':'Bearer '+token,'Content-Type':'application/json','X-Request-Id':'tier5-drill','X-Correlation-Id':'tier5-trace'}
    if body: headers['Idempotency-Key']=body['idempotency_key']
    req=urllib.request.Request(base+path,headers=headers,data=json.dumps(body).encode() if body else None)
    try:r=urllib.request.urlopen(req,timeout=5)
    except urllib.error.HTTPError as e:r=e
    except (OSError,urllib.error.URLError):return 0,None
    with r:
        raw=r.read().decode()
        return r.status,json.loads(raw) if raw.startswith('{') or raw.startswith('[') else raw

def wait(predicate):
    until=time.monotonic()+30
    while time.monotonic()<until:
        if predicate():return
        time.sleep(.1)
    raise AssertionError('condition timeout')

with tempfile.TemporaryDirectory(prefix='kernel-tier5-') as temp:
    temp=Path(temp);processes=[];containers=[];logs=[]
    try:
        sql(f"CREATE ROLE {API} LOGIN PASSWORD '{PASSWORD}' NOSUPERUSER NOBYPASSRLS;GRANT kernel_runtime TO {API};CREATE ROLE {WORKER} LOGIN PASSWORD '{PASSWORD}' NOSUPERUSER NOBYPASSRLS;GRANT kernel_worker TO {WORKER};")
        cli('register-token',check=False) # Missing provisioning token must fail safely.
        run([BIN/'kernelctl','register-token'],env={**CLI,'KERNEL_NEW_SERVICE_TOKEN':TOKEN})
        pack={'api_version':'kernel/v1','kind':'DecisionPack','id':'ops.drill','version':'1',
              'required_evidence':[],'outputs':[{'kind':'binary','id':'ok','proposition':'ok'}],
              'nodes':[{'id':'deny','op':'fact_equals','field':'blocked','value':True},{'id':'yes','op':'literal_answer','output':'ok','answer':{'kind':'binary','proposition':'ok','value':True,'probability_true':None}}],
              'policy':{'hard_deny_nodes':['deny'],'accept_deterministic':True,'accept_qualified_semantic':False},
              'limits':{'deadline_ms':1000,'max_billable_attempts':1,'max_cost_nano_usd':1000},'qualification':{'semantic_acceptance_qualified':False}}
        baseline=temp/'baseline.json';baseline.write_text(json.dumps(pack));cli('publish',baseline);cli('promote','ops.drill','1','0')
        candidate=copy.deepcopy(pack);candidate['version']='2';candidate['policy']['accept_deterministic']=False
        cand=temp/'candidate.json';cand.write_text(json.dumps(candidate));cli('publish',cand)
        bases=[]
        for i in range(2):
            address=f'127.0.0.1:{port()}';bases.append('http://'+address)
            log=open(temp/f'api{i}.log','w+');logs.append(log)
            processes.append(subprocess.Popen([BIN/'kerneld'],env={**ENV,'DATABASE_URL':login(API),'KERNEL_LISTEN_ADDR':address,'KERNEL_METRICS_TOKEN':METRICS},stdout=log,stderr=log))
        for base in bases:wait(lambda:http(base)[0]==200)
        def body(key,profile='fast'):return {'schema_version':'1.1','product_id':'product','decision_type':'ops.drill','release':'active','idempotency_key':key,'mode':'evaluate','execution_profile':profile,'state':{'blocked':False},'evidence_refs':[],'constraints':{}}
        assert http(bases[0],'/metrics',token='invalid')[0]==401
        assert 'kernel_http_requests_total' in http(bases[0],'/metrics',token=METRICS)[1]
        assert http(bases[0],'/v1/decisions',body('stable'))[1]['disposition']=='accept'
        assert sql(f"SELECT count(*) FROM recorded_requests WHERE tenant_id='{TENANT}';")=='1'
        check('separate non-owner API logins, readiness, protected metrics and exact request capture')
        sql(f"INSERT INTO operation_limits VALUES ('{TENANT}','project','test','product',3); DELETE FROM operation_windows WHERE tenant_id='{TENANT}';")
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            statuses=list(pool.map(lambda i:http(bases[i%2],'/v1/decisions',body('limit-'+str(i)))[0],range(8)))
        assert statuses.count(200)==3 and statuses.count(429)==5,statuses
        sql(f"UPDATE operation_limits SET requests_per_minute=6000 WHERE tenant_id='{TENANT}';")
        check('atomic rate limit shared across two API replicas: exactly 3 admitted, 5 rejected')
        assert cli('promote','ops.drill','2','1').returncode==0
        assert http(bases[0],'/v1/decisions',body('candidate'))[1]['disposition']=='review'
        assert cli('rollback','ops.drill','1','1',check=False).returncode!=0
        cli('rollback','ops.drill','1','2')
        assert http(bases[1],'/v1/decisions',body('rollback'))[1]['disposition']=='accept'
        assert http(bases[1],'/v1/decisions',body('stable'))[1]['release']=='ops.drill@1'
        check('candidate promotion, stale revision rejection, pack rollback and pinned receipt replay')
        worker_port=port();worker_log=open(temp/'worker.log','w+');logs.append(worker_log)
        worker=subprocess.Popen([BIN/'kernel-worker'],env={**ENV,'DATABASE_URL':login(WORKER),'KERNEL_WORKER_HEALTH_ADDR':f'127.0.0.1:{worker_port}','KERNEL_METRICS_TOKEN':METRICS},stdout=worker_log,stderr=worker_log);processes.append(worker)
        wait(lambda:http(f'http://127.0.0.1:{worker_port}')[0]==200)
        assert 'kernel_attempts_unresolved' in http(f'http://127.0.0.1:{worker_port}','/metrics',token=METRICS)[1]
        assert http(bases[0],'/v1/decision-jobs',body('deferred','deferred'))[0]==202
        wait(lambda:sql(f"SELECT status FROM decision_jobs WHERE tenant_id='{TENANT}' AND idempotency_key='deferred';")=='completed')
        worker.terminate();worker.wait(timeout=10)
        check('worker readiness and deferred execution with distinct worker database credential')
        export=temp/'export.json';ops('export','--output',export)
        report=cli('compare-stored',baseline,cand,export)
        comparison=json.loads(report.stdout);assert comparison['compared']>=5 and comparison['changed']==comparison['compared']
        assert comparison['semantic_qualification_granted'] is False
        (OUT/'comparison.json').write_text(report.stdout)
        REPORT['comparison_sha256']=hashlib.sha256(report.stdout.encode()).hexdigest()
        tampered=json.loads(export.read_text());tampered['data']['recorded_requests'][0]['request']['state']['blocked']=True
        bad=temp/'tampered.json';bad.write_text(json.dumps(tampered))
        assert cli('compare-stored',baseline,cand,bad,check=False).returncode!=0
        semantic=copy.deepcopy(candidate);semantic['semantic_state_fields']=['new_input'];changed=temp/'semantic.json';changed.write_text(json.dumps(semantic))
        assert cli('compare-stored',baseline,changed,export,check=False).returncode!=0
        check('stored-request digest validation, frozen-time candidate comparison and semantic contract change rejection')
        sql(f"INSERT INTO decision_reviews (tenant_id,project_id,environment,product_id,idempotency_key,reviewer_id,disposition,notes) VALUES ('{TENANT}','project','test','product','stable','reviewer:drill','review','customer $$ note'); INSERT INTO decision_outcomes (tenant_id,project_id,environment,product_id,idempotency_key,outcome) VALUES ('{TENANT}','project','test','product','stable','pending');")
        nonowner=run(['python3',ROOT/'scripts/kernel-ops.py','export','--output',temp/'forbidden-export.json'],env={**OPS,'PGUSER':API},check=False)
        assert nonowner.returncode!=0 and 'owner maintenance credential required' in nonowner.stderr
        for product in ['product','other-product']:
            sql(f"INSERT INTO prediction_cache (tenant_id,project_id,environment,product_id,cache_key_sha256,semantic_response,expires_at_unix_ms) VALUES ('{TENANT}','project','test','{product}','expired','{{}}',1);")
        ops('retention')
        assert sql(f"SELECT count(*) FROM prediction_cache WHERE tenant_id='{TENANT}' AND product_id='product';")=='0'
        assert sql(f"SELECT count(*) FROM prediction_cache WHERE tenant_id='{TENANT}' AND product_id='other-product';")=='1'
        check('owner-only lifecycle access, expired cache retention and cross-product isolation')
        # Financial hold survives a real custom-format backup and restore.
        sql(f"INSERT INTO budget_buckets (tenant_id,project_id,environment,product_id,purpose,cap_nano_usd,held_nano_usd) VALUES ('{TENANT}','project','test','product','ops.drill',100,80); INSERT INTO provider_attempts (tenant_id,project_id,environment,product_id,idempotency_key,attempt_no,purpose,reserved_nano_usd,status,provider_id,model_id) VALUES ('{TENANT}','project','test','product','uncertain',1,'ops.drill',80,'uncertain','fixture','fixture');")
        restore='tier5_restore_'+SUFFIX
        started=time.monotonic()
        dump=subprocess.run(['docker','exec',CONTAINER,'pg_dump','-U','postgres','-d',DB,'-Fc'],capture_output=True,check=True).stdout
        sql('CREATE DATABASE '+restore+';')
        run(['docker','exec','-i',CONTAINER,'pg_restore','-U','postgres','-d',restore,'--exit-on-error'],input=dump)
        assert sql(f"SELECT held_nano_usd FROM budget_buckets WHERE tenant_id='{TENANT}';",restore)=='80'
        assert sql(f"SELECT status FROM provider_attempts WHERE tenant_id='{TENANT}';",restore)=='uncertain'
        for table in ['decision_receipts','recorded_requests','release_pointers','outbox_events']:
            select=f"SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]') FROM {table} t WHERE tenant_id='{TENANT}';"
            assert sql(select)==sql(select,restore),table
        restore_api_port,restore_worker_port=port(),port()
        restore_processes=[]
        for binary,role,var,address in [('kerneld',API,'KERNEL_LISTEN_ADDR',f'127.0.0.1:{restore_api_port}'),('kernel-worker',WORKER,'KERNEL_WORKER_HEALTH_ADDR',f'127.0.0.1:{restore_worker_port}')]:
            log=open(temp/(binary+'-restore.log'),'w+');logs.append(log)
            restored_url=urllib.parse.urlunsplit(urllib.parse.urlsplit(login(role))._replace(path='/'+restore))
            process=subprocess.Popen([BIN/binary],env={**ENV,'DATABASE_URL':restored_url,var:address},stdout=log,stderr=log)
            processes.append(process);restore_processes.append(process)
        wait(lambda:http(f'http://127.0.0.1:{restore_api_port}')[0]==200 and http(f'http://127.0.0.1:{restore_worker_port}')[0]==200)
        assert http(f'http://127.0.0.1:{restore_api_port}','/v1/decisions',body('stable'))[1]==http(bases[0],'/v1/decisions',body('stable'))[1]
        for process in restore_processes:process.terminate();process.wait(timeout=10)
        REPORT['restore']={'elapsed_seconds':round(time.monotonic()-started,3),'dump_sha256':hashlib.sha256(dump).hexdigest(),'bytes':len(dump),'preserved_uncertain_hold':80}
        sql('DROP DATABASE '+restore+';')
        check('pg_dump/pg_restore drill: receipts, inputs, release pointers, outbox and uncertain financial holds preserved')
        if A.images:
            images=json.loads(A.images.read_text())['images'];previous=json.loads(A.baseline_images.read_text())['images'] if A.baseline_images else images
            if A.baseline_images: assert images['kerneld']!=previous['kerneld'],'distinct baseline/candidate API images required'
            run(['docker','run','--rm','--network','host','--read-only','--cap-drop','ALL','-e','DATABASE_URL='+URL,images['kernelctl'],'migrate'])
            run(['docker','run','--rm','--network','host','--read-only','--cap-drop','ALL','-e','DATABASE_URL='+URL,images['kernelctl'],'migrate'])
            image_port=port();base=f'http://127.0.0.1:{image_port}'
            receipt=http(bases[0],'/v1/decisions',body('image-replay'))[1]
            timeline=[]
            for version,manifest in ([('baseline',previous),('candidate',images),('rollback',previous)] if A.baseline_images else [('candidate-smoke',images)]):
                names=[]
                for binary,role,health_var,address in [('kerneld',API,'KERNEL_LISTEN_ADDR',f'127.0.0.1:{image_port}'),('kernel-worker',WORKER,'KERNEL_WORKER_HEALTH_ADDR',f'127.0.0.1:{worker_port}')]:
                    name='tier5-'+binary+'-'+SUFFIX
                    run(['docker','run','-d','--name',name,'--network','host','--read-only','--cap-drop','ALL','--security-opt','no-new-privileges:true','--memory','512m','--pids-limit','128','-e','DATABASE_URL='+login(role),'-e',health_var+'='+address,manifest[binary]])
                    names.append(name);containers.append(name)
                wait(lambda:http(base)[0]==200 and http(f'http://127.0.0.1:{worker_port}')[0]==200)
                assert http(base,'/v1/decisions',body('image-replay'))[1]==receipt
                for name in names:
                    assert run(['docker','inspect','--format','{{.Config.User}}',name]).stdout.strip()=='65532:65532'
                    run(['docker','exec',name,'/usr/local/bin/'+('kernel-worker' if 'kernel-worker' in name else 'kerneld'),'--check-ready'])
                assert http(base,'/v1/decisions',body('image-fresh-'+version))[1]['disposition']=='accept'
                assert http(base,'/v1/decision-jobs',body('image-job-'+version,'deferred'))[0]==202
                wait(lambda:sql(f"SELECT status FROM decision_jobs WHERE tenant_id='{TENANT}' AND idempotency_key='image-job-{version}';")=='completed')
                timeline.append({'phase':version,'images':manifest,'receipt_id':receipt['receipt_id'],'ready':True})
                for name in names:run(['docker','rm','-f',name]);containers.remove(name)
            REPORT['image_drill']=timeline
            manifest=json.loads(A.images.read_text())
            REPORT['release_commit']=manifest.get('commit')
            if REPORT.get('tested_commit')!=manifest.get('commit'): REPORT['prior_tiers_passed']=False
            REPORT['deployment_images']=images
            check('read-only non-root API/worker images: '+('baseline → candidate → rollback' if A.baseline_images else 'candidate smoke')+'; readiness and exact receipt preserved')
        ops('block')
        assert http(bases[0],'/v1/decisions',body('blocked'))[0] in [401,410]
        for process in processes:
            if process.poll() is None:process.terminate();process.wait(timeout=10)
        blocked_export=temp/'blocked-export.json';exported=json.loads(ops('export','--output',blocked_export).stdout)
        assert ops('delete','--output',blocked_export,'--export-sha256',exported['sha256'],check=False).returncode!=0
        check('durable lifecycle block and deletion refusal while financial holds remain')
        # Reconciliation uses the existing administrative path and retains invoice reference.
        cli('reconcile-attempt','uncertain','20','synthetic-invoice:tier5')
        # An old export must fail even after holds are resolved.
        assert ops('delete','--output',blocked_export,'--export-sha256',exported['sha256'],check=False).returncode!=0
        fresh=temp/'final-export.json';digest=json.loads(ops('export','--output',fresh).stdout)['sha256']
        ops('delete','--output',fresh,'--export-sha256',digest)
        assert sql(f"SELECT count(*) FROM decision_receipts WHERE tenant_id='{TENANT}';")=='0'
        assert sql(f"SELECT count(*) FROM lifecycle_blocks WHERE tenant_id='{TENANT}';")=='1'
        check('stale export rejection; reconciled scope deletion removes inputs/receipts while durable block survives')
        # Test deployment validation without contacting a registry or deployment host.
        deployment_images={binary:'example.test/kernel/'+binary+'@sha256:'+('a'*64) for binary in ['kerneld','kernel-worker','kernelctl']}
        evidence=temp/'deployment-evidence.json'
        evidence.write_text(json.dumps({'passed':True,'prior_tiers_passed':True,'image_drill':[{'phase':'candidate-smoke'}],'release_commit':'b'*40,'deployment_images':deployment_images}))
        manifest=temp/'deployment.json'
        payload={'schema_version':'kernel-deployment/v1','images':deployment_images,'commit':'b'*40,'schema_min':17,'schema_max':17,'evidence_sha256':hashlib.sha256(evidence.read_bytes()).hexdigest()}
        manifest.write_text(json.dumps(payload))
        command=['python3',ROOT/'scripts/deploy-release.py',manifest,'--evidence',evidence,'--validate-only']
        assert run(command).returncode==0
        invalid=copy.deepcopy(payload);invalid['images']['kerneld']='example.test/kernel/kerneld:latest';manifest.write_text(json.dumps(invalid))
        assert run(command,check=False).returncode!=0
        invalid=copy.deepcopy(payload);invalid['evidence_sha256']='0'*64;manifest.write_text(json.dumps(invalid))
        assert run(command,check=False).returncode!=0
        invalid=copy.deepcopy(payload);invalid['commit']='c'*40;manifest.write_text(json.dumps(invalid))
        assert run(command,check=False).returncode!=0
        check('deployment validation binds immutable registry images, exact commit and actual evidence bytes')
        REPORT['passed']=True
        (OUT/'results.json').write_text(json.dumps(REPORT,indent=2)+'\n')
    finally:
        for process in processes:
            if process.poll() is None:process.terminate();process.wait(timeout=10)
        for name in containers:run(['docker','rm','-f',name],check=False)
        for log in logs:
            log.seek(0)
            content=log.read()
            assert PASSWORD not in content and TOKEN not in content and METRICS not in content,'secret in process logs'
            log.close()
