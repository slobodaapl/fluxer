// SPDX-License-Identifier: AGPL-3.0-or-later
'use strict';
const childProcess = require('node:child_process');

const INPUT_SAMPLES = 960;
const MAX_OUTPUT_SAMPLES = 9600;
const MAX_PACKET_BYTES = MAX_OUTPUT_SAMPLES * 4;

function packet(kind, body) {
	const header = Buffer.alloc(8);
	header.writeUInt32LE(kind, 0);
	header.writeUInt32LE(body.length, 4);
	return Buffer.concat([header, body]);
}

class NativeStream {
	constructor({executable, modelDir, sample, signal, onFailure, mode = 'streamr'}, backend) {
		this.backend = backend;
		this.closed = false;
		this.initialized = false;
		this.pending = null;
		this.buffer = Buffer.alloc(0);
		this.stderr = '';
		this.signal = signal;
		this.onFailure = onFailure;
		this.ready = new Promise((resolve, reject) => {
			this.startup = {resolve, reject};
		});
		this.child = childProcess.spawn(executable, [modelDir, backend, ...(mode === 'gamr' ? ['gamr'] : [])], {
			stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true,
		});
		this.abort = () => this.close(new Error('Streamr start cancelled'));
		signal.addEventListener('abort', this.abort, {once: true});
		this.timer = setTimeout(() => this.fail(new Error(`Streamr ${backend} initialization timed out`)), 30_000);
		this.child.stdout.on('data', (chunk) => this.receive(chunk));
		this.child.stderr.on('data', (chunk) => { this.stderr = (this.stderr + chunk.toString()).slice(-4096); });
		this.child.stdin.on('error', (error) => this.fail(error));
		this.child.stdout.on('error', (error) => this.fail(error));
		this.child.stderr.on('error', (error) => this.fail(error));
		this.child.on('error', (error) => this.fail(error));
		this.child.on('exit', (code, reason) => this.fail(new Error(
			`Streamr ${backend} exited (${reason ?? code})${this.stderr ? `: ${this.stderr.trim()}` : ''}`,
		)));
		if (signal.aborted) this.abort();
		else this.child.stdin.write(packet(0, mode === 'gamr' ? Buffer.alloc(0) : Buffer.from(sample.buffer, sample.byteOffset, sample.byteLength)));
	}

	receive(chunk) {
		if (this.closed) return;
		this.buffer = Buffer.concat([this.buffer, chunk]);
		if (this.buffer.length > MAX_PACKET_BYTES + 8) {
			this.fail(new Error('Streamr native response exceeds its size limit'));
			return;
		}
		while (this.buffer.length >= 8) {
			const kind = this.buffer.readUInt32LE(0), size = this.buffer.readUInt32LE(4);
			if (size > MAX_PACKET_BYTES) { this.fail(new Error('Invalid Streamr response size')); return; }
			if (this.buffer.length < 8 + size) return;
			const body = this.buffer.subarray(8, 8 + size);
			this.buffer = this.buffer.subarray(8 + size);
			if (kind === 2) { this.fail(new Error(body.toString('utf8'))); return; }
			if (kind === 0 && !this.initialized && body.toString('utf8') === this.backend) {
				clearTimeout(this.timer);
				this.initialized = true;
				this.startup.resolve();
				this.startup = null;
				continue;
			}
			if (kind !== 1 || !this.initialized || !this.pending || size % 4 !== 0) {
				this.fail(new Error('Unexpected Streamr native response'));
				return;
			}
			const audio = new Float32Array(body.buffer.slice(body.byteOffset, body.byteOffset + body.byteLength));
			if (!audio.every(Number.isFinite)) { this.fail(new Error('Nonfinite Streamr native output')); return; }
			clearTimeout(this.pending.timer);
			this.pending.resolve(audio);
			this.pending = null;
		}
	}

	push(audio) {
		if (this.closed || !this.initialized) return Promise.reject(new Error('Streamr is not running'));
		if (this.pending) return Promise.reject(new Error('Concurrent Streamr audio requests are prohibited'));
		if (!(audio instanceof Float32Array) || audio.length !== INPUT_SAMPLES || !audio.every(Number.isFinite)) {
			return Promise.reject(new Error('Streamr requires 960 finite float32 samples per block'));
		}
		return new Promise((resolve, reject) => {
			const timer = setTimeout(() => this.fail(new Error('Streamr transport timed out')), 5000);
			this.pending = {resolve, reject, timer};
			this.child.stdin.write(packet(1, Buffer.from(audio.buffer, audio.byteOffset, audio.byteLength)));
		});
	}

	fail(error) {
		if (this.closed) return;
		const running = this.initialized;
		this.close(error);
		if (running) this.onFailure(error);
	}

	close(error = new Error('Streamr session closed')) {
		if (this.closed) return;
		this.closed = true;
		clearTimeout(this.timer);
		this.signal.removeEventListener('abort', this.abort);
		this.startup?.reject(error);
		this.startup = null;
		if (this.pending) {
			clearTimeout(this.pending.timer);
			this.pending.reject(error);
			this.pending = null;
		}
		this.child.stdin.destroy();
		this.child.kill();
		this.buffer = Buffer.alloc(0);
	}
}

async function createNativeStream(options) {
	const errors = [];
	for (const backend of ['vulkan', 'cpu']) {
		options.signal.throwIfAborted();
		let stream;
		try {
			stream = new NativeStream(options, backend);
			await stream.ready;
			options.signal.throwIfAborted();
			return stream;
		} catch (error) {
			stream?.close();
			errors.push(`${backend}: ${error.message}`);
		}
	}
	options.signal.throwIfAborted();
	throw new Error(`Streamr could not start. ${errors.join('; ')}`);
}

module.exports = {createNativeStream, INPUT_SAMPLES, MAX_OUTPUT_SAMPLES};
