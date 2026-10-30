// SPDX-License-Identifier: AGPL-3.0-or-later

// The desktop app store APIs of fluxer_desktop/src/preload/DesktopStoragePreload.ts, served by
// src-tauri/src/storage/commands.rs.

import type {
	DesktopAccountCompareAndSwapRequest,
	DesktopAccountImportReport,
	DesktopAccountImportRequest,
	DesktopAccountPruneReport,
	DesktopAccountPruneRequest,
	DesktopAccountRecord,
	DesktopAccountStorageAPI,
} from '@fluxer/desktop_ipc/src/AccountContract';
import type {
	DesktopKnownInstanceRecord,
	DesktopKnownInstanceStorageAPI,
} from '@fluxer/desktop_ipc/src/KnownInstanceContract';
import type {
	DesktopStorageAPI,
	DesktopStorageEntry,
	DesktopStorageImportReport,
	DesktopStorageImportRequest,
	DesktopStorageWrite,
	DesktopStoreStatus,
} from '@fluxer/desktop_ipc/src/StorageContract';
import {invokeOwned} from './ipc';

/** Tauri resolves a unit result with `null`; Electron's void handlers resolved `undefined`. */
async function invokeVoid(command: string, args?: Record<string, unknown>): Promise<void> {
	await invokeOwned<null>(command, args);
}

export const desktopAccounts: DesktopAccountStorageAPI = Object.freeze<DesktopAccountStorageAPI>({
	getAll: () => invokeOwned<Array<DesktopAccountRecord>>('desktop_accounts_get_all'),
	get: (storageKey: string) => invokeOwned<DesktopAccountRecord | null>('desktop_accounts_get', {storageKey}),
	upsert: (account: DesktopAccountRecord) => invokeVoid('desktop_accounts_upsert', {account}),
	compareAndSwap: (request: DesktopAccountCompareAndSwapRequest) =>
		invokeOwned<boolean>('desktop_accounts_compare_and_swap', {request}),
	delete: (storageKey: string) => invokeVoid('desktop_accounts_delete', {storageKey}),
	import: (request: DesktopAccountImportRequest) =>
		invokeOwned<DesktopAccountImportReport>('desktop_accounts_import', {request}),
	prune: (request: DesktopAccountPruneRequest) =>
		invokeOwned<DesktopAccountPruneReport>('desktop_accounts_prune', {request}),
});

export const desktopStorage: DesktopStorageAPI = Object.freeze<DesktopStorageAPI>({
	getStatus: () => invokeOwned<DesktopStoreStatus>('desktop_storage_get_status'),
	load: (scope: string) => invokeOwned<Array<DesktopStorageEntry>>('desktop_storage_load', {scope}),
	get: (scope: string, key: string) => invokeOwned<DesktopStorageEntry | null>('desktop_storage_get', {scope, key}),
	set: (scope: string, key: string, value: string) => invokeVoid('desktop_storage_set', {scope, key, value}),
	delete: (scope: string, key: string) => invokeVoid('desktop_storage_delete', {scope, key}),
	clearAllForScope: (scope: string) => invokeVoid('desktop_storage_clear_all_for_scope', {scope}),
	clearAllExcept: (keysToKeep: ReadonlyArray<string>) =>
		invokeVoid('desktop_storage_clear_all_except', {keysToKeep: [...keysToKeep]}),
	setMany: (writes: ReadonlyArray<DesktopStorageWrite>) =>
		invokeVoid('desktop_storage_set_many', {writes: [...writes]}),
	import: (request: DesktopStorageImportRequest) =>
		invokeOwned<DesktopStorageImportReport>('desktop_storage_import', {
			request: {entries: [...request.entries], marker: request.marker ?? null},
		}),
	getMarker: (key: string) => invokeOwned<string | null>('desktop_storage_get_marker', {key}),
	setMarker: (key: string, value: string) => invokeVoid('desktop_storage_set_marker', {key, value}),
});

export const desktopKnownInstances: DesktopKnownInstanceStorageAPI = Object.freeze<DesktopKnownInstanceStorageAPI>({
	getAll: () => invokeOwned<Array<DesktopKnownInstanceRecord>>('desktop_known_instances_get_all'),
	upsert: (instance: DesktopKnownInstanceRecord) => invokeVoid('desktop_known_instances_upsert', {instance}),
	delete: (instanceKey: string) => invokeVoid('desktop_known_instances_delete', {instanceKey}),
});
