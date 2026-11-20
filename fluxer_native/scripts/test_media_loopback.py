#!/usr/bin/env python3
"""Generate short-lived local test tokens and run synthetic media exchange.
Start a disposable server first, bound to loopback:
  podman run --rm --network host docker.io/livekit/livekit-server --dev --bind 127.0.0.1 --node-ip 127.0.0.1
The default test uses no account, microphone, display, or real user media.
--audio-lifecycle exercises desktop devices and muted local joins; personal
microphone audio is never published or saved.
--microphone-pipeline publishes only its explicitly selected synthetic virtual
source; use test_microphone_capture.py to create and clean up that source.
--shared-audio-capture publishes only an isolated synthetic playback process;
use test_shared_audio_capture.py to create and clean up its virtual sinks.
"""
import argparse
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
parser = argparse.ArgumentParser(description=__doc__)
mode = parser.add_mutually_exclusive_group()
mode.add_argument('--audio-lifecycle', action='store_true',
                    help='Exercise muted MediaEngine joins using desktop audio devices; no personal audio is sent.')
mode.add_argument('--microphone-pipeline', action='store_true',
                  help='Exercise the explicitly selected synthetic microphone; use test_microphone_capture.py.')
mode.add_argument('--output-pipeline', action='store_true',
                  help='Exercise decoded playback to isolated sinks; use test_output_routing.py --media.')
mode.add_argument('--camera', action='store_true',
                  help='Exercise independent synthetic camera and screen subscriptions.')
mode.add_argument('--screen-watch', action='store_true',
                  help='Exercise explicit remote screen viewing; use test_output_routing.py --screen-watch.')
mode.add_argument('--shared-audio-capture', action='store_true',
                  help='Exercise actual isolated application-audio capture; use test_shared_audio_capture.py.')
args = parser.parse_args()
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
command = [env.get('FLUXER_MEDIA_TEST_CARGO', 'cargo'), 'test', '--manifest-path', str(native/'Cargo.toml'), '-p',
           'fluxer-media', '--test', 'camera_loopback' if args.camera else 'shared_audio_capture' if args.shared_audio_capture else 'screen_watch' if args.screen_watch else 'output_pipeline' if args.output_pipeline else 'microphone_pipeline' if args.microphone_pipeline else 'audio_lifecycle' if args.audio_lifecycle else 'loopback',
           '--', '--ignored']
if args.audio_lifecycle or args.microphone_pipeline or args.output_pipeline or args.screen_watch or args.shared_audio_capture:
    # PlatformAudio owns process-wide routing; hardware checks must be serial.
    command.append('--test-threads=1')
if args.output_pipeline or args.screen_watch or args.shared_audio_capture:
    command.append('--nocapture')
result = subprocess.run(command, env=env)
raise SystemExit(result.returncode)
