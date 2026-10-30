// SPDX-License-Identifier: AGPL-3.0-or-later
export const STREAMR_CHANNELS = {
	status: 'streamr:status',
	saveSample: 'streamr:save-sample',
	clearSample: 'streamr:clear-sample',
	start: 'streamr:start',
	push: 'streamr:push',
	connect: 'streamr:connect',
	port: 'streamr:port',
	stop: 'streamr:stop',
} as const;

export type StreamrMode = 'streamr' | 'gamr';

export interface StreamrAPI {
	status(): Promise<{enrolled: boolean; modelsAvailable: boolean}>;
	/** Three seconds of mono float32 at 48 kHz, stored only on this device. */
	saveSample(sample: Float32Array): Promise<void>;
	clearSample(): Promise<void>;
	start(mode?: StreamrMode): Promise<{id: string; backend: 'vulkan' | 'cpu'}>;
	/** Enqueue 20 ms at 48 kHz; return completed 48 kHz output, possibly empty. One outstanding request. */
	push(id: string, audio: Float32Array): Promise<Float32Array>;
	connect(id: string): Promise<void>;
	stop(id: string): Promise<void>;
}
