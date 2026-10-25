// SPDX-License-Identifier: AGPL-3.0-or-later
'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {pathToFileURL} = require('node:url');
const path = require('node:path');
const modulePromise = import(pathToFileURL(path.resolve(__dirname,
	'../../../../fluxer_app/src/features/voice/utils/noise_suppression/NativeVoiceIsolation.ts')));
const deferred = () => {
	let resolve, reject;
	const promise = new Promise((a, b) => {resolve = a; reject = b;});
	return {promise, resolve, reject};
};
const turn = () => new Promise(resolve => setImmediate(resolve));

function harness(t) {
	const stopped = [], pushed = [], nodes = [], failures = [];
	const controller = new AbortController();
	class AudioWorkletNode {
		constructor() {
			this.messages = [];
			this.port = {onmessage: null, postMessage: message => this.messages.push(message), close: () => {this.closed = true;}};
			nodes.push(this);
		}
		disconnect() { this.disconnected = true; }
	}
	const previous = globalThis.AudioWorkletNode;
	globalThis.AudioWorkletNode = AudioWorkletNode;
	t.after(() => {globalThis.AudioWorkletNode = previous;});
	const context = {sampleRate: 48000, audioWorklet: {addModule: async () => {}}};
	const api = {start: async () => 'session', stop: async id => {stopped.push(id);}, push: async (_id, audio) => {pushed.push(audio); return audio;}};
	return {context, api, stopped, pushed, nodes, failures, controller,
		async start() {const {createNativeVoiceIsolationNode} = await modulePromise;
			return createNativeVoiceIsolationNode(context, controller.signal, error => failures.push(error), api, 'worklet.js', 'fluxer-streamr', 'Streamr');},
	};
}

test('cancellation while enrollment starts closes the eventual session without creating a node', async t => {
	const h = harness(t), ready = deferred(); h.api.start = () => ready.promise;
	const start = h.start(); await turn(); h.controller.abort(); ready.resolve('late-session');
	await assert.rejects(start, {name: 'AbortError'});
	assert.deepEqual(h.stopped, ['late-session']); assert.equal(h.nodes.length, 0);
});

test('native node serializes queued audio and disposes idempotently', async t => {
	const h = harness(t), first = deferred(); let calls = 0;
	h.api.push = async (_id, audio) => {h.pushed.push(audio); return ++calls === 1 ? first.promise : audio;};
	const result = await h.start(), node = result.node;
	const a = new Float32Array([1]), b = new Float32Array([2]);
	node.port.onmessage({data: {type: 'audio', audio: a}});
	node.port.onmessage({data: {type: 'audio', audio: b}});
	assert.equal(h.pushed.length, 1);
	first.resolve(a); await turn();
	assert.deepEqual(h.pushed, [a, b]);
	assert.deepEqual(node.messages.filter(m => m.type === 'processed').map(m => m.audio), [a, b]);
	result.dispose(); result.dispose(); h.controller.abort(); await turn();
	assert.deepEqual(h.stopped, ['session']); assert.equal(node.closed, true); assert.equal(node.disconnected, true);
});

test('abort suppresses a late audio response and releases the session', async t => {
	const h = harness(t), response = deferred(); h.api.push = () => response.promise;
	const {node} = await h.start(); node.port.onmessage({data: {type: 'audio', audio: new Float32Array(960)}});
	h.controller.abort(); response.resolve(new Float32Array(960)); await turn();
	assert.equal(node.messages.filter(m => m.type === 'processed').length, 0);
	assert.deepEqual(h.stopped, ['session']); assert.equal(h.failures.length, 0);
});

test('backend failure closes the node and reports exactly once', async t => {
	const h = harness(t); h.api.push = async () => {throw new Error('native failed');};
	const {node, dispose} = await h.start(); node.port.onmessage({data: {type: 'audio', audio: new Float32Array(960)}});
	await turn(); dispose();
	assert.equal(h.failures.length, 1); assert.match(h.failures[0].message, /native failed/);
	assert.deepEqual(h.stopped, ['session']); assert.equal(node.closed, true);
});

test('failed worklet loading can retry in the same context', async t => {
	const h = harness(t); let attempts = 0;
	h.context.audioWorklet.addModule = async () => {if (++attempts === 1) throw new Error('module unavailable');};
	await assert.rejects(h.start(), /module unavailable/);
	const {dispose} = await h.start(); dispose();
	assert.equal(attempts, 2); assert.deepEqual(h.stopped, ['session']);
});
