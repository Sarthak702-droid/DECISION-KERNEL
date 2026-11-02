"""Local HTTPS provider fixture used only by the Tier 0 integration proof."""
import collections
import http.server
import json
from pathlib import Path
import ssl
import subprocess
import threading
import time


class HTTPSFixture:
    def __init__(self, directory):
        self.directory = Path(directory)
        self.calls = collections.Counter()
        self.lock = threading.Lock()
        self.server = None
        self.response_model = 'test-model'
        self.answer_factory = None
        self.auth_headers = []
        self.delay_seconds = 0
        self.keep_alive = False
        self.connections = set()
        self.active = 0
        self.max_active = 0

    def __enter__(self):
        root = self.directory / 'root.pem'
        root_key = self.directory / 'root.key'
        certificate = self.directory / 'server.pem'
        key = self.directory / 'server.key'
        csr = self.directory / 'server.csr'
        extensions = self.directory / 'server.ext'
        extensions.write_text('subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n')
        commands = [
            ['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', str(root_key), '-out', str(root), '-days', '1', '-subj', '/CN=Tier0 Test CA', '-addext', 'basicConstraints=critical,CA:TRUE'],
            ['openssl', 'req', '-newkey', 'rsa:2048', '-nodes', '-keyout', str(key), '-out', str(csr), '-subj', '/CN=localhost'],
            ['openssl', 'x509', '-req', '-in', str(csr), '-CA', str(root), '-CAkey', str(root_key), '-CAcreateserial', '-out', str(certificate), '-days', '1', '-extfile', str(extensions)],
        ]
        for command in commands:
            subprocess.run(command, check=True, capture_output=True)
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1' if fixture.keep_alive else 'HTTP/1.0'

            def do_POST(self):
                with fixture.lock:
                    fixture.connections.add(self.client_address)
                    fixture.active += 1
                    fixture.max_active = max(fixture.max_active, fixture.active)
                try:
                    self.handle_prediction()
                finally:
                    with fixture.lock:
                        fixture.active -= 1

            def handle_prediction(self):
                assert self.path in ['/chat/completions', '/v1/predictions']
                payload = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                native = self.path == '/v1/predictions'
                semantic = payload if native else json.loads(payload['messages'][-1]['content'])
                case = semantic['state'].get('case', 'probe')
                with fixture.lock:
                    fixture.calls[case] += 1
                    fixture.auth_headers.append(self.headers.get('Authorization'))
                if fixture.delay_seconds:
                    time.sleep(fixture.delay_seconds)
                if case == 'slow':
                    time.sleep(2)
                answers = {q['id']: {'kind': 'binary', 'proposition': q.get('output', {}).get('proposition', q.get('proposition', 'ok')), 'value': True,
                                    'probability_true': None, 'probability_provenance': None}
                           for q in semantic['questions']}
                if fixture.answer_factory:
                    answers = fixture.answer_factory(semantic)
                cost = 150 if case == 'overspend' else 90 if case == 'invalid' else 80
                content = '{invalid-json' if case == 'invalid' else json.dumps({'answers': answers})
                raw = json.dumps({'model': fixture.response_model, 'cost_nano_usd': cost,
                                  'usage': {'input_tokens': 10, 'output_tokens': 2, 'debug_secret': self.headers.get('Authorization')},
                                  **({'predictions': {q['id']: {'type': 'noul', 'value': True} for q in semantic['questions']}} if native else {}),
                                  'choices': [{'finish_reason': 'stop', 'message': {'content': content}}]}).encode()
                try:
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json')
                    self.send_header('Content-Length', str(len(raw)))
                    self.end_headers()
                    self.wfile.write(raw)
                except (BrokenPipeError, ConnectionResetError, ssl.SSLError):
                    pass  # Expected when the test kills an in-flight API process.

            def log_message(self, *args):
                pass

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(str(certificate), str(key))
        self.server.socket = context.wrap_socket(self.server.socket, server_side=True)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.environment = {
            'KERNEL_PROVIDER_BASE_URL': f'https://127.0.0.1:{self.server.server_port}',
            'KERNEL_PROVIDER_ALLOWED_HOST': '127.0.0.1',
            'KERNEL_PROVIDER_CA_CERT_PATH': str(root),
            'KERNEL_PROVIDER_API_KEY': 'synthetic-test-token',
            'KERNEL_PROVIDER_ID': 'local-fixture',
            'KERNEL_PROVIDER_MODEL': 'test-model',
            'KERNEL_PROVIDER_ADAPTER': 'compatible',
            'KERNEL_PROVIDER_MAX_DISPATCH_NANO_USD': '100',
        }
        return self

    def count(self, case):
        with self.lock:
            return self.calls[case]

    def __exit__(self, *args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
