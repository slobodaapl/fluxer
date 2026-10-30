// SPDX-License-Identifier: AGPL-3.0-or-later

import type {
	VoiceBackgroundMediaCacheRequest,
	VoiceBackgroundMediaCacheResult,
} from '@fluxer/desktop_ipc/src/DesktopBridgeTypes';
import {invokeOwnedBytes} from './ipc';

/**
 * `voice-background-media-cache:write`. The media travels as the raw body; header values must be
 * byte strings, so the id, type and file name are percent-encoded.
 */
export async function cacheVoiceBackgroundMedia(
	options: VoiceBackgroundMediaCacheRequest,
): Promise<VoiceBackgroundMediaCacheResult> {
	const headers: Record<string, string> = {
		'x-fluxer-media-id': encodeURIComponent(options.id),
		'x-fluxer-media-type': encodeURIComponent(options.mimeType),
	};
	if (options.fileName) {
		headers['x-fluxer-media-name'] = encodeURIComponent(options.fileName);
	}
	return invokeOwnedBytes('voice_background_media_cache_write', new Uint8Array(options.data), headers);
}
