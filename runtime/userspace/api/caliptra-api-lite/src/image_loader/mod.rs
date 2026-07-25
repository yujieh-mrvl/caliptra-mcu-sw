// Licensed under the Apache-2.0 license

//! Image loading via Caliptra mailbox — scratch-buffer backed.
//!
//! This module provides the [`ImageLoader`] trait and concrete implementations
//! for flash-based and PLDM-streamed boot flows. All large buffers are
//! allocated through [`ApiAlloc`] rather than living on the stack, keeping
//! async task futures small.

extern crate alloc;
use alloc::boxed::Box;
use async_trait::async_trait;

mod flash_client;
pub(crate) mod pldm_client;
pub(crate) mod pldm_context;
pub(crate) mod pldm_fdops;

use caliptra_mcu_flash_image::{FlashHeader, SOC_MANIFEST_IDENTIFIER};
use caliptra_mcu_libsyscall_caliptra::dma::{AXIAddr, DMAMapping};
use caliptra_mcu_libsyscall_caliptra::flash::SpiFlash as FlashSyscall;
use caliptra_mcu_libsyscall_caliptra::mailbox::PayloadStream;
use caliptra_mcu_libtock_platform::ErrorCode;

use caliptra_mcu_pldm_common::message::firmware_update::get_fw_params::FirmwareParameters;
use caliptra_mcu_pldm_common::message::firmware_update::verify_complete::VerifyResult;
use caliptra_mcu_pldm_common::protocol::firmware_update::Descriptor;
use embassy_executor::Spawner;
use mcu_error::McuResult;

use crate::errors::image_loader as image_errors;
use crate::wire::{
    calc_checksum, mbox_execute, mbox_execute_with_payload_stream, CMD_ACTIVATE_FIRMWARE,
    CMD_GET_IMAGE_INFO, CMD_VERIFY_AUTH_MANIFEST, MBOX_RESP_HEADER_SIZE,
};

pub const IMAGE_MEASUREMENT_DIGEST_SIZE: usize = 48;

/// Maximum number of firmware IDs in a single ACTIVATE_FIRMWARE command.
const MAX_FW_ID_COUNT: usize = 128;

/// Size of the GET_IMAGE_INFO request: chksum(4) + fw_id(4).
const GET_IMAGE_INFO_REQ_SIZE: usize = 8;

/// Size of the GET_IMAGE_INFO response:
/// hdr(8) + component_id(4) + flags(4) + load_addr_high(4) + load_addr_low(4)
/// + staging_addr_high(4) + staging_addr_low(4) + digest(48) = 80
const GET_IMAGE_INFO_RSP_SIZE: usize = 80;

/// Offsets into the GET_IMAGE_INFO response (after the 8-byte mailbox header).
const IMG_INFO_COMPONENT_ID_OFF: usize = MBOX_RESP_HEADER_SIZE;
const IMG_INFO_LOAD_ADDR_HIGH_OFF: usize = MBOX_RESP_HEADER_SIZE + 8;
const IMG_INFO_LOAD_ADDR_LOW_OFF: usize = MBOX_RESP_HEADER_SIZE + 12;
const IMG_INFO_DIGEST_OFF: usize = MBOX_RESP_HEADER_SIZE + 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadedImage {
    pub image_size: u32,
    pub measurement: [u8; IMAGE_MEASUREMENT_DIGEST_SIZE],
}

/// Generic trait for performing source-to-destination DMA transfers.
///
/// This trait abstracts the transfer of data from a source (identified
/// by offset) to an AXI destination address. Implementations can use
/// any transfer mechanism: direct DMA from a peripheral, buffered
/// copy through SRAM, memory-to-memory DMA, etc.
pub trait DmaTransfer: DMAMapping {
    /// The maximum number of bytes that can be transferred in a single
    /// operation. The caller will chunk transfers to this size.
    fn max_transfer_size(&self) -> usize;

