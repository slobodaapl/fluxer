// SPDX-License-Identifier: AGPL-3.0-or-later
'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.resolve(__dirname,
	'../../../../fluxer_app/src/features/voice/utils/noise_suppression/streamrProcessor.worklet.js'), 'utf8');

function harness() {
	let processor;
	const messages = [];
	class AudioWorkletProcessor {
		constructor() { this.port = {postMessage: (message) => messages.push(message)}; }
	}
	vm.runInNewContext(source, {Float32Array, AudioWorkletProcessor,
		registerProcessor(name, type) { assert.equal(name, 'fluxer-streamr'); processor = new type(); },
	});
	const transport = {postMessage: (message, transfer) => { assert.ok(!transfer?.length, 'Electron main-process ports require cloned audio buffers'); messages.push(message); }, start() {}, close() {}};
	processor.port.onmessage({data: {type: 'transport', port: transport}});
	return {
		messages,
		tick(channels = [new Float32Array(128)], activity) {
			const output = new Float32Array(128);
			const active = processor.process([channels, ...(activity ? [[activity]] : [])], [[output]]);
			return {output, active};
		},
		reply(audio) { transport.onmessage({data: {type: 'processed', audio}}); },
		dispose() { processor.port.onmessage({data: {type: 'dispose'}}); },
	};
}

test('worklet sends ordered 20ms mono blocks across 128-sample render quanta', () => {
	const h = harness(), sent = [];
	for (let quantum = 0; quantum < 30; quantum++) {
		const left = Float32Array.from({length: 128}, (_, i) => (quantum * 128 + i) / 4096);
		const right = new Float32Array(128).fill(-0.25);
		assert.ok(h.tick([left, right]).output.every(x => x === 0));
		for (const message of h.messages.splice(0)) {
			assert.equal(message.type, 'audio'); sent.push(...message.audio);
			h.reply(new Float32Array(0));
		}
	}
	assert.equal(sent.length, 3840);
	for (let i = 0; i < sent.length; i++) assert.equal(sent[i], (i / 4096 - 0.25) / 2);
});

test('buffered playback preserves samples through ring wrap without repeating or dropping them', () => {
	const h = harness(), played = [], supplied = [];
	let sequence = 0, started = false;
	for (let quantum = 0; quantum < 1200; quantum++) {
		const {output} = h.tick();
		if (!started) {
			const first = output.findIndex(x => x !== 0);
			if (first !== -1) { started = true; played.push(...output.subarray(first)); }
		} else played.push(...output);
		for (const message of h.messages.splice(0)) {
			assert.equal(message.type, 'audio');
			const audio = Float32Array.from({length: 960}, () => ++sequence);
			supplied.push(...audio); h.reply(audio);
		}
	}
	assert.ok(played.length > 100000);
	assert.deepEqual(played, supplied.slice(0, played.length));
	h.dispose(); assert.equal(h.tick().active, false);
});

test('missing native responses report bounded backpressure once', () => {
	const h = harness();
	for (let i = 0; i < 220; i++) h.tick();
	assert.equal(h.messages.filter(x => x.type === 'audio').length, 1);
	const errors = h.messages.filter(x => x.type === 'error');
	assert.equal(errors.length, 1); assert.match(errors[0].message, /backlog exceeded 500 ms/);
});

test('playback starvation emits silence and resumes buffered output without replaying stale samples', () => {
 const h = harness();
 const played = [];
 let requests = 0;
 for (let i = 0; i < 250; i++) {
  played.push(...h.tick().output);
  for (const message of h.messages.splice(0)) {
   assert.equal(message.type, 'audio');
   requests++;
   h.reply(requests === 1 ? new Float32Array(4000).fill(0.25)
    : requests >= 18 ? new Float32Array(960).fill(0.75) : new Float32Array(0));
  }
 }
 assert.equal(played.filter(x => x === 0.25).length, 4000);
 const lastInitial = played.lastIndexOf(0.25), resumed = played.indexOf(0.75);
 assert.ok(resumed > lastInitial + 1);
 assert.ok(played.slice(lastInitial + 1, resumed).every(x => x === 0));
 assert.ok(played.slice(resumed).every(x => x === 0.75));
});

