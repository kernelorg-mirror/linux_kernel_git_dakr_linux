// SPDX-License-Identifier: GPL-2.0

//! Nova-core auxbus data. Contains all the methods used by the auxbus drivers
//! to interact with nova-core.

use core::pin::Pin;

use kernel::{
    auxiliary,
    device::Bound,
    pci,
    prelude::*,
    types::ForLt, //
};

use crate::gpu::{
    Gpu, //
};

/// API handle for the auxiliary bus child drivers to interact with nova-core.
pub struct NovaCoreApi<'bound> {
    pub(crate) gpu: Pin<&'bound Gpu<'bound>>,
    pub(crate) pdev: &'bound pci::Device<Bound>,
}

impl NovaCoreApi<'_> {
    /// Returns the NUL-terminated full GPU name supplied by GSP-RM.
    pub fn gpu_name(&self) -> [u8; 64] {
        *self.gpu.gsp_static_info.gpu_name_bytes()
    }

    /// Returns the NUL-terminated short GPU name supplied by GSP-RM.
    pub fn gpu_short_name(&self) -> [u8; 64] {
        *self.gpu.gsp_static_info.gpu_short_name_bytes()
    }

    /// Returns the 16-byte SHA-1 GPU identifier supplied by GSP-RM.
    pub fn gpu_gid(&self) -> [u8; 16] {
        *self.gpu.gsp_static_info.gpu_gid()
    }

    /// Obtain a [`NovaCoreApiHandle`] from an auxiliary device registered by nova-core.
    pub fn handle(adev: &auxiliary::Device<Bound>) -> Result<NovaCoreApiHandle<'_>> {
        NovaCoreApiHandle::of(adev)
    }

    /// Returns the architecture identifier of this GPU.
    pub fn architecture(&self) -> u32 {
        self.gpu.spec.chipset.arch() as u32
    }

    /// Returns the implementation identifier of this GPU.
    pub fn implementation(&self) -> u32 {
        self.gpu.spec.chipset.implementation()
    }

    /// Returns the size of the PCIe BAR used for accessing VRAM, typically
    /// BAR1.
    pub fn bar1_size(&self) -> Result<u64> {
        self.pdev.resource_len(1)
    }

    /// Returns the total usable VRAM size of this GPU in bytes.
    pub fn vram_size(&self) -> u64 {
        self.gpu.gsp_static_info.vram_size()
    }
}

/// Closure-based API handle for invariant registration data types.
pub struct NovaCoreApiHandle<'a> {
    adev: &'a auxiliary::Device<Bound>,
}

impl<'a> NovaCoreApiHandle<'a> {
    fn of(adev: &'a auxiliary::Device<Bound>) -> Result<Self> {
        adev.registration_data_with::<ForLt!(NovaCoreApi<'_>), ()>(|_| ())?;
        Ok(Self { adev })
    }

    /// Access the [`NovaCoreApi`] through a closure.
    pub fn with<R>(&self, f: impl for<'b> FnOnce(Pin<&'b NovaCoreApi<'b>>) -> R) -> R {
        self.adev
            .registration_data_with::<ForLt!(NovaCoreApi<'_>), R>(f)
            .expect("TypeId was validated in NovaCoreApiHandle::of()")
    }
}
