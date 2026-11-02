#!/usr/bin/env python3
"""Keep the lockfile-only RSA advisory exception tied to the active graph."""
import subprocess

result = subprocess.run([
    'cargo', 'tree', '--locked', '--workspace', '--target', 'all', '--all-features',
    '--edges', 'normal,build,dev', '--prefix', 'none', '--format', '{p}',
], text=True, capture_output=True, check=True)
packages = {line.split()[0] for line in result.stdout.splitlines() if line.strip()}
forbidden = packages.intersection({'rsa', 'sqlx-mysql', 'sqlx-sqlite'})
if forbidden:
    raise SystemExit('Inactive-driver advisory exception is invalid: ' + ', '.join(sorted(forbidden)))
print('PASS PostgreSQL-only active graph; RSA/MySQL/SQLite remain unreachable')
