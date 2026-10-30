// SPDX-License-Identifier: AGPL-3.0-or-later

// The `desktopRuntimeConfig` API of fluxer_desktop/src/preload/LocalAppPreload.ts, served by
// src-tauri/src/runtime/commands.rs.

import type {
	DesktopRuntimeAbort,
	DesktopRuntimeAbortRequest,
	DesktopRuntimeCancelRequest,
	DesktopRuntimeCommit,
	DesktopRuntimeCommittedTransactionRequest,
	DesktopRuntimeConfigAPI,
	DesktopRuntimeDeactivateRequest,
	DesktopRuntimeDeactivation,
	DesktopRuntimeFinalization,
	DesktopRuntimePlan,
	DesktopRuntimePreparation,
	DesktopRuntimePrepareRequest,
	DesktopRuntimeResolveRequest,
	DesktopRuntimeRollback,
	DesktopRuntimeTransactionRequest,
} from '@fluxer/desktop_ipc/src/LocalAppRuntimeContract';
import {invokeOwned} from './ipc';

export const desktopRuntimeConfig: DesktopRuntimeConfigAPI = Object.freeze<DesktopRuntimeConfigAPI>({
	initialInput: () => invokeOwned<string | null>('desktop_runtime_config_initial_input'),
	resolve: (request: DesktopRuntimeResolveRequest) =>
		invokeOwned<DesktopRuntimePlan>('desktop_runtime_config_resolve', {request}),
	cancelResolution: async (request: DesktopRuntimeCancelRequest) => {
		// Tauri resolves a unit result with `null`; Electron's void handler resolved `undefined`.
		await invokeOwned<null>('desktop_runtime_config_cancel_resolution', {request});
	},
	prepare: (request: DesktopRuntimePrepareRequest) =>
		invokeOwned<DesktopRuntimePreparation>('desktop_runtime_config_prepare', {request}),
	commit: (request: DesktopRuntimeTransactionRequest) =>
		invokeOwned<DesktopRuntimeCommit>('desktop_runtime_config_commit', {request}),
	abort: (request: DesktopRuntimeAbortRequest) =>
		invokeOwned<DesktopRuntimeAbort>('desktop_runtime_config_abort', {request}),
	finalize: (request: DesktopRuntimeCommittedTransactionRequest) =>
		invokeOwned<DesktopRuntimeFinalization>('desktop_runtime_config_finalize', {request}),
	rollback: (request: DesktopRuntimeCommittedTransactionRequest) =>
		invokeOwned<DesktopRuntimeRollback>('desktop_runtime_config_rollback', {request}),
	deactivate: (request: DesktopRuntimeDeactivateRequest) =>
		invokeOwned<DesktopRuntimeDeactivation>('desktop_runtime_config_deactivate', {request}),
});
