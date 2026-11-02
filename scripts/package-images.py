#!/usr/bin/env python3
"""Package already validated Linux release binaries and their exact shared libraries.
No mutable base images, package managers, compiler or shell in the runtime image.
Writes local image IDs; registry RepoDigests are recorded after push by CI.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

ROOT=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--prefix',default='decision-kernel')
parser.add_argument('--output',type=Path,required=True)
args=parser.parse_args()
manifest={'schema_version':'kernel-images/v1','images':{},'commit':os.environ.get('GITHUB_SHA')}
with tempfile.TemporaryDirectory(prefix='kernel-images-') as temp:
    context=Path(temp)
    for binary in ['kerneld','kernel-worker','kernelctl']:
        stage=context/binary
        stage.mkdir()
        executable=ROOT/'target/release'/binary
        if not executable.is_file(): raise SystemExit('cargo build --release --locked first')
        destination=stage/'usr/local/bin'/binary
        destination.parent.mkdir(parents=True)
        shutil.copy2(executable,destination)
        dependencies=subprocess.run(['ldd',str(executable)],capture_output=True,text=True,check=True).stdout
        if 'not found' in dependencies: raise SystemExit(dependencies)
        for lib in re.findall(r'(/[^\s()]+)',dependencies):
            target=stage/lib.lstrip('/')
            target.parent.mkdir(parents=True,exist_ok=True)
            shutil.copy2(lib,target)
        cert=stage/'etc/ssl/certs/ca-certificates.crt'
        cert.parent.mkdir(parents=True,exist_ok=True)
        shutil.copy2('/etc/ssl/certs/ca-certificates.crt',cert)
        health='' if binary=='kernelctl' else f'HEALTHCHECK --interval=10s --timeout=5s --start-period=10s --retries=3 CMD ["/usr/local/bin/{binary}","--check-ready"]\n'
        (context/'Dockerfile').write_text(f'FROM scratch\nCOPY {binary}/ /\nUSER 65532:65532\n{health}ENTRYPOINT ["/usr/local/bin/{binary}"]\n')
        tag=args.prefix+'/'+binary
        subprocess.run(['docker','build','--network=none','--tag',tag,str(context)],check=True)
        image_id=subprocess.check_output(['docker','image','inspect','--format','{{.Id}}',tag],text=True).strip()
        manifest['images'][binary]=image_id
args.output.parent.mkdir(parents=True,exist_ok=True)
args.output.write_text(json.dumps(manifest,indent=2)+'\n')
