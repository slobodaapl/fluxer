# Device revocation

Native supports revoking one non-current device from Devices. The user confirms
before the first POST, since accounts without verifiable credentials or with a
recent SSO sign-in can pass sudo without a challenge. The worker binds the target
to the account and instance and rechecks the device list before each POST.

On an exact `SUDO_MODE_REQUIRED` response, Native supports password when
advertised without MFA, and TOTP or backup codes using `mfa_method: totp`.
WebAuthn, email codes, and SSO reauthentication are not implemented and fail
closed. Proofs are entered in a masked input, cleared on submission or close,
and not logged. A definite verification rejection can be retried; transport
failures and timeouts require the user to check the device list before trying
again. The list refreshes after confirmed success. Closing an in-flight request
cannot guarantee that the server did not process it.

The direct logout request carries the proof, so Native does not persist or reuse
the sudo token that MFA may return. Current-device and bulk revocation remain
unavailable in this flow.

Source: `fluxer_api/src/api/auth/AuthController.ts`, `AuthSession.ts`,
`services/SudoVerificationService.ts`, `services/SudoMethods.ts`,
`middleware/SudoModeMiddleware.ts`, and classic `AuthSessionCommands.ts` and
`RestTransport.ts`.
