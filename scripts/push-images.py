#!/usr/bin/env python3
"""Release CI helper; no production deployment. Push images by full commit tag and record RepoDigests."""
import json
import os
from pathlib import Path
import subprocess
import sys
source=json.loads(Path(sys.argv[1]).read_text())
commit=os.environ['GITHUB_SHA']
repository=os.environ['GITHUB_REPOSITORY'].lower()
result={'schema_version':'kernel-candidate/v1','commit':commit,'schema_min':17,'schema_max':17,'images':{}}
for binary,identity in source['images'].items():
    tag=f'ghcr.io/{repository}/{binary}:{commit}'
    subprocess.run(['docker','tag',identity,tag],check=True)
    subprocess.run(['docker','push',tag],check=True)
    digests=json.loads(subprocess.check_output(['docker','image','inspect','--format','{{json .RepoDigests}}',tag],text=True))
    result['images'][binary]=next(d for d in digests if d.startswith(tag.rsplit(':',1)[0]+'@'))
Path(sys.argv[2]).write_text(json.dumps(result,indent=2)+'\n')