for (const [label, samples] of [
	['nonfinite', new Float32Array([NaN])], ['oversized', new Float32Array(48001)],
]) test(`${label} output terminates worklet processing`, () => {
	const h = harness();
	for (let i = 0; i < 8; i++) h.tick();
	h.messages.length = 0; h.reply(samples);
	assert.equal(h.messages.length, 1); assert.equal(h.messages[0].type, 'error');
	assert.ok(h.tick().output.every(x => x === 0));
});


test('silence submits no inference; speech resumes with pre-roll and returns to idle after its tail', () => {
 const h = harness();
 const quiet = new Float32Array(128), active = new Float32Array(128).fill(1);
 for (let i = 0; i < 1000; i++) assert.ok(h.tick([quiet], quiet).output.every(x => x === 0));
 assert.equal(h.messages.length, 0);
 const sent = [];
 const drive = (count, speech) => {
  for (let i = 0; i < count; i++) {
   h.tick([new Float32Array(128).fill(speech ? 0.4 : 0)], speech ? active : quiet);
   for (const message of h.messages.splice(0)) {
    assert.equal(message.type, 'audio');
    sent.push(message.audio);
    h.reply(message.audio.slice());
   }
  }
 };
 drive(80, true); drive(500, false);
 assert.ok(sent.length > 10 && sent.length < 50);
 assert.ok(sent[0].every(x => x === 0));
 const afterTail = sent.length;
 drive(1000, false); assert.equal(sent.length, afterTail);
 drive(80, true); assert.ok(sent.length > afterTail);
 drive(500, false); const final = sent.length;
 drive(1000, false); assert.equal(sent.length, final);
});

test('the existing activity detector drives Streamr from the manual voice threshold', () => {
 const types = new Map();
 class AudioWorkletProcessor { constructor() { this.port = {postMessage() {}}; } }
 const gateSource = fs.readFileSync(path.resolve(__dirname, '../../../../fluxer_app/src/features/voice/utils/voiceGateProcessor.worklet.js'), 'utf8');
 vm.runInNewContext(gateSource, {Float32Array, Float64Array, Uint8Array, Uint16Array, AudioWorkletProcessor,
  sampleRate: 48000, currentTime: 0, registerProcessor: (name, type) => types.set(name, type)});
 const gate = new (types.get('fluxer-voice-gate'))({processorOptions: {
  apply: true, auto: false, manualThresholdRms: 0.1, minRms: 0.01, maxRms: 1,
  releaseMs: 200, channelCount: 1, activityOutput: true,
 }});
 const h = harness(); let requests = 0;
 const drive = (amplitude, frames) => {
  for (let i = 0; i < frames; i++) {
   const raw = new Float32Array(128).fill(amplitude), audio = new Float32Array(128), activity = new Float32Array(128);
   gate.process([[raw]], [[audio], [activity]]);
   assert.deepEqual(audio, raw);
   h.tick([audio], activity);
   for (const message of h.messages.splice(0)) { assert.equal(message.type, 'audio'); requests++; h.reply(message.audio.slice()); }
  }
 };
 drive(0.05, 1000); assert.equal(requests, 0);
 drive(0.2, 150); assert.ok(requests > 0);
 gate.port.onmessage({data: {type: 'config', apply: true, auto: false, manualThresholdRms: 0.3}});
 drive(0.2, 1000); const idle = requests;
 drive(0.2, 1000); assert.equal(requests, idle);
});


test('a 300ms transport stall drains in order without dropping samples or failing', () => {
 const h = harness(); let requests = 0; const sent = [];
 for(let quantum=0;quantum<180;quantum++) {
  h.tick([Float32Array.from({length:128},(_,i)=>(quantum*128+i)/100000)]);
  if(quantum<112) continue;
  for(const message of h.messages.splice(0)) {
   assert.equal(message.type,'audio'); sent.push(...message.audio); requests++;
   h.reply(new Float32Array(960));
  }
 }
 while(h.messages.length) {
  const message=h.messages.shift();assert.equal(message.type,'audio');sent.push(...message.audio);requests++;h.reply(new Float32Array(960));
 }
 assert.equal(requests,24);
 assert.deepEqual(sent,Array.from(Float32Array.from({length:23040},(_,i)=>i/100000)));
});
