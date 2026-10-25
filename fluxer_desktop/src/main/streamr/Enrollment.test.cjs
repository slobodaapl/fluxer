// SPDX-License-Identifier: AGPL-3.0-or-later
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {pathToFileURL} = require('node:url');
const path = require('node:path');
const modulePromise = import(pathToFileURL(path.resolve(__dirname, '../../../../fluxer_app/src/features/voice/utils/noise_suppression/EnrollmentSample.ts')));
test('enrollment selects a contiguous three-second voiced region and preserves its samples', async () => {
 const {selectEnrollmentSample} = await modulePromise;
 const rate = 48000, audio = new Float32Array(rate * 10);
 const speech = Float32Array.from({length: rate * 3}, (_, i) => 0.2 + i / (rate * 30));
 audio.set(speech, rate * 4);
 assert.deepEqual(selectEnrollmentSample([audio], rate), speech);
});
test('enrollment rejects silence, nonfinite audio and recordings shorter than three seconds', async () => {
 const {selectEnrollmentSample} = await modulePromise;
 for (const audio of [new Float32Array(144000), new Float32Array(143999).fill(1), new Float32Array(144000).fill(NaN)]) assert.throws(() => selectEnrollmentSample([audio], 48000));
});
