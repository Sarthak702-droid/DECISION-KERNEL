#!/usr/bin/env python3
"""Bind a passing staged drill report to the exact candidate registry manifest.
Review and sign/archive output through your deployment approval system before applying.
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
candidate=json.loads(Path(sys.argv[1]).read_text())
evidence=Path(sys.argv[2])
manifest={**candidate,'schema_version':'kernel-deployment/v1','evidence_sha256':hashlib.sha256(evidence.read_bytes()).hexdigest()}
output=Path(sys.argv[3])
output.write_text(json.dumps(manifest,indent=2)+'\n')
subprocess.run([sys.executable,str(Path(__file__).with_name('deploy-release.py')),str(output),'--evidence',str(evidence),'--validate-only'],check=True)
