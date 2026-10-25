// SPDX-License-Identifier: AGPL-3.0-or-later
export function selectEnrollmentSample(channels: ReadonlyArray<Float32Array>, sampleRate: number): Float32Array {
	const count = sampleRate * 3;
	const length = channels[0]?.length ?? 0;
	if (length < count || channels.some((channel) => channel.length !== length))
		throw new Error('Recording was too short. Please try again.');
	const mono = new Float32Array(length);
	for (const channel of channels) for (let i = 0; i < length; i++) mono[i] += channel[i] / channels.length;
	let energy = 0;
	for (let i = 0; i < count; i++) energy += mono[i] ** 2;
	let best = energy,
		start = 0;
	for (let i = count; i < length; i++) {
		energy += mono[i] ** 2 - mono[i - count] ** 2;
		if (energy > best) {
			best = energy;
			start = i - count + 1;
		}
	}
	if (!mono.every(Number.isFinite) || best / count < 1e-7)
		throw new Error('No clear voice detected. Move closer to your microphone and try again.');
	return mono.slice(start, start + count);
}
