#!/usr/bin/env python3
"""Apply a reviewed digest-only manifest. Same command restores a prior manifest.
Migration runs separately with owner credentials; this process never receives them.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('manifest',type=Path)
p.add_argument('--evidence',type=Path,required=True)
p.add_argument('--validate-only',action='store_true')
a=p.parse_args()
m=json.loads(a.manifest.read_text())
if m.get('schema_version')!='kernel-deployment/v1': raise SystemExit('invalid deployment schema')
images=m['images']
for key in ['kerneld','kernel-worker','kernelctl']:
    if not re.fullmatch(r'[a-z0-9][a-z0-9./:_-]*@sha256:[0-9a-f]{64}',images[key]): raise SystemExit('registry digest required: '+key)
if not re.fullmatch('[0-9a-f]{40}',m.get('commit','')): raise SystemExit('full commit required')
if not re.fullmatch('[0-9a-f]{64}',m.get('evidence_sha256','')): raise SystemExit('qualification evidence digest required')
if m.get('schema_min')!=17 or m.get('schema_max')!=17: raise SystemExit('review schema compatibility before deploying')
import hashlib
evidence_bytes=a.evidence.read_bytes()
if hashlib.sha256(evidence_bytes).hexdigest()!=m['evidence_sha256']: raise SystemExit('evidence digest mismatch')
evidence=json.loads(evidence_bytes)
if not evidence.get('prior_tiers_passed'): raise SystemExit('passing Tier 0-4 evidence for this commit required')
if not evidence.get('passed') or not evidence.get('image_drill'): raise SystemExit('passing recovery and image evidence required')
if evidence.get('release_commit')!=m['commit']: raise SystemExit('evidence must bind the exact release commit')
if evidence.get('deployment_images')!=images: raise SystemExit('evidence must bind all exact registry digests')
if not a.validate_only:
    env={**os.environ,'KERNEL_API_IMAGE':images['kerneld'],'KERNEL_WORKER_IMAGE':images['kernel-worker']}
    subprocess.run(['docker','compose','-p','decision-kernel','-f',str(Path(__file__).resolve().parents[1]/'deploy/compose.yaml'),'up','-d','--wait','--wait-timeout','60'],env=env,check=True)
print(json.dumps({'validated':True,'applied':not a.validate_only,'commit':m['commit']}))
