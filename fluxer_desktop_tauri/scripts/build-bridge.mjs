// SPDX-License-Identifier: AGPL-3.0-or-later

import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {build} from 'esbuild';

const ROOT_DIR = path.dirname(path.dirname(fileURLToPath(import.meta.url)));

await build({
	entryPoints: [path.join(ROOT_DIR, 'bridge', 'index.ts')],
	outfile: path.join(ROOT_DIR, 'dist', 'bridge.js'),
	bundle: true,
	format: 'iife',
	platform: 'browser',
	target: ['es2022', 'safari16'],
	minify: process.env.NODE_ENV === 'production',
	legalComments: 'none',
	logLevel: 'info',
});