    /// Transfer `length` bytes starting at `src_offset` in the source
    /// directly to `dest_addr` on the AXI bus.
    fn transfer(
        &self,
        src_offset: usize,
        dest_addr: AXIAddr,
        length: usize,
    ) -> impl core::future::Future<Output = Result<(), ErrorCode>>;
}

pub trait ImageLoader {
    /// Loads the specified image to storage mapped to the AXI bus memory map.
    ///
    /// # Parameters
    /// image_id: The unsigned integer identifier of the image.
    ///
    /// # Returns
    /// - `Ok(LoadedImage)`: Image has been loaded and metadata preserved.
    /// - `Err`: Indication of the failure to load the image.
    fn load(&self, image_id: u32) -> impl core::future::Future<Output = McuResult<LoadedImage>>;
}

pub struct FlashImageLoader<'a, T: DmaTransfer> {
    flash: FlashSyscall,
    dma_transfer: &'a T,
}

pub struct PldmImageLoader<'a, D: DMAMapping + 'static> {
    spawner: Spawner,
    params: &'a PldmFirmwareDeviceParams,
    dma_mapping: &'static D,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PldmFirmwareDeviceParams {
    pub descriptors: &'static [Descriptor],
    pub fw_params: &'static FirmwareParameters,
}

impl<'a, T: DmaTransfer> FlashImageLoader<'a, T> {
    pub fn new(flash_syscall: FlashSyscall, dma_transfer: &'a T) -> Self {
        Self {
            flash: flash_syscall,
            dma_transfer,
        }
    }
}

impl<T: DmaTransfer> ImageLoader for FlashImageLoader<'_, T> {
    async fn load(&self, image_id: u32) -> McuResult<LoadedImage> {
        let image_info = get_image_info(image_id).await?;
        let load_address = convert_dma_cptra_addr_to_mcu_addr(
            self.dma_transfer,
            ((image_info.load_address_high as u64) << 32) | (image_info.load_address_low as u64),
        )?;
        let mut header = [0u8; core::mem::size_of::<FlashHeader>()];
        flash_client::flash_read_header(&self.flash, &mut header).await?;
        let (offset, size) =
            flash_client::flash_read_toc(&self.flash, &header, image_info.component_id).await?;
        flash_client::flash_load_image(
            self.dma_transfer,
            load_address,
            offset as usize,
            size as usize,
        )
        .await?;
        Ok(LoadedImage {
            image_size: size,
            measurement: image_info.digest,
        })
    }
}

impl<T: DmaTransfer> FlashImageLoader<'_, T> {
    pub async fn set_auth_manifest(&self) -> McuResult<()> {
        let mut header = [0u8; core::mem::size_of::<FlashHeader>()];
        flash_client::flash_read_header(&self.flash, &mut header).await?;
        let (offset, size) =
            flash_client::flash_read_toc(&self.flash, &header, SOC_MANIFEST_IDENTIFIER).await?;

        let mut stream =
            FlashMailboxPayloadStream::new(&self.flash, offset as usize, size as usize);

        // Build the request header: chksum(4) + manifest_size(4)
        let mut req_header = [0u8; 8];
        req_header[4..8].copy_from_slice(&size.to_le_bytes());

        // Calculate the mailbox checksum over cmd + header + payload
        let mut checksum = stream.get_bytesum().await;
        for b in CMD_VERIFY_AUTH_MANIFEST.to_le_bytes().iter() {
            checksum = checksum.wrapping_add(u32::from(*b));
        }
        for b in req_header.iter() {
            checksum = checksum.wrapping_add(u32::from(*b));
        }
        req_header[..4].copy_from_slice(&0u32.wrapping_sub(checksum).to_le_bytes());

        let mut response_buffer = [0u8; MBOX_RESP_HEADER_SIZE];
        loop {
            let result = mbox_execute_with_payload_stream(
                CMD_VERIFY_AUTH_MANIFEST,
                Some(&req_header),
                &mut stream,
                &mut response_buffer,
            )
            .await;
            match result {
                Ok(_) => return Ok(()),
                Err(e) if e == mcu_error::codes::MAILBOX_BUSY => continue,
                Err(_) => return Err(image_errors::AUTH_MANIFEST_VERIFICATION_FAILED),
            }
        }
    }
}

