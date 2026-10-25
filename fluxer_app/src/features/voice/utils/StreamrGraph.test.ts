// SPDX-License-Identifier: AGPL-3.0-or-later
import {afterEach, beforeEach, expect, it, vi} from 'vitest';

const h = vi.hoisted(() => {
	const node = () => ({
		connect: vi.fn(),
		disconnect: vi.fn(),
		port: {postMessage: vi.fn(), close: vi.fn()},
		gain: {
			value: 0,
			setTargetAtTime: vi.fn(),
			cancelScheduledValues: vi.fn(),
			setValueAtTime: vi.fn(),
			linearRampToValueAtTime: vi.fn(),
		},
	});
	return {node, start: vi.fn(), acquire: vi.fn(), gates: vi.fn()};
});
vi.mock('@app/features/input/state/InputKeybind', () => ({default: {transmitMode: 'voice_activity'}}));
vi.mock('@app/features/platform/utils/AppLogger', () => ({
	Logger: class {
		warn() {}
		debug() {}
	},
}));
vi.mock('@app/features/platform/utils/PlatformHelpers', () => ({isAbortError: (e: Error) => e.name === 'AbortError'}));
vi.mock('@app/features/voice/engine/VoiceInputAudioContext', () => ({discardVoiceInputContext() {}}));
vi.mock('@app/features/voice/engine/VoiceSharedAudioContext', () => ({createVoiceSoftClipNode: () => null}));
vi.mock('@app/features/voice/state/VoiceSettings', () => ({default: {}}));
vi.mock('@app/features/voice/utils/VoiceProcessingProfile', () => ({}));
vi.mock('@app/features/voice/utils/noise_suppression/NoiseSuppressionRuntime', () => ({
	readCaptureNoiseSuppressionBackend: () => 'none',
}));
vi.mock('@app/features/voice/utils/noise_suppression/NoiseSuppressionChain', () => ({}));
vi.mock('@app/features/voice/utils/noise_suppression/DeepFilter', () => ({
	acquireDeepFilterNode: h.acquire,
	hasIdleDeepFilterNode: () => true,
	reportVoiceInputLevel() {},
}));
vi.mock('@app/features/voice/utils/noise_suppression/Streamr', () => ({createStreamrNode: h.start}));
vi.mock('@app/features/voice/utils/VoiceActivityGate', () => ({
	loadVoiceActivityGate: async () => {},
	createVoiceActivityGate: h.gates,
}));

import availability from '@app/features/voice/utils/noise_suppression/NoiseSuppressionAvailability';
import {VoiceInputGraph} from '@app/features/voice/utils/VoiceInputProcessor';

let graph: VoiceInputGraph;
let dfn: {node: ReturnType<typeof h.node>; primed: ReturnType<typeof vi.fn>; release: ReturnType<typeof vi.fn>};
let native: {node: ReturnType<typeof h.node>; dispose: ReturnType<typeof vi.fn>};
beforeEach(() => {
	availability.failures.clear();
	availability.streamrEnrolled = null;
	availability.streamrRevision = 0;
	availability.voiceInputGraphUnavailable = false;
	availability.inputContextRate = 48000;
	h.start.mockReset();
	h.acquire.mockReset();
	h.gates.mockReset();
	h.start.mockImplementation(async () => {
		native = {node: h.node(), dispose: vi.fn()};
		return native;
	});
	h.acquire.mockImplementation(async () => {
		dfn = {node: h.node(), primed: vi.fn(async () => {}), release: vi.fn()};
		return dfn;
	});
	h.gates.mockImplementation(() => h.node());
	graph = new VoiceInputGraph({createGain: h.node, currentTime: 0} as unknown as AudioContext, 1, () => ({
		backend: availability.resolveEffectiveBackend('streamr'),
		inputVolumePercent: 100,
		gateEnabled: true,
		gateAuto: false,
		gateThresholdRms: 0.1,
	}));
});
afterEach(() => graph.dispose());
it('saving enrollment retries a failed start without changing the selected backend', async () => {
	h.start.mockRejectedValueOnce(new Error('Record a voice sample first'));
	await graph.configure();
	expect(h.start).toHaveBeenCalledTimes(1);
	expect(availability.getFallback('streamr')).toBe('failed');
	availability.setStreamrEnrollment(true, true);
	await graph.configure();
	expect(h.start).toHaveBeenCalledTimes(2);
	expect(availability.getFallback('streamr')).toBeNull();
});
it('re-recording replaces a running native session and returns the old DFN lease', async () => {
	availability.setStreamrEnrollment(true);
	await graph.configure();
	const oldDispose = native.dispose,
		oldRelease = dfn.release;
	availability.setStreamrEnrollment(true, true);
	await graph.configure();
	expect(h.start).toHaveBeenCalledTimes(2);
	await vi.waitFor(() => {
		expect(oldDispose).toHaveBeenCalledTimes(1);
		expect(oldRelease).toHaveBeenCalledTimes(1);
	});
});
it('deleting enrollment stops the active chain and does not retry until another sample is saved', async () => {
	availability.setStreamrEnrollment(true);
	await graph.configure();
	availability.setStreamrEnrollment(false, true);
	await graph.configure();
	await vi.waitFor(() => expect(native.dispose).toHaveBeenCalledTimes(1));
	expect(h.start).toHaveBeenCalledTimes(1);
	expect(availability.getFallback('streamr')).toBe('enrollment');
	availability.setStreamrEnrollment(true, true);
	await graph.configure();
	expect(h.start).toHaveBeenCalledTimes(2);
});
it('chains existing DFN through the activity sidechain before the native models', async () => {
	await graph.configure();
	const activity = h.gates.mock.results.find((_, i) => h.gates.mock.calls[i][5] === true)!.value;
	expect(dfn.primed).toHaveBeenCalledTimes(1);
	expect(dfn.node.connect).toHaveBeenCalledWith(activity);
	expect(activity.connect).toHaveBeenCalledWith(native.node, 0, 0);
	expect(activity.connect).toHaveBeenCalledWith(native.node, 1, 1);
	expect(h.gates.mock.calls.find((args) => args[5] === true)?.[2]).toEqual({
		apply: true,
		auto: false,
		manualThresholdRms: 0.1,
	});
});
it('disposal prevents later enrollment notifications from creating sessions', async () => {
	await graph.configure();
	graph.dispose();
	availability.setStreamrEnrollment(true, true);
	await graph.configure();
	expect(h.start).toHaveBeenCalledTimes(1);
});

it('a sample changed during startup retires the stale session and starts with the new enrollment', async () => {
	let resolve!: (value: {node: ReturnType<typeof h.node>; dispose: ReturnType<typeof vi.fn>}) => void;
	const stale = {node: h.node(), dispose: vi.fn()};
	h.start.mockImplementationOnce(
		() =>
			new Promise((r) => {
				resolve = r;
			}),
	);
	const configuring = graph.configure();
	await vi.waitFor(() => expect(h.start).toHaveBeenCalledTimes(1));
	availability.setStreamrEnrollment(true, true);
	resolve(stale);
	await configuring;
	expect(h.start).toHaveBeenCalledTimes(2);
	await vi.waitFor(() => expect(stale.dispose).toHaveBeenCalledTimes(1));
	expect(native.dispose).not.toHaveBeenCalled();
	expect(availability.getFallback('streamr')).toBeNull();
});
