// SPDX-License-Identifier: AGPL-3.0-or-later
const {createHash} = require('node:crypto');
const fs = require('node:fs/promises');
const path = require('node:path');

async function packageStreamr(appDir, resourcesDir, platform, arch) {
	if (arch !== 'x64' && arch !== 'universal') return;
	const directory = path.join(appDir, 'native', 'streamr', 'artifact');
	const manifest = JSON.parse(await fs.readFile(path.join(directory, 'manifest.json'), 'utf8'));
	if (manifest.platform !== platform || manifest.arch !== 'x64') {
		throw new Error(`Streamr helper does not match ${platform}/x64. Build it on the target platform first.`);
	}
	const name = platform === 'win32' ? 'streamr-native.exe' : 'streamr-native';
	const source = path.join(directory, name);
	if (createHash('sha256').update(await fs.readFile(source)).digest('hex') !== manifest.sha256) {
		throw new Error('Streamr helper hash mismatch. Rebuild before packaging.');
	}
	const models = path.join(appDir, 'streamr-models');
	const modelManifest = JSON.parse(await fs.readFile(path.join(models, 'manifest.json'), 'utf8'));
	for (const [file, expected] of Object.entries(modelManifest.files)) {
		const bytes = await fs.readFile(path.join(models, file));
		if (bytes.length !== expected.bytes || createHash('sha256').update(bytes).digest('hex') !== expected.sha256) {
			throw new Error(`Streamr model hash mismatch: ${file}`);
		}
	}
	await fs.mkdir(path.join(resourcesDir, 'streamr-native'), {recursive: true});
	await fs.copyFile(source, path.join(resourcesDir, 'streamr-native', name));
	await fs.chmod(path.join(resourcesDir, 'streamr-native', name), 0o755);
	await fs.copyFile(path.join(directory, 'manifest.json'), path.join(resourcesDir, 'streamr-native', 'manifest.json'));
	await fs.cp(models, path.join(resourcesDir, 'streamr-models'), {recursive: true});
	await fs.cp(path.join(appDir, 'native', 'streamr', 'licenses'), path.join(resourcesDir, 'streamr-native', 'licenses'), {recursive: true});
}
module.exports = {packageStreamr};