impl<'a, D: DMAMapping + 'static> PldmImageLoader<'a, D> {
    pub fn new(
        params: &'a PldmFirmwareDeviceParams,
        spawner: Spawner,
        dma_mapping: &'static D,
    ) -> Self {
        Self {
            spawner,
            params,
            dma_mapping,
        }
    }

    pub fn finalize(&self, verify_result: VerifyResult) -> McuResult<()> {
        pldm_client::finalize(verify_result)
    }

    /// Wait for the PLDM service to fully stop (protocol complete, tasks exited).
    pub async fn wait_for_service_stopped(&self) {
        pldm_client::wait_service_stopped().await;
    }
}

impl<D: DMAMapping + 'static> ImageLoader for PldmImageLoader<'_, D> {
    async fn load(&self, image_id: u32) -> McuResult<LoadedImage> {
        let image_info = get_image_info(image_id).await?;
        let load_address = convert_dma_cptra_addr_to_mcu_addr(
            self.dma_mapping,
            ((image_info.load_address_high as u64) << 32) | (image_info.load_address_low as u64),
        )?;

        pldm_client::initialize_pldm(
            self.spawner,
            self.params.descriptors,
            self.params.fw_params,
            self.dma_mapping,
        )
        .await?;
        let (offset, size) = pldm_client::pldm_download_toc(image_info.component_id).await?;
        pldm_client::pldm_download_image(load_address, offset, size).await?;
        Ok(LoadedImage {
            image_size: size,
            measurement: image_info.digest,
        })
    }
}

fn convert_dma_cptra_addr_to_mcu_addr(
    dma_mapping: &impl DMAMapping,
    caliptra_axi_addr: u64,
) -> McuResult<AXIAddr> {
    dma_mapping
        .cptra_axi_to_mcu_axi(caliptra_axi_addr)
        .map_err(|_| image_errors::IMAGE_OFFSET_OVERFLOW)
}

/// Parsed GET_IMAGE_INFO response fields we need.
struct ImageInfo {
    component_id: u32,
    load_address_high: u32,
    load_address_low: u32,
    digest: [u8; IMAGE_MEASUREMENT_DIGEST_SIZE],
}

async fn get_image_info(image_id: u32) -> McuResult<ImageInfo> {
    let mut req = [0u8; GET_IMAGE_INFO_REQ_SIZE];
    // fw_id at offset 4
    req[4..8].copy_from_slice(&image_id.to_le_bytes());
    // Compute and place checksum
    let checksum = calc_checksum(CMD_GET_IMAGE_INFO, &req[4..]);
    req[..4].copy_from_slice(&checksum.to_le_bytes());

    let mut rsp = [0u8; GET_IMAGE_INFO_RSP_SIZE];

    loop {
        let result = mbox_execute(CMD_GET_IMAGE_INFO, &req, &mut rsp).await;
        match result {
            Ok(_) => break,
            Err(e) if e == mcu_error::codes::MAILBOX_BUSY => continue,
            Err(_) => return Err(image_errors::GET_IMAGE_INFO_FAILED),
        }
    }

    if rsp.len() < GET_IMAGE_INFO_RSP_SIZE {
        return Err(image_errors::IMAGE_INFO_RESPONSE_TOO_SHORT);
    }

    let component_id = u32::from_le_bytes(
        rsp[IMG_INFO_COMPONENT_ID_OFF..IMG_INFO_COMPONENT_ID_OFF + 4]
            .try_into()
            .unwrap(),
    );
    let load_address_high = u32::from_le_bytes(
        rsp[IMG_INFO_LOAD_ADDR_HIGH_OFF..IMG_INFO_LOAD_ADDR_HIGH_OFF + 4]
            .try_into()
            .unwrap(),
    );
    let load_address_low = u32::from_le_bytes(
        rsp[IMG_INFO_LOAD_ADDR_LOW_OFF..IMG_INFO_LOAD_ADDR_LOW_OFF + 4]
            .try_into()
            .unwrap(),
    );

    let mut digest = [0u8; IMAGE_MEASUREMENT_DIGEST_SIZE];
    digest.copy_from_slice(
        &rsp[IMG_INFO_DIGEST_OFF..IMG_INFO_DIGEST_OFF + IMAGE_MEASUREMENT_DIGEST_SIZE],
    );

    Ok(ImageInfo {
        component_id,
        load_address_high,
        load_address_low,
        digest,
    })
}

