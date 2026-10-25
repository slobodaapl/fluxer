// SPDX-License-Identifier: AGPL-3.0-or-later
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import fs from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath} from 'node:url';

const desktop = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const native = path.join(desktop, 'native', 'streamr');

export async function verifyStreamrModels() {
	const directory = path.join(desktop, 'streamr-models');
	const manifest = JSON.parse(await fs.readFile(path.join(directory, 'manifest.json'), 'utf8'));
	for (const [name, expected] of Object.entries(manifest.files)) {
		const bytes = await fs.readFile(path.join(directory, name));
		if (createHash('sha256').update(bytes).digest('hex') !== expected.sha256 || bytes.length !== expected.bytes) {
			throw new Error(`Streamr ${name} is missing or corrupt. Restore its Git LFS object before building.`);
		}
	}
}

export async function buildStreamr() {
	if ((process.env.ELECTRON_ARCH || process.arch) === 'arm64') {
		console.log('Streamr requires x86-64 AVX2; ARM64 build omits this option.');
		return;
	}
	if (process.arch !== 'x64') throw new Error('Build the Streamr x64 helper on an x64 host.');
	await verifyStreamrModels();
	const rust = execFileSync('rustc', ['-vV'], {cwd: native, encoding: 'utf8'});
	const host = /^host: (.+)$/m.exec(rust)?.[1];
	if (!host || (process.env.CARGO_BUILD_TARGET && process.env.CARGO_BUILD_TARGET !== host)) {
		throw new Error('Streamr helper must target the build host; use a matching platform build job.');
	}
	const buildOutput = execFileSync('cargo', ['build', '--release', '--locked', '--target', host, '--message-format=json-render-diagnostics'], {
		cwd: native, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'], maxBuffer: 16 * 1024 * 1024,
	});
	const artifact = buildOutput.trim().split('\n').map((line) => JSON.parse(line)).find(
		(message) => message.reason === 'compiler-artifact' && message.target?.name === 'streamr-native' && message.executable,
	);
	if (!artifact) throw new Error('Cargo did not report the Streamr executable.');
	const name = process.platform === 'win32' ? 'streamr-native.exe' : 'streamr-native';
	const source = artifact.executable;
	const output = path.join(native, 'artifact');
	await fs.mkdir(output, {recursive: true});
	await fs.copyFile(source, path.join(output, name));
	await fs.chmod(path.join(output, name), 0o755);
	const sha256 = createHash('sha256').update(await fs.readFile(source)).digest('hex');
	await fs.writeFile(path.join(output, 'manifest.json'), `${JSON.stringify({platform: process.platform, arch: 'x64', host, sha256}, null, 2)}\n`);
	console.log(`Streamr ${process.platform}/x64 helper built; model hashes verified.`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await buildStreamr();
