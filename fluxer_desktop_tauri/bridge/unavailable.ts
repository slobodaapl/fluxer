// SPDX-License-Identifier: AGPL-3.0-or-later

/** A member of the Electron bridge contract that this shell has not ported yet. */
export class DesktopBridgeUnavailableError extends Error {
	constructor(member: string) {
		super(`The desktop shell does not implement ${member} yet`);
		this.name = 'DesktopBridgeUnavailableError';
	}
}

export function unavailable(member: string): () => Promise<never> {
	return () => Promise.reject(new DesktopBridgeUnavailableError(member));
}

export function unavailableAction(member: string): () => void {
	return () => {
		console.warn(new DesktopBridgeUnavailableError(member).message);
	};
}
