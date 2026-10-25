// SPDX-License-Identifier: AGPL-3.0-or-later
export interface NativeStream {
	readonly backend: 'vulkan' | 'cpu';
	push(audio: Float32Array): Promise<Float32Array>;
	close(error?: Error): void;
}
export function createNativeStream(options: {
	executable: string;
	modelDir: string;
	sample: Float32Array;
	signal: AbortSignal;
	onFailure(error: Error): void;
}): Promise<NativeStream>;
export const INPUT_SAMPLES: 960;
export const MAX_OUTPUT_SAMPLES: 9600;
