// SPDX-License-Identifier: AGPL-3.0-or-later
'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const {createHash} = require('node:crypto');
const {packageStreamr} = require('../../../scripts/streamr-package.cjs');

async function fixture(t) {
	const artifacts = path.resolve(__dirname, '../../../native/streamr/artifact');
	await fs.mkdir(artifacts, {recursive: true});
	const root = await fs.mkdtemp(path.join(artifacts, 'package-test-'));
	t.after(() => fs.rm(root, {recursive: true, force: true}));
	const app = path.join(root, 'app'), resources = path.join(root, 'resources');
	const native = path.join(app, 'native/streamr/artifact'), models = path.join(app, 'streamr-models');
	await fs.mkdir(native, {recursive: true}); await fs.mkdir(models, {recursive: true});
	await fs.mkdir(path.join(app, 'native/streamr/licenses'), {recursive: true});
	const helper = Buffer.from('test executable'), model = Buffer.from('test weights');
	const sha = data => createHash('sha256').update(data).digest('hex');
	await fs.writeFile(path.join(native, 'streamr-native'), helper);
	await fs.writeFile(path.join(native, 'manifest.json'), JSON.stringify({platform: 'linux', arch: 'x64', sha256: sha(helper)}));
	await fs.writeFile(path.join(models, 'model.nnef'), model);
	await fs.writeFile(path.join(models, 'LICENSE.txt'), 'model terms');
	await fs.writeFile(path.join(models, 'manifest.json'), JSON.stringify({files: {'model.nnef': {bytes: model.length, sha256: sha(model)}}}));
	await fs.writeFile(path.join(app, 'native/streamr/licenses/runner.txt'), 'runner terms');
	return {app, resources, native, models, helper, model};
}

test('packaged helper, model bytes and separate notices survive resource copying', async t => {
	const f = await fixture(t); await packageStreamr(f.app, f.resources, 'linux', 'x64');
	assert.deepEqual(await fs.readFile(path.join(f.resources, 'streamr-native/streamr-native')), f.helper);
	assert.deepEqual(await fs.readFile(path.join(f.resources, 'streamr-models/model.nnef')), f.model);
	assert.equal(await fs.readFile(path.join(f.resources, 'streamr-models/LICENSE.txt'), 'utf8'), 'model terms');
	assert.equal(await fs.readFile(path.join(f.resources, 'streamr-native/licenses/runner.txt'), 'utf8'), 'runner terms');
	assert.ok((await fs.stat(path.join(f.resources, 'streamr-native/streamr-native'))).mode & 0o100);
});

test('ARM64 packaging needs no x64 helper or models', async t => {
	const f = await fixture(t);
	await packageStreamr(path.join(f.app, 'absent'), f.resources, 'linux', 'arm64');
	await assert.rejects(fs.access(f.resources), {code: 'ENOENT'});
});

test('wrong-platform helper and corrupted model stop packaging', async t => {
	const f = await fixture(t);
	await assert.rejects(packageStreamr(f.app, f.resources, 'win32', 'x64'), /does not match/);
	await fs.writeFile(path.join(f.models, 'model.nnef'), 'corrupt weights');
	await assert.rejects(packageStreamr(f.app, f.resources, 'linux', 'x64'), /model hash mismatch/);
	await assert.rejects(fs.access(f.resources), {code: 'ENOENT'});
});

test('modified native executable stops packaging', async t => {
	const f = await fixture(t); await fs.writeFile(path.join(f.native, 'streamr-native'), 'changed');
	await assert.rejects(packageStreamr(f.app, f.resources, 'linux', 'x64'), /helper hash mismatch/);
	await assert.rejects(fs.access(f.resources), {code: 'ENOENT'});
});
