// Licensed under the Apache-2.0 license

//! Caliptra API-lite error codes within [`mcu_error::domain::CALIPTRA_API`].
//!
//! Subdomain and code allocations are owned by this crate.

use mcu_error::{domain, McuErrorCode};

/// Image-loading errors under [`domain::CALIPTRA_API`].
pub const SUBDOMAIN_IMAGE_LOADING: u8 = 0x01;

pub mod image_loader {
    use super::{domain, McuErrorCode, SUBDOMAIN_IMAGE_LOADING};

    const fn code(code: u16) -> McuErrorCode {
        McuErrorCode::new(domain::CALIPTRA_API, SUBDOMAIN_IMAGE_LOADING, code)
    }

    /// The caller supplied more firmware IDs than the Caliptra command supports.
    pub const FW_ID_COUNT_TOO_LARGE: McuErrorCode = code(0x0001);
    /// Building an image-loading mailbox request failed.
    pub const REQUEST_BUILD_FAILED: McuErrorCode = code(0x0002);
    /// GET_IMAGE_INFO failed in Caliptra mailbox execution.
    pub const GET_IMAGE_INFO_FAILED: McuErrorCode = code(0x0003);
    /// GET_IMAGE_INFO returned fewer bytes than the fixed response layout requires.
    pub const IMAGE_INFO_RESPONSE_TOO_SHORT: McuErrorCode = code(0x0004);
    /// A mailbox command response did not contain the common response header.
    pub const MAILBOX_RESPONSE_TOO_SHORT: McuErrorCode = code(0x0005);
    /// The Auth Manifest described by flash metadata is larger than supported.
    pub const AUTH_MANIFEST_TOO_LARGE: McuErrorCode = code(0x0006);
    /// VERIFY_AUTH_MANIFEST failed in Caliptra mailbox execution.
    pub const AUTH_MANIFEST_VERIFICATION_FAILED: McuErrorCode = code(0x0007);
    /// ACTIVATE_FIRMWARE failed in Caliptra mailbox execution.
    pub const FIRMWARE_ACTIVATION_FAILED: McuErrorCode = code(0x0008);
    /// The flash image header could not be parsed.
    pub const INVALID_FLASH_HEADER: McuErrorCode = code(0x0009);
    /// A flash image TOC entry could not be parsed.
    pub const INVALID_IMAGE_HEADER: McuErrorCode = code(0x000a);
    /// The requested image ID was not found in the flash or PLDM image TOC.
    pub const IMAGE_NOT_FOUND: McuErrorCode = code(0x000b);
    /// Image offset or address arithmetic overflowed.
    pub const IMAGE_OFFSET_OVERFLOW: McuErrorCode = code(0x000c);
    /// The DMA transfer implementation reported a zero maximum transfer size.
    pub const DMA_TRANSFER_SIZE_ZERO: McuErrorCode = code(0x000d);
    /// PLDM image loading observed an unexpected state transition.
    pub const PLDM_UNEXPECTED_STATE: McuErrorCode = code(0x000e);
    /// The flash header advertised more images than the PLDM image-loader supports.
    pub const PLDM_IMAGE_COUNT_TOO_LARGE: McuErrorCode = code(0x000f);
    /// PLDM streaming boot was started without firmware-device descriptors.
    pub const PLDM_DESCRIPTORS_EMPTY: McuErrorCode = code(0x0010);
    /// Spawning the PLDM service task failed.
    pub const PLDM_TASK_SPAWN_FAILED: McuErrorCode = code(0x0011);
    /// The PLDM service failed to reach the initialized state.
    pub const PLDM_SERVICE_START_FAILED: McuErrorCode = code(0x0012);
    /// Reading streamed Auth Manifest bytes from flash failed.
    pub const AUTH_MANIFEST_STREAM_FAILED: McuErrorCode = code(0x0013);
}
