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
	const transport = {close() {this.closed = true;}};
	const api = {connect: async () => transport, start: async () => 'session', stop: async id => {stopped.push(id);}, push: async (_id, audio) => {pushed.push(audio); return audio;}};
	return {context, api, transport, stopped, pushed, nodes, failures, controller,
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

test('native node transfers its direct audio port and disposes idempotently', async t => {
 const h=harness(t);const {node,dispose}=await h.start();
 assert.deepEqual(node.messages,[{type:'transport',port:h.transport}]);
 assert.equal(h.pushed.length,0);
 dispose();dispose();h.controller.abort();await turn();
 assert.deepEqual(h.stopped,['session']);assert.equal(node.closed,true);assert.equal(node.disconnected,true);
});

test('abort while connecting closes a late port and releases the session', async t => {
 const h=harness(t),connected=deferred();h.api.connect=()=>connected.promise;
 const start=h.start();await turn();h.controller.abort();connected.resolve(h.transport);
 await assert.rejects(start,{name:'AbortError'});
 assert.equal(h.transport.closed,true);assert.deepEqual(h.stopped,['session']);assert.equal(h.nodes.length,0);
});

test('worklet transport failure closes the session and reports exactly once', async t => {
 const h=harness(t);const {node,dispose}=await h.start();
 node.port.onmessage({data:{type:'error',message:'native failed'}});dispose();
 assert.equal(h.failures.length,1);assert.match(h.failures[0].message,/native failed/);
 assert.deepEqual(h.stopped,['session']);assert.equal(node.closed,true);
});

test('failed worklet loading can retry in the same context', async t => {
	const h = harness(t); let attempts = 0;
	h.context.audioWorklet.addModule = async () => {if (++attempts === 1) throw new Error('module unavailable');};
	await assert.rejects(h.start(), /module unavailable/);
	const {dispose} = await h.start(); dispose();
	assert.equal(attempts, 2); assert.deepEqual(h.stopped, ['session']);
});