/// Activate a set of firmware images via Caliptra ACTIVATE_FIRMWARE mailbox command.
pub async fn activate_firmware(fw_id_list: &[u32]) -> McuResult<()> {
    if fw_id_list.len() > MAX_FW_ID_COUNT {
        return Err(image_errors::FW_ID_COUNT_TOO_LARGE);
    }

    // Request layout: chksum(4) + fw_id_count(4) + fw_ids(128*4) + mcu_fw_image_size(4) + flags(4)
    const REQ_SIZE: usize = 4 + 4 + MAX_FW_ID_COUNT * 4 + 4 + 4;
    let mut req = [0u8; REQ_SIZE];
    // fw_id_count at offset 4
    req[4..8].copy_from_slice(&(fw_id_list.len() as u32).to_le_bytes());
    // fw_ids starting at offset 8
    for (i, fw_id) in fw_id_list.iter().enumerate() {
        let off = 8 + i * 4;
        req[off..off + 4].copy_from_slice(&fw_id.to_le_bytes());
    }
    // mcu_fw_image_size = 0 at offset 8 + 128*4 = 520 (already zero)
    // flags = 0 at offset 524 (already zero)

    // Compute checksum
    let checksum = calc_checksum(CMD_ACTIVATE_FIRMWARE, &req[4..]);
    req[..4].copy_from_slice(&checksum.to_le_bytes());

    let mut rsp = [0u8; GET_IMAGE_INFO_RSP_SIZE]; // Response type is GetImageInfoResp per Caliptra API
    loop {
        let result = mbox_execute(CMD_ACTIVATE_FIRMWARE, &req, &mut rsp).await;
        match result {
            Ok(_) => return Ok(()),
            Err(e) if e == mcu_error::codes::MAILBOX_BUSY => continue,
            Err(_) => return Err(image_errors::FIRMWARE_ACTIVATION_FAILED),
        }
    }
}

pub struct FlashMailboxPayloadStream<'a> {
    pub flash: &'a FlashSyscall,
    pub offset: usize,
    pub cursor: usize,
    pub len: usize,
}

impl<'a> FlashMailboxPayloadStream<'a> {
    pub fn new(flash: &'a FlashSyscall, starting_offset: usize, len: usize) -> Self {
        Self {
            flash,
            offset: starting_offset,
            cursor: starting_offset,
            len,
        }
    }
    pub fn reset(&mut self) {
        self.cursor = self.offset;
    }
    pub async fn get_bytesum(&mut self) -> u32 {
        self.reset();
        let mut sum = 0u32;
        let mut buffer = [0u8; 256];
        while let Ok(bytes_read) = self.read(&mut buffer).await {
            if bytes_read == 0 {
                break;
            }
            for byte in &buffer[..bytes_read] {
                sum = sum.wrapping_add(u32::from(*byte));
            }
        }
        self.reset();
        sum
    }
}

#[async_trait(?Send)]
impl PayloadStream for FlashMailboxPayloadStream<'_> {
    fn size(&self) -> usize {
        self.len
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, ErrorCode> {
        if (self.cursor - self.offset) >= self.len {
            return Ok(0);
        }

        let bytes_to_read = (self.len - (self.cursor - self.offset)).min(buffer.len());
        self.flash
            .read(self.cursor, bytes_to_read, &mut buffer[..bytes_to_read])
            .await?;
        self.cursor += bytes_to_read;
        Ok(bytes_to_read)
    }
}
