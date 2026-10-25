// SPDX-License-Identifier: AGPL-3.0-or-later
'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {EventEmitter} = require('node:events');
const {PassThrough, Writable} = require('node:stream');
const childProcess = require('node:child_process');
const {createNativeStream} = require('./Native.cjs');

function frame(kind, body = Buffer.alloc(0)) {
	const header = Buffer.alloc(8);
	header.writeUInt32LE(kind); header.writeUInt32LE(body.length, 4);
	return Buffer.concat([header, body]);
}
function harness(t, respond) {
	const children = [], failures = [];
	t.mock.method(childProcess, 'spawn', (_executable, [_modelDir, backend]) => {
		const child = new EventEmitter();
		child.stdout = new PassThrough(); child.stderr = new PassThrough(); child.killed = false;
		child.kill = () => { child.killed = true; return true; };
		child.stdin = new Writable({write(data, _encoding, done) {
			queueMicrotask(() => respond(child, backend, data)); done();
		}});
		children.push(child); return child;
	});
	const controller = new AbortController();
	return {children, failures, controller, start: () => createNativeStream({
		executable: '/native', modelDir: '/models', sample: new Float32Array(144000),
		signal: controller.signal, onFailure: (error) => failures.push(error),
	})};
}

test('fragmented native packets preserve float samples and enrollment framing', async (t) => {
	const wanted = new Float32Array([0.25, -0.75, 0]);
	const h = harness(t, (child, backend, data) => {
		const kind = data.readUInt32LE();
		assert.equal(data.readUInt32LE(4), data.length - 8);
		if (kind === 0) assert.equal(data.length - 8, 144000 * 4);
		else assert.equal(data.length - 8, 960 * 4);
		const reply = frame(kind, kind === 0 ? Buffer.from(backend) : Buffer.from(wanted.buffer));
		for (const byte of reply) child.stdout.write(Buffer.from([byte]));
	});
	const stream = await h.start(); t.after(() => stream.close());
	assert.equal(stream.backend, 'vulkan');
	assert.deepEqual(await stream.push(new Float32Array(960)), wanted);
	stream.close(); stream.close();
	assert.equal(h.children[0].killed, true);
	assert.equal(h.failures.length, 0);
});

test('failed Vulkan startup closes its child before CPU fallback', async (t) => {
	const h = harness(t, (child, backend) => {
		if (backend === 'cpu') assert.equal(h.children[0].killed, true);
		child.stdout.write(backend === 'vulkan' ? frame(2, Buffer.from('no GPU')) : frame(0, Buffer.from('cpu')));
	});
	const stream = await h.start(); t.after(() => stream.close());
	assert.equal(stream.backend, 'cpu'); assert.equal(h.children.length, 2);
	assert.equal(h.failures.length, 0);
});

test('cancellation during startup prevents fallback and releases the child', async (t) => {
	const h = harness(t, () => h.controller.abort());
	await assert.rejects(h.start(), {name: 'AbortError'});
	assert.equal(h.children.length, 1); assert.equal(h.children[0].killed, true);
});

test('only one request may be outstanding; close rejects it', async (t) => {
	const h = harness(t, (child, backend, data) => {
		if (data.readUInt32LE() === 0) child.stdout.write(frame(0, Buffer.from(backend)));
	});
	const stream = await h.start();
	await assert.rejects(stream.push(new Float32Array(959)), /960 finite/);
	await assert.rejects(stream.push(new Float32Array(960).fill(NaN)), /960 finite/);
	const pending = stream.push(new Float32Array(960));
	await assert.rejects(stream.push(new Float32Array(960)), /Concurrent/);
	stream.close(); await assert.rejects(pending, /closed/);
	await assert.rejects(stream.push(new Float32Array(960)), /not running/);
	assert.equal(h.children[0].killed, true);
});

for (const [label, response, message] of [
	['oversized header', Buffer.from([1, 0, 0, 0, 255, 255, 255, 127]), /size/],
	['unaligned samples', frame(1, Buffer.alloc(3)), /Unexpected/],
	['nonfinite samples', frame(1, Buffer.from(new Float32Array([Infinity]).buffer)), /Nonfinite/],
	['unknown packet', frame(9), /Unexpected/],
]) test(`${label} fails the live session without fallback`, async (t) => {
	const h = harness(t, (child, backend, data) => child.stdout.write(
		data.readUInt32LE() === 0 ? frame(0, Buffer.from(backend)) : response,
	));
	const stream = await h.start();
	await assert.rejects(stream.push(new Float32Array(960)), message);
	assert.equal(h.failures.length, 1); assert.equal(h.children.length, 1);
	assert.equal(h.children[0].killed, true);
});

test('native process exit rejects pending audio and reports failure once', async (t) => {
	const h = harness(t, (child, backend, data) => {
		if (data.readUInt32LE() === 0) child.stdout.write(frame(0, Buffer.from(backend)));
		else { child.emit('exit', 7, null); child.emit('error', new Error('late error')); }
	});
	const stream = await h.start();
	await assert.rejects(stream.push(new Float32Array(960)), /exited \(7\)/);
	assert.equal(h.failures.length, 1); assert.equal(h.children[0].killed, true);
});

for (const pipe of ['stdin', 'stdout', 'stderr']) test(`${pipe} error closes the session and rejects pending audio`, async (t) => {
	const h = harness(t, (child, backend, data) => {
		if (data.readUInt32LE() === 0) child.stdout.write(frame(0, Buffer.from(backend)));
		else child[pipe].emit('error', new Error('pipe failure'));
	});
	const stream = await h.start();
	await assert.rejects(stream.push(new Float32Array(960)), /pipe failure/);
	assert.equal(h.failures.length, 1); assert.equal(h.children[0].killed, true);
});
