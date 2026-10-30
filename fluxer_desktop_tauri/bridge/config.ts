// SPDX-License-Identifier: AGPL-3.0-or-later

import type {DesktopCapabilityManifest} from '@fluxer/desktop_ipc/src/CapabilityManifest';

const CONFIG_GLOBAL = '__FLUXER_DESKTOP_BRIDGE_CONFIG__';

/** Serialised by fluxer_desktop_tauri/src-tauri/src/bridge.rs. */
export interface BridgeConfig {
	readonly platform: 'darwin' | 'win32' | 'linux';
	readonly buildChannel: 'stable' | 'canary' | 'development';
	readonly localDevelopmentInstanceUrl: string | null;
	readonly offlineBuild: boolean;
	readonly localAppOrigin: string;
	readonly capabilities: DesktopCapabilityManifest;
	/** What the main window was created with; Electron passed these as renderer arguments. */
	readonly activeUseNativeTitleBar: boolean;
	readonly activeAllowTransparency: boolean;
}

export function takeBridgeConfig(): BridgeConfig {
	const host = globalThis as Record<string, unknown>;
	const config = host[CONFIG_GLOBAL] as BridgeConfig | undefined;
	if (config === undefined) {
		throw new Error('The desktop bridge started without its configuration');
	}
	delete host[CONFIG_GLOBAL];
	return Object.freeze({...config, capabilities: Object.freeze({...config.capabilities})});
}
