// SPDX-License-Identifier: AGPL-3.0-or-later

// The document start-up side effects of fluxer_desktop/src/preload/index.ts. The flags Electron
// passed as `--fluxer-active-*` renderer arguments come from the bridge config.

import {readInstanceProductName} from '@fluxer/desktop_ipc/src/InstanceProductNameContract';
import {
	buildNativeTitlebarStartupHtml,
	NATIVE_TITLEBAR_CLASS,
	NATIVE_TITLEBAR_CONTROL_ATTR,
	type NativeTitlebarControlName,
	type NativeTitlebarLayout,
} from '@fluxer/desktop_ipc/src/NativeTitlebarShell';
import {ZOOM_PREBOOT_MIRROR_STORAGE_KEY} from '@fluxer/desktop_ipc/src/StorageContract';
import {isRecord} from '@fluxer/record_utils/src/RecordGuards';
import type {BridgeConfig} from './config';
import {send} from './ipc';

const ACCESSIBILITY_STORE_STORAGE_KEY = 'AccessibilityStore';
const THEME_STORAGE_KEY = 'theme';
const CUSTOM_TITLEBAR_HEIGHT = '32px';
const NATIVE_TITLEBAR_HEIGHT = '0px';
const STARTUP_NATIVE_TITLEBAR_ID = 'fluxer-startup-native-titlebar';
const SERVED_PRODUCT_NAME_SELECTOR = 'meta[name="apple-mobile-web-app-title"]:not([data-fluxer-branding])';

/** The window commands behind Electron's `window-${control}` channels. */
const TITLEBAR_CONTROL_COMMANDS: Readonly<Record<NativeTitlebarControlName, string>> = {
	minimize: 'window_minimize',
	maximize: 'window_toggle_maximize',
	close: 'window_close',
};

interface AccessibilityStartupSettings {
	syncReducedMotionWithSystem: boolean;
	reducedMotionOverride: boolean | null;
}

function readLegacyAccessibilityStartupSettings(): AccessibilityStartupSettings | null {
	let raw: string | null;
	try {
		raw = window.localStorage.getItem(ACCESSIBILITY_STORE_STORAGE_KEY);
	} catch {
		return null;
	}
	if (raw === null) {
		return null;
	}
	let parsed: unknown;
	try {
		parsed = JSON.parse(raw);
	} catch {
		return null;
	}
	if (!isRecord(parsed)) {
		return null;
	}
	const metadata = parsed.__mps__;
	if (isRecord(metadata) && metadata.version !== 1) {
		return null;
	}
	return {
		syncReducedMotionWithSystem:
			typeof parsed.syncReducedMotionWithSystem === 'boolean' ? parsed.syncReducedMotionWithSystem : true,
		reducedMotionOverride: typeof parsed.reducedMotionOverride === 'boolean' ? parsed.reducedMotionOverride : null,
	};
}

function hasPrebootZoomMirror(): boolean {
	let raw: string | null;
	try {
		raw = window.localStorage.getItem(ZOOM_PREBOOT_MIRROR_STORAGE_KEY);
	} catch {
		return false;
	}
	return raw !== null && Number.isFinite(Number.parseInt(raw, 10));
}

function readAccessibilityStartupSettings(): AccessibilityStartupSettings | null {
	const hasZoomMirror = hasPrebootZoomMirror();
	const legacySettings = readLegacyAccessibilityStartupSettings();
	if (!hasZoomMirror && legacySettings === null) {
		return null;
	}
	return {
		syncReducedMotionWithSystem: legacySettings?.syncReducedMotionWithSystem ?? true,
		reducedMotionOverride: legacySettings?.reducedMotionOverride ?? null,
	};
}

