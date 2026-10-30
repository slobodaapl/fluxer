// SPDX-License-Identifier: AGPL-3.0-or-later

import type {
	DesktopInfo,
	DesktopWindowBehaviorSettings,
	ElectronAPI,
	TrayActionPayload,
	UpdaterEvent,
} from '@app/types/electron';
import type {
	AppMetricsSnapshot,
	DesktopTroubleshootingSettings,
	GpuInfo,
	LinuxAppearanceSnapshot,
	StreamingPriorityDiagnostics,
	ThemeDirectoryCssFile,
	ThemeLinkedFileChange,
	ThemeLocalFileReadResult,
	ThemeLocalFileReference,
	VoiceBackgroundMediaReadResult,
} from '@fluxer/desktop_ipc/src/DesktopBridgeTypes';
import {takeBridgeConfig} from './config';
import {installTitlebarDrag} from './drag';
import {createFirstContentPaintSignal} from './firstContentPaint';
import {createNativeGatewayTransport} from './gateway';
import {attachDocument, dispatch, documentToken, invoke, invokeOwned, send, subscribe} from './ipc';
import {installProtocolAuthorization} from './protocol';
import {desktopRuntimeConfig} from './runtime';
import {spellcheck, startSpellcheck} from './spellcheck';
import {installMenuAccelerators} from './shortcuts';
import {runDocumentStartup} from './startup';
import {desktopAccounts, desktopKnownInstances, desktopStorage} from './storage';
import {unavailable, unavailableAction} from './unavailable';
import {localAppUpload} from './upload';
import {cacheVoiceBackgroundMedia} from './voiceBackground';

// Frames get nothing: the bridge serves the top-level renderer document only.
if (window.top === window.self) {
	installBridge();
}

