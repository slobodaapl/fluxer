// SPDX-License-Identifier: AGPL-3.0-or-later

// The `localAppUpload` API of fluxer_desktop/src/preload/LocalAppPreload.ts, served by
// src-tauri/src/local_app/proxy/upload_progress.rs.

import type {ElectronAPI} from '@app/types/electron';
import type {DesktopLocalAppUploadProgress} from '@fluxer/desktop_ipc/src/LocalAppRouteContract';
import {LOCAL_APP_UPLOAD_PROGRESS_CHANNELS} from '@fluxer/desktop_ipc/src/LocalAppRouteContract';
import {invokeOwned, subscribe} from './ipc';

type DesktopLocalAppUploadAPI = NonNullable<ElectronAPI['localAppUpload']>;

export const localAppUpload: DesktopLocalAppUploadAPI = Object.freeze<DesktopLocalAppUploadAPI>({
	subscribe: (listener: (progress: DesktopLocalAppUploadProgress) => void) => {
		// Sent on every call like the preload's `renderer.send`; the shell keeps one entry per webview.
		invokeOwned<null>('local_app_upload_subscribe').catch((error: unknown) => {
			console.error('[desktop bridge] local_app_upload_subscribe failed', error);
		});
		return subscribe<DesktopLocalAppUploadProgress>(LOCAL_APP_UPLOAD_PROGRESS_CHANNELS.progress, listener);
	},
});
