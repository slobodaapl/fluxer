#!/usr/bin/env python3
"""Generate short-lived local test tokens and run synthetic media exchange.
Start a disposable server first, bound to loopback:
  podman run --rm --network host docker.io/livekit/livekit-server --dev --bind 127.0.0.1 --node-ip 127.0.0.1
No Fluxer account, microphone, display, or real user media is used.
"""
import base64
import hashlib
import hmac
import json
import os
from pathlib import Path
import subprocess
import time
import uuid

native = Path(__file__).resolve().parents[1]
env = os.environ.copy()
env.setdefault('FLUXER_MEDIA_TEST_URL', 'ws://127.0.0.1:7880')
room = 'fluxer-native-test-' + uuid.uuid4().hex
key = env.get('FLUXER_MEDIA_TEST_API_KEY', 'devkey')
secret = env.get('FLUXER_MEDIA_TEST_API_SECRET', 'secret').encode()
def encode(value):
    return base64.urlsafe_b64encode(json.dumps(value, separators=(',', ':')).encode()).rstrip(b'=')
def token(identity):
    now = int(time.time())
    payload = {'iss': key, 'sub': identity, 'nbf': now-10, 'exp': now+120,
               'video': {'roomJoin': True, 'room': room, 'canPublish': True, 'canSubscribe': True}}
    signing = encode({'alg': 'HS256', 'typ': 'JWT'}) + b'.' + encode(payload)
    signature = base64.urlsafe_b64encode(hmac.new(secret, signing, hashlib.sha256).digest()).rstrip(b'=')
    return (signing + b'.' + signature).decode()
env['FLUXER_MEDIA_TEST_TOKEN_A'] = token('publisher')
env['FLUXER_MEDIA_TEST_TOKEN_B'] = token('receiver')
result = subprocess.run(['cargo', 'test', '--manifest-path', str(native/'Cargo.toml'), '-p',
                         'fluxer-media', '--test', 'loopback', '--', '--ignored'], env=env)
raise SystemExit(result.returncode)