function installBridge(): void {
	const config = takeBridgeConfig();

	installProtocolAuthorization(documentToken);
	runDocumentStartup(config);
	if (!config.activeUseNativeTitleBar) {
		installTitlebarDrag(config.platform);
	}
	startSpellcheck();
	if (config.platform !== 'darwin') {
		installMenuAccelerators(config.platform);
	}

	const api: ElectronAPI = {
		platform: config.platform,
		buildChannel: config.buildChannel,
		localDevelopmentInstanceUrl: config.localDevelopmentInstanceUrl,
		offlineBuild: config.offlineBuild,
		capabilities: config.capabilities,
		localApp: Object.freeze({origin: config.localAppOrigin}),
		desktopAccounts,
		desktopStorage,
		desktopKnownInstances,
		desktopRuntimeConfig,
		nativeGatewayTransport: createNativeGatewayTransport(config),
		localAppUpload,
		...spellcheck,

		openExternal: (url) => invoke('open_external', {url}),
		getDesktopInfo: () => invoke<DesktopInfo>('get_desktop_info'),
		setZoomFactor: (factor) => send('set_zoom_factor', {factor}),
		notifyFirstContentPainted: createFirstContentPaintSignal(),

		windowMinimize: () => send('window_minimize'),
		windowMaximize: () => send('window_toggle_maximize'),
		windowClose: () => send('window_close'),
		windowIsMaximized: () => invoke<boolean>('window_is_maximized'),
		onWindowMaximizeChange: (callback) => subscribe('window-maximize-change', callback),
		onWindowLiveResizeChange: (callback) => subscribe('window-live-resize-change', callback),
		flashFrame: (persistent) => send('window_flash', {persistent: persistent === true}),
		stopFlashFrame: () => send('window_stop_flash'),
		setTaskbarProgress: (fraction, mode) =>
			send('taskbar_progress_set', {payload: {fraction, mode: mode ?? 'normal'}}),
		popoutSetAlwaysOnTop: (key, flag) => invoke<boolean>('popout_set_always_on_top', {key, flag}),
		popoutFocus: (key) => invoke<boolean>('popout_focus', {key}),
		cssEditorWindow: true,
		openVoiceDebugEventSinkPopout: async (entries) => {
			await invoke<null>('voice_debug_event_sink_open', {entries});
		},
		appendVoiceDebugEventSinkEntries: (entries) => send('voice_debug_event_sink_append', {entries}),
		setVoiceDebugEventSinkStatsHtml: (html) => send('voice_debug_event_sink_set_stats_html', {html}),

		setTrayRuntimeState: (state) => send('tray_runtime_state_update', {state, documentToken}),
		onTrayAction: (callback) => subscribe<TrayActionPayload>('tray-action', callback),
		onOpenSettings: (callback) => subscribe('open-settings', () => callback()),
		onZoomIn: (callback) => subscribe('zoom-in', () => callback()),
		onZoomOut: (callback) => subscribe('zoom-out', () => callback()),
		onZoomReset: (callback) => subscribe('zoom-reset', () => callback()),
		onJumpListNewDm: (callback) => subscribe('jump-list-new-dm', () => callback()),
		// Only the Windows jump list and the macOS dock menu follow it.
		...(config.platform === 'linux'
			? {}
			: {
					setDirectMessagesAvailable: (available: boolean) =>
						send('direct_messages_availability_set', {available, documentToken}),
				}),
		setBadgeCount: (count) => send('set_badge_count', {count, documentToken}),
		setUnreadBadge: (badge) => send('set_unread_badge', {badge, documentToken}),
		setNativeLocale: (locale, strings) => send('native_locale_set', {payload: {locale, strings}}),
		acquireStreamingPriority: () => send('streaming_priority_acquire'),
		releaseStreamingPriority: () => send('streaming_priority_release'),
		getStreamingPriorityDiagnostics: () =>
			invoke<StreamingPriorityDiagnostics>('streaming_priority_get_diagnostics'),

		getDesktopWindowBehaviorSettings: () => invoke<DesktopWindowBehaviorSettings>('desktop_window_behavior_get'),
		setDesktopWindowBehaviorSettings: (settings) =>
			invoke<DesktopWindowBehaviorSettings>('desktop_window_behavior_set', {settings}),
		getDesktopWindowBehaviorPendingRestart: () => invoke<boolean>('desktop_window_behavior_pending_restart'),
		desktopAppRelaunch: async () => {
			await invoke<null>('desktop_app_relaunch');
		},
		getDesktopTroubleshootingSettings: () => invoke<DesktopTroubleshootingSettings>('desktop_troubleshooting_get'),
		setDesktopDisableHardwareAcceleration: (options) =>
			invoke<DesktopTroubleshootingSettings>('desktop_troubleshooting_set_disable_hardware_acceleration', {options}),
		setDesktopDisableModuleUpdates: (options) =>
			invoke<DesktopTroubleshootingSettings>('desktop_troubleshooting_set_disable_module_updates', {options}),
		setPrebootTheme: (theme) => send('desktop_preboot_theme_report', {theme}),
		reportInstanceProductName: (instanceKey, productName) =>
			send('desktop_instance_product_name_report', {report: {instanceKey, productName}}),
		reportLastRoute: (routePath) => send('desktop_last_route_report', {routePath}),

		onUpdaterEvent: (callback) => subscribe('updater-event', callback),
		updaterCheck: async (context) => {
			const event: UpdaterEvent = {type: 'unsupported', context, reason: 'unpackaged'};
			dispatch('updater-event', event);
		},
		onDeepLink: (callback) => subscribe('deep-link', callback),
		getInitialDeepLink: () => invoke<string | null>('get_initial_deep_link'),
		onTextareaContextMenu: (callback) => subscribe('textarea-context-menu', callback),
		showNotification: (payload) => invokeOwned<{id: string}>('show_notification', {payload}),
		shouldPlayNotificationSound: () => invoke<boolean>('notification_sound_allowed'),
		closeNotification: (id) => send('close_notification', {id}),
		closeNotifications: (ids) => send('close_notifications', {ids}),
		onNotificationClick: (callback) =>
			subscribe<[string, string | null, string | null]>('notification-click', ([id, url, accountKey]) =>
				callback(id, url, accountKey),
			),
		onSpellcheckStateChanged: (callback) => subscribe('spellcheck-state-changed', callback),
		onGlobalKeyEvent: (callback) => subscribe('global-key-event', callback),
		onGlobalMouseEvent: (callback) => subscribe('global-mouse-event', callback),
		getSystemIdleTimeMs: () => invoke<number>('system_idle_time_ms'),
		getGpuInfo: () => invoke<GpuInfo>('get_gpu_info'),
		getAppMetrics: () => invoke<AppMetricsSnapshot>('get_app_metrics'),
		// Electron answered off Linux with a fixed no-preference snapshot; the renderer reads it
		// on Linux only, so other platforms leave the pair out.
		...(config.platform === 'linux'
			? {
					getLinuxAppearance: () => invoke<LinuxAppearanceSnapshot>('linux_appearance_get'),
					onLinuxAppearanceChanged: (callback: (snapshot: LinuxAppearanceSnapshot) => void) =>
						subscribe('linux-appearance-changed', callback),
				}
			: {}),

		downloadFile: (url, defaultPath, sha256) => invokeOwned('download_file', {url, defaultPath, sha256}),
		getDesktopSources: unavailable('getDesktopSources'),
		selectDisplayMediaSource: unavailableAction('selectDisplayMediaSource'),
		autostartEnable: async () => {
			await invoke<null>('autostart_enable');
		},
		autostartDisable: async () => {
			await invoke<null>('autostart_disable');
		},
		autostartIsEnabled: () => invoke<boolean>('autostart_is_enabled'),
		autostartIsInitialized: () => invoke<boolean>('autostart_is_initialized'),
		autostartMarkInitialized: async () => {
			await invoke<null>('autostart_mark_initialized');
		},
		globalKeyHookStart: unavailable('globalKeyHookStart'),
		globalKeyHookStop: unavailable('globalKeyHookStop'),
		checkInputMonitoringAccess: unavailable('checkInputMonitoringAccess'),
		checkMediaAccess: unavailable('checkMediaAccess'),
		requestMediaAccess: unavailable('requestMediaAccess'),
		openInputMonitoringSettings: unavailable('openInputMonitoringSettings'),
		openMediaAccessSettings: unavailable('openMediaAccessSettings'),

		pickThemeLocalFiles: () => invokeOwned<Array<ThemeLocalFileReference>>('theme_local_files_pick'),
		readThemeLocalFiles: (paths) => invokeOwned<Array<ThemeLocalFileReadResult>>('theme_local_files_read', {paths}),
		clearThemeLocalFiles: () => invokeOwned<void>('theme_local_files_clear'),
		importThemeDirectory: () => invokeOwned<Array<ThemeDirectoryCssFile>>('theme_directory_import'),
		pickThemeLinkedFiles: (options) =>
			invokeOwned<Array<ThemeDirectoryCssFile>>('theme_linked_files_pick', {options}),
		watchThemeLinkedFiles: (paths) => invokeOwned<void>('theme_linked_files_watch', {paths}),
		onThemeLinkedFileChange: (callback) => subscribe<ThemeLinkedFileChange>('theme-linked-file-changed', callback),
		cacheVoiceBackgroundMedia,
		readVoiceBackgroundMedia: (id) =>
			invokeOwned<VoiceBackgroundMediaReadResult | null>('voice_background_media_cache_read', {id}),
		deleteVoiceBackgroundMedia: (id) => invokeOwned<void>('voice_background_media_cache_delete', {id}),
	};

	attachDocument();

	Object.defineProperty(window, 'electron', {
		value: Object.freeze(api),
		enumerable: true,
		configurable: false,
		writable: false,
	});
}