function getSystemReducedMotionPreference(): boolean {
	if (typeof window.matchMedia !== 'function') {
		return false;
	}
	return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

function resolveReducedMotion(settings: AccessibilityStartupSettings): boolean {
	return settings.syncReducedMotionWithSystem
		? getSystemReducedMotionPreference()
		: (settings.reducedMotionOverride ?? false);
}

function withDocumentElement(callback: (root: HTMLElement) => void): void {
	const apply = (): boolean => {
		const root = document.documentElement as HTMLElement | null;
		if (root === null) {
			return false;
		}
		callback(root);
		return true;
	};
	if (apply()) {
		return;
	}
	let observer: MutationObserver | null = null;
	let applied = false;
	const applyWhenReady = (): void => {
		if (applied) {
			return;
		}
		const root = document.documentElement as HTMLElement | null;
		if (root === null) {
			return;
		}
		applied = true;
		observer?.disconnect();
		document.removeEventListener('DOMContentLoaded', applyWhenReady);
		callback(root);
	};
	observer = new MutationObserver(applyWhenReady);
	observer.observe(document, {childList: true});
	document.addEventListener('DOMContentLoaded', applyWhenReady, {once: true});
	applyWhenReady();
}

function applyReducedMotionClass(reducedMotion: boolean): void {
	withDocumentElement((root) => {
		root.classList.toggle('reduced-motion', reducedMotion);
	});
}

function getStartupPlatformClass(platform: BridgeConfig['platform']): string {
	switch (platform) {
		case 'darwin':
			return 'platform-macos';
		case 'win32':
			return 'platform-windows';
		case 'linux':
			return 'platform-linux';
		default:
			return 'platform-unknown';
	}
}

function wireStartupNativeTitlebarControls(titlebar: HTMLElement): void {
	titlebar.querySelectorAll<HTMLButtonElement>(`[${NATIVE_TITLEBAR_CONTROL_ATTR}]`).forEach((button) => {
		const control = button.getAttribute(NATIVE_TITLEBAR_CONTROL_ATTR) as NativeTitlebarControlName | null;
		if (control == null) return;
		button.addEventListener('click', () => {
			send(TITLEBAR_CONTROL_COMMANDS[control]);
		});
	});
}

function readServedProductName(): string | null {
	const content = document.head?.querySelector<HTMLMetaElement>(SERVED_PRODUCT_NAME_SELECTOR)?.getAttribute('content');
	return readInstanceProductName(content);
}

function installStartupNativeTitlebar(config: BridgeConfig): void {
	// A popout's blank document (`about:`) gets its titlebar from the renderer that writes it.
	if (config.activeUseNativeTitleBar || window.location.protocol === 'data:' || window.location.protocol === 'about:') {
		return;
	}
	const layout: NativeTitlebarLayout = config.platform === 'darwin' ? 'macos' : 'controls';
	const install = (): void => {
		if (!document.body || document.getElementById(STARTUP_NATIVE_TITLEBAR_ID)) return;
		const titlebar = document.createElement('div');
		titlebar.id = STARTUP_NATIVE_TITLEBAR_ID;
		titlebar.className = NATIVE_TITLEBAR_CLASS.root;
		titlebar.setAttribute('role', 'group');
		titlebar.setAttribute('aria-hidden', 'true');
		titlebar.setAttribute('data-native-titlebar', '');
		titlebar.setAttribute('data-platform', getStartupPlatformClass(config.platform).replace('platform-', ''));
		titlebar.innerHTML = buildNativeTitlebarStartupHtml(layout, readServedProductName());
		if (layout === 'controls') {
			wireStartupNativeTitlebarControls(titlebar);
		}
		document.body.prepend(titlebar);
	};
	if (document.body) {
		install();
		return;
	}
	document.addEventListener('DOMContentLoaded', install, {once: true});
}

function applyStartupDesktopWindowClasses(config: BridgeConfig): void {
	withDocumentElement((root) => {
		root.classList.remove(
			'platform-web',
			'platform-native',
			'platform-macos',
			'platform-windows',
			'platform-linux',
			'platform-unknown',
		);
		root.classList.add('platform-native', getStartupPlatformClass(config.platform));
		root.classList.toggle('allow-transparency', config.activeAllowTransparency);
		root.classList.toggle('native-system-titlebar', config.activeUseNativeTitleBar);
		root.style.setProperty(
			'--native-titlebar-height',
			config.activeUseNativeTitleBar ? NATIVE_TITLEBAR_HEIGHT : CUSTOM_TITLEBAR_HEIGHT,
		);
	});
	installStartupNativeTitlebar(config);
}

function applyStartupAccessibilitySettings(): void {
	const settings = readAccessibilityStartupSettings();
	if (settings === null) {
		return;
	}
	send('set_zoom_factor', {factor: 1});
	applyReducedMotionClass(resolveReducedMotion(settings));
}

function reportPrebootTheme(): void {
	let theme: string | null = null;
	try {
		theme = window.localStorage.getItem(THEME_STORAGE_KEY);
	} catch {
		return;
	}
	if (typeof theme === 'string' && theme.length > 0) {
		send('desktop_preboot_theme_report', {theme});
	}
}

/** Runs once per document, before page scripts, in the order the Electron preload used. */
export function runDocumentStartup(config: BridgeConfig): void {
	applyStartupDesktopWindowClasses(config);

	// The preload seeded legacy raw local storage harvested from the Electron web origin here;
	// this shell has no such origin to harvest from.

	reportPrebootTheme();
	window.addEventListener('pagehide', () => reportPrebootTheme());
	document.addEventListener('visibilitychange', () => {
		if (document.visibilityState === 'hidden') {
			reportPrebootTheme();
		}
	});

	applyStartupAccessibilitySettings();
}
