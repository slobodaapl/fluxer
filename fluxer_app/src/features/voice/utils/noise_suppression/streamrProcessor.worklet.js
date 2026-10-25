// SPDX-License-Identifier: AGPL-3.0-or-later
class StreamrProcessor extends AudioWorkletProcessor {
	constructor() {
		super();
		this.active = true;
		this.failed = false;
		this.input = new Float32Array(960);
		this.inputIndex = 0;
		this.queue = new Float32Array(24000);
		this.read = 0;
		this.write = 0;
		this.count = 0;
		this.started = false;
		this.outstanding = 0;
		this.preRoll = null;
		this.blockActive = false;
		this.hangover = 0;
		this.feeding = false;
		this.port.onmessage = ({data}) => {
			if (data.type === 'dispose') { this.active = false; return; }
			if (data.type !== 'processed' || !this.active || this.failed) return;
			if (this.outstanding === 0) { this.fail('Unexpected Streamr response'); return; }
			this.outstanding--;
			const audio = data.audio;
			if (!(audio instanceof Float32Array) || this.count + audio.length > this.queue.length) {
				this.fail('Streamr output buffer overflow'); return;
			}
			for (const sample of audio) {
				if (!Number.isFinite(sample)) { this.fail('Invalid Streamr output'); return; }
				this.queue[this.write] = sample;
				this.write = (this.write + 1) % this.queue.length;
				this.count++;
			}
		};
	}
	fail(message) {
		if (this.failed) return;
		this.failed = true;
		this.port.postMessage({type: 'error', message});
	}
	send(audio) {
		if (this.outstanding >= 3) { this.fail('Streamr audio transport cannot keep up'); return; }
		this.port.postMessage({type: 'audio', audio}, [audio.buffer]);
		this.outstanding++;
	}
	process(inputs, outputs) {
		if (!this.active) return false;
		const output = outputs[0]?.[0];
		if (!output || this.failed) return true;
		const channels = inputs[0] ?? [];
		const activity = inputs[1]?.[0];
		for (let i = 0; i < output.length; i++) {
			let sample = 0;
			for (const channel of channels) sample += channel[i] ?? 0;
			this.input[this.inputIndex++] = sample / (channels.length || 1);
			this.blockActive ||= (activity?.[i] ?? 1) > 0;
			if (this.inputIndex === this.input.length) {
				if (this.blockActive) this.hangover = 24000;
				else this.hangover = Math.max(0, this.hangover - 960);
				if (this.hangover > 0) {
					if (!this.feeding && this.preRoll) this.send(this.preRoll);
					this.preRoll = null;
					this.send(this.input);
					this.feeding = true;
				} else {
					this.preRoll = this.input;
					this.feeding = false;
				}
				this.input = new Float32Array(960);
				this.inputIndex = 0;
				this.blockActive = false;
				if (this.failed) return true;
			}
			if (!this.started && this.count >= 3840) this.started = true;
			if (!this.started) continue;
			if (!this.count) {
				this.started = false;
				continue;
			}
			output[i] = this.queue[this.read];
			this.read = (this.read + 1) % this.queue.length;
			this.count--;
		}
		return true;
	}
}
registerProcessor('fluxer-streamr', StreamrProcessor);
