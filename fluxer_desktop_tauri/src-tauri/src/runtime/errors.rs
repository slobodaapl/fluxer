// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bridge errors for failures of the outbound HTTP layer that reach the renderer.

use crate::error::BridgeError;
use crate::net::outbound::OutboundError;

/// The DesktopOutboundHTTP.ts error class each failure mirrors. Resolver and connect failures
/// were Node errno errors there, whose name is `Error`.
fn outbound_error_name(error: &OutboundError) -> &'static str {
    match error {
        OutboundError::Closed => "DesktopOutboundHTTPClosedError",
        OutboundError::InvalidOrigin => "DesktopOutboundHTTPInvalidOriginError",
        OutboundError::InvalidTarget => "DesktopOutboundHTTPInvalidTargetError",
        OutboundError::OriginNotRegistered(_) => "DesktopOutboundHTTPOriginNotRegisteredError",
        OutboundError::PublicAddressRequired => "DesktopOutboundHTTPPublicAddressRequiredError",
        OutboundError::InsecureTransport => "DesktopOutboundHTTPInsecureTransportError",
        OutboundError::BodyLimit => "DesktopOutboundHTTPRequestBodyLimitError",
        OutboundError::Timeout => "DesktopOutboundHTTPRequestTimeoutError",
        OutboundError::Aborted => "DesktopOutboundHTTPRequestAbortedError",
        OutboundError::ResolutionTimeout => "DesktopOutboundHTTPResolutionTimeoutError",
        OutboundError::EmptyResolution => "DesktopOutboundHTTPEmptyResolutionError",
        OutboundError::Capacity => "DesktopOutboundHTTPCapacityError",
        OutboundError::Transport(_) => "DesktopOutboundHTTPTransportError",
        OutboundError::Resolution(_) | OutboundError::Connect(_) => "Error",
    }
}

pub fn outbound_error(error: &OutboundError) -> BridgeError {
    BridgeError::new(outbound_error_name(error), error.to_string())
}

/// `isDesktopHostResolutionFailure`: getaddrinfo failed or timed out.
pub fn is_host_resolution_failure(error: &OutboundError) -> bool {
    matches!(
        error,
        OutboundError::Resolution(_) | OutboundError::ResolutionTimeout
    )
}
